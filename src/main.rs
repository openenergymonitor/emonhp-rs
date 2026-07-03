#![no_std]
#![no_main]

use core::sync::atomic::Ordering::Relaxed;
use core::sync::atomic::{AtomicBool, AtomicU32};

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::I2c;
use embassy_stm32::mode::Async;
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{BufferedUart, BufferedUartRx, BufferedUartTx};
use embassy_stm32::{bind_interrupts, interrupt};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::Point;
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_io_async::{Read, Write};
use ssd1306::mode::DisplayConfig;
use ssd1306::size::DisplaySize128x64;
use ssd1306::{I2CDisplayInterface, Ssd1306};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

enum UartTxMsg {
    Echo(u8),
    Static(&'static [u8]),
}

#[repr(u32)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum MBusState {
    Disabled = 0,
    Enabled = 1,
    OverCurrentError = 2,
}

impl MBusState {
    fn as_u32(self) -> u32 {
        self as u32
    }

    fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Disabled,
            1 => Self::Enabled,
            2 => Self::OverCurrentError,
            _ => Self::Disabled,
        }
    }
}

static UART_TX_CH: Channel<CriticalSectionRawMutex, UartTxMsg, 2> = Channel::new();
static UART_TX_BUF: StaticCell<[u8; 512]> = StaticCell::new();
static UART_RX_BUF: StaticCell<[u8; 64]> = StaticCell::new();

static MBUS_SIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static MBUSSTATE: AtomicU32 = AtomicU32::new(MBusState::Disabled as u32);

static IRQ_CH: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static BOOT_IND: AtomicBool = AtomicBool::new(true);

bind_interrupts!(
    pub struct Irqs{
        EXTI0_1 => exti::InterruptHandler<interrupt::typelevel::EXTI0_1>;
        EXTI4_15 => exti::InterruptHandler<interrupt::typelevel::EXTI4_15>;

        USART2 => embassy_stm32::usart::BufferedInterruptHandler<embassy_stm32::peripherals::USART2>;
});

fn display_boot(
    i2c: embassy_stm32::Peri<'static, embassy_stm32::peripherals::I2C1>,
    scl: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PB8>,
    sda: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PB9>,
) {
    /* Set image + text on the OLED
       REVISIT : PA9/PA10 for real board
    */
    let mut i2c_cfg = embassy_stm32::i2c::Config::default();
    i2c_cfg.frequency = Hertz(400_000);
    let i2c = I2c::new_blocking(i2c, scl, sda, i2c_cfg);

    let ssd_interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(
        ssd_interface,
        DisplaySize128x64,
        ssd1306::rotation::DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();
    let res = display.init();

    info!("Initialising SSD1306...");
    if let Err(_) = res {
        info!("  - Failed.");
    } else {
        info!("  - Success.");

        let raw: ImageRaw<BinaryColor> = ImageRaw::new(include_bytes!("./rust.raw"), 64);
        let im = Image::new(&raw, Point::new(32, 0));
        im.draw(&mut display).unwrap();

        if let Err(_) = display.flush() {
            info!("Failed to update display");
        }
    }
}

async fn config_handler(cmd: &str) {
    let help_str: &'static str = "====== emonHP ======\r\n\
                          - ?               : Print this message again\r\n\
                          - b               : MBus regulator status\r\n\
                          - b<n>            : set MBus regulator, n = 0 OFF, n = 1 ON\r\n\
                          - i               : read interrupt status\r\n\
                          - i<x>            : clear interrupt index x\r\n\
                          - v               : firmware and board information\r\n";

    let fw_str: &'static str = concat!(
        "\r\n====== emonHP ======\r\n\
          - Firmware    : v",
        env!("CARGO_PKG_VERSION"),
        "\r\n\
          - Board       : emonHP1 (arch. rev. 1)\r\n\r\n\
          - emonHP Copyright (C) 2026 Angus Logan\r\n\
          - Distributed under GPL3 license, see COPYING.md\r\n\
          - For Bear and Moose\r\n"
    );

    match cmd.as_bytes() {
        b"?" => {
            UART_TX_CH
                .send(UartTxMsg::Static(help_str.as_bytes()))
                .await;
        }
        b"b" => {
            UART_TX_CH.send(UartTxMsg::Static(b"> MBus status: ")).await;
            match MBusState::from_u32(MBUSSTATE.load(Relaxed)) {
                MBusState::OverCurrentError => {
                    UART_TX_CH
                        .send(UartTxMsg::Static(b"Over Current Error\r\n"))
                        .await;
                }
                MBusState::Enabled => {
                    UART_TX_CH.send(UartTxMsg::Static(b"Enabled\r\n")).await;
                }
                MBusState::Disabled => {
                    UART_TX_CH.send(UartTxMsg::Static(b"Disabled\r\n")).await;
                }
            }
        }
        b"b0" => {
            let mbs = MBUSSTATE.load(Relaxed);
            if MBusState::from_u32(mbs) == MBusState::Enabled {
                MBUSSTATE.store(MBusState::as_u32(MBusState::Disabled), Relaxed);
                MBUS_SIG.signal(());
            }
        }
        b"b1" => {
            if matches!(
                MBusState::from_u32(MBUSSTATE.load(Relaxed)),
                MBusState::Disabled | MBusState::OverCurrentError
            ) {
                MBUSSTATE.store(MBusState::as_u32(MBusState::Enabled), Relaxed);
            }
            MBUS_SIG.signal(());
        }
        b"i" => {
            // Very basic handler [4] is MBus overcurrent, [0] is BOOT
            if MBusState::from_u32(MBUSSTATE.load(Relaxed)) == MBusState::OverCurrentError {
                UART_TX_CH.send(UartTxMsg::Echo(b'1')).await;
            } else {
                UART_TX_CH.send(UartTxMsg::Echo(b'0')).await;
            }

            if BOOT_IND.load(Relaxed) {
                UART_TX_CH.send(UartTxMsg::Echo(b'1')).await;
            } else {
                UART_TX_CH.send(UartTxMsg::Echo(b'0')).await;
            }
            UART_TX_CH.send(UartTxMsg::Static(b"\r\n")).await;
        }
        b"i0" => {
            BOOT_IND.store(false, Relaxed);
            IRQ_CH.signal(());
        }
        b"i4" => {
            MBUSSTATE.store(MBusState::as_u32(MBusState::Disabled), Relaxed);
            IRQ_CH.signal(());
        }
        b"v" => {
            UART_TX_CH.send(UartTxMsg::Static(fw_str.as_bytes())).await;
        }
        _ => {
            UART_TX_CH
                .send(UartTxMsg::Static(b"> Unknown command.\r\n"))
                .await
        }
    }
}

#[embassy_executor::task]
async fn uart_tx_task(mut uart_tx: BufferedUartTx<'static>) {
    loop {
        let res = match UART_TX_CH.receive().await {
            UartTxMsg::Echo(byte) => uart_tx.write_all(&[byte]).await,
            UartTxMsg::Static(bytes) => uart_tx.write_all(bytes).await,
        };

        if res.is_ok() {
            let _ = uart_tx.flush().await;
        }
    }
}

#[embassy_executor::task]
async fn uart_rx_task(mut uart_rx: BufferedUartRx<'static>) {
    let mut ln = heapless::String::<64>::new();
    let mut byte = [0u8, 1];
    let mut overflowed = false;

    loop {
        match uart_rx.read(&mut byte).await {
            Ok(1) => {}
            Ok(_) | Err(_) => continue,
        }

        let ch = byte[0];
        if ch.is_ascii_alphanumeric() {
            UART_TX_CH.send(UartTxMsg::Echo(ch)).await;
        }

        match ch {
            b'\n' => {
                if overflowed {
                    UART_TX_CH
                        .send(UartTxMsg::Static(b"> Command too long"))
                        .await;
                } else {
                    if ln.ends_with('\r') {
                        ln.pop();
                    }
                    config_handler(ln.as_str()).await;
                }
                ln.clear();
                overflowed = false;
            }

            // Handle delete and backspace
            8 | 127 if !overflowed => {
                if ln.pop().is_some() {
                    UART_TX_CH.send(UartTxMsg::Static(b"\x08 \x08")).await;
                }
            }

            b if b.is_ascii() && !overflowed => {
                let _ = ln.push(b as char);
            }

            _ => {}
        }
    }
}

#[embassy_executor::task]
async fn interrupt_handler(irq: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PA12>) {
    // Assert interrupt high at reset to indicate need for configuration.
    let mut irq = Output::new(irq, Level::High, Speed::Low);

    loop {
        while !((MBusState::from_u32(MBUSSTATE.load(Relaxed)) == MBusState::OverCurrentError)
            || BOOT_IND.load(Relaxed))
        {
            IRQ_CH.wait().await;
        }
        irq.set_high();
        while (MBusState::from_u32(MBUSSTATE.load(Relaxed)) == MBusState::OverCurrentError)
            || BOOT_IND.load(Relaxed)
        {
            IRQ_CH.wait().await;
        }
        irq.set_low();
    }
}

#[embassy_executor::task]
async fn mbus_en_handler(mut mbus_en: Output<'static>) {
    loop {
        MBUS_SIG.wait().await;
        match MBusState::from_u32(MBUSSTATE.load(Relaxed)) {
            MBusState::Disabled | MBusState::OverCurrentError => {
                mbus_en.set_low();
            }
            MBusState::Enabled => {
                mbus_en.set_high();
            }
        }
    }
}

#[embassy_executor::task]
async fn mbus_oc_handler(mut mbus_oc: ExtiInput<'static, Async>) {
    loop {
        mbus_oc.wait_for_rising_edge().await;
        MBUSSTATE.store(MBusState::OverCurrentError.as_u32(), Relaxed);
        MBUS_SIG.signal(());
        IRQ_CH.signal(());
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());
    info!("Hello emonHP!");

    /* emonHP board specific resets for bring up */
    // DHW
    // let _opa1 = Input::new(p.PA2, Pull::Down);
    // let _opa1_pu = Input::new(p.PA3, Pull::None);
    // // Pulse counting; soft pull down.
    let _opa2 = Input::new(p.PA4, Pull::Down);
    let _opa2_pu = Input::new(p.PA5, Pull::None);
    // // OneWire; hard pull up.
    let _opa3 = Input::new(p.PA6, Pull::None);
    let _opa3_pu = Output::new(p.PA7, Level::High, Speed::Low);

    let mbus_oc = ExtiInput::new(p.PB0, p.EXTI0, Pull::Down, Irqs);
    let mbus_en = Output::new(p.PB1, Level::Low, Speed::Low);

    /* Initialise UART. Split into Rx and Tx parts for different tasks
      REVISIT : USART1 and PB7 (rx) and PB6 (tx) for real board
    */
    let mut uart_cfg = embassy_stm32::usart::Config::default();
    uart_cfg.baudrate = 115_200;
    // let mut uart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, uart_cfg).unwrap();
    let uart = BufferedUart::new(
        p.USART2,
        p.PA3,
        p.PA2,
        UART_TX_BUF.init([0; 512]),
        UART_RX_BUF.init([0; 64]),
        Irqs,
        uart_cfg,
    )
    .unwrap();

    let (uart_tx, uart_rx) = uart.split();

    display_boot(p.I2C1, p.PB8, p.PB9);

    // Drive !FW pin LOW to indicate working firmware
    let _ = Output::new(p.PB3, Level::Low, Speed::Low);

    _spawner.spawn(interrupt_handler(p.PA12)).unwrap();
    _spawner.spawn(mbus_oc_handler(mbus_oc)).unwrap();
    _spawner.spawn(mbus_en_handler(mbus_en)).unwrap();
    _spawner.spawn(uart_tx_task(uart_tx)).unwrap();
    _spawner.spawn(uart_rx_task(uart_rx)).unwrap();

    config_handler("v").await;

    loop {
        core::future::pending::<()>().await;
    }
}
