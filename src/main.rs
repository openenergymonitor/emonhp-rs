#![no_std]
#![no_main]

use core::future::pending;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};

use embassy_stm32::{
    bind_interrupts,
    exti::{self, ExtiInput},
    gpio::{Input, Level, Output, Pull, Speed},
    i2c::I2c,
    interrupt,
    mode::Async,
    peripherals::IWDG,
    time::Hertz,
    usart::{BufferedUart, BufferedUartRx, BufferedUartTx},
    wdg::IndependentWatchdog,
};

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};

use embassy_time::{Duration, Timer};
use embedded_graphics::{
    Drawable,
    geometry::Point,
    image::{Image, ImageRaw},
    mono_font::{MonoTextStyleBuilder, ascii::FONT_6X10},
    pixelcolor::BinaryColor,
    text::{Baseline, Text},
};
use embedded_io_async::{Read, Write};

use ssd1306::{I2CDisplayInterface, Ssd1306, mode::DisplayConfig, size::DisplaySize128x64};
use static_cell::StaticCell;

use panic_probe as _;

const UART_BAUD: u32 = 115_200;
const UART_TX_BUF_LEN: usize = 512;
const UART_RX_BUF_LEN: usize = 64;
const MBUS_FAULT_FLASH_MS: u64 = 250;
const OLED_I2C_FREQ: Hertz = Hertz(400_000);

enum UartTxMsg {
    Echo(u8),
    Static(&'static [u8]),
}

#[repr(u32)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum MBusState {
    Disabled,
    Enabled,
    OverCurrentError,
}

struct AtomicMbusState(AtomicU32);

impl AtomicMbusState {
    const fn new(state: MBusState) -> Self {
        Self(AtomicU32::new(Self::encode(state)))
    }

    fn load(&self) -> MBusState {
        Self::decode(self.0.load(Relaxed))
    }

    fn store(&self, state: MBusState) {
        self.0.store(Self::encode(state), Relaxed);
    }

    const fn encode(state: MBusState) -> u32 {
        match state {
            MBusState::Disabled => 0,
            MBusState::Enabled => 1,
            MBusState::OverCurrentError => 2,
        }
    }

    fn decode(value: u32) -> MBusState {
        match value {
            0 => MBusState::Disabled,
            1 => MBusState::Enabled,
            2 => MBusState::OverCurrentError,
            _ => MBusState::Disabled,
        }
    }
}

static UART_TX_CH: Channel<CriticalSectionRawMutex, UartTxMsg, 2> = Channel::new();
static UART_TX_BUF: StaticCell<[u8; UART_TX_BUF_LEN]> = StaticCell::new();
static UART_RX_BUF: StaticCell<[u8; UART_RX_BUF_LEN]> = StaticCell::new();

static MBUS_EN_SIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static MBUS_LED_SIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static MBUS_STATE: AtomicMbusState = AtomicMbusState::new(MBusState::Disabled);

static IRQ_CH: Signal<CriticalSectionRawMutex, ()> = Signal::new();

static SHDN_SIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static PWR_OFF_SIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();

bind_interrupts!(
    pub struct Irqs{
        EXTI0_1 => exti::InterruptHandler<interrupt::typelevel::EXTI0_1>;
        EXTI4_15 => exti::InterruptHandler<interrupt::typelevel::EXTI4_15>;

        USART1 => embassy_stm32::usart::BufferedInterruptHandler<embassy_stm32::peripherals::USART1>;
});

async fn config_handler(cmd: &str) {
    let help_str: &'static str = "- ?               : Print this message again\r\n\
                          - b               : MBus regulator status\r\n\
                          - b<n>            : set MBus regulator, n = 0 OFF, n = 1 ON\r\n\
                          - h               : power down the system\r\n
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
            UART_TX_CH.send(UartTxMsg::Static(b"> MBus: ")).await;
            match mbus_state() {
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
            if mbus_state() == MBusState::Enabled {
                mbus_state_set(MBusState::Disabled);
            }
            UART_TX_CH
                .send(UartTxMsg::Static(b"> MBus: Disabled\r\n"))
                .await;
        }
        b"b1" => {
            if matches!(
                mbus_state(),
                MBusState::Disabled | MBusState::OverCurrentError
            ) {
                mbus_state_set(MBusState::Enabled);
                UART_TX_CH
                    .send(UartTxMsg::Static(b"> MBus: Enabled\r\n"))
                    .await;
            }
        }
        b"i" => {
            // Very basic handler [4] is MBus overcurrent
            if mbus_state() == MBusState::OverCurrentError {
                UART_TX_CH.send(UartTxMsg::Echo(b'1')).await;
            } else {
                UART_TX_CH.send(UartTxMsg::Echo(b'0')).await;
            }

            UART_TX_CH.send(UartTxMsg::Static(b"0\r\n")).await;
        }
        b"i4" => {
            mbus_state_set(MBusState::Disabled);
            UART_TX_CH
                .send(UartTxMsg::Static(b"> MBus disabled, interrupt cleared\r\n"))
                .await;
        }
        b"h" => {
            mbus_state_set(MBusState::Disabled);
            SHDN_SIG.signal(());
            UART_TX_CH
                .send(UartTxMsg::Static(b"> Shutting down...\r\n"))
                .await;
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
async fn display_handler(
    i2c: embassy_stm32::Peri<'static, embassy_stm32::peripherals::I2C1>,
    scl: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PA9>,
    sda: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PA10>,
) {
    /* Set image + text on the OLED */
    let mut i2c_cfg = embassy_stm32::i2c::Config::default();
    i2c_cfg.frequency = OLED_I2C_FREQ;
    let i2c = I2c::new_blocking(i2c, scl, sda, i2c_cfg);

    let ssd_interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(
        ssd_interface,
        DisplaySize128x64,
        ssd1306::rotation::DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();

    for _ in 0..4 {
        let mut i2c_success = true;
        match display.init() {
            Ok(_) => {
                let raw: ImageRaw<BinaryColor> =
                    ImageRaw::new(include_bytes!("./emonhp_64x64.raw"), 64);
                let im = Image::new(&raw, Point::new(32, 0));

                match im.draw(&mut display) {
                    Ok(_) => (),
                    Err(_) => {
                        i2c_success = false;
                    }
                }

                match display.flush() {
                    Ok(_) => (),
                    Err(_) => {
                        i2c_success = false;
                    }
                }
            }
            Err(_) => {
                i2c_success = false;
            }
        }
        if i2c_success {
            break;
        } else {
            Timer::after_millis(100).await;
        }
    }

    loop {
        SHDN_SIG.wait().await;

        let text_style = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();

        // Spinner style countdown for 30 s, then remove power from RPi.
        let mut s_type: u32 = 0;
        for _ in (1..=30).rev() {
            display.clear_buffer();

            Text::with_baseline("emonHP", Point::new(48, 0), text_style, Baseline::Top)
                .draw(&mut display)
                .unwrap();

            Text::with_baseline(
                "Shutting down...",
                Point::new(10, 16),
                text_style,
                Baseline::Top,
            )
            .draw(&mut display)
            .unwrap();

            let spinner;
            match s_type {
                0 => spinner = "|",
                1 => spinner = "/",
                2 => spinner = "-",
                3 => spinner = r"\",
                _ => spinner = "|",
            }
            s_type = (s_type + 1) & 0x3;
            Text::with_baseline(spinner, Point::new(110, 16), text_style, Baseline::Top)
                .draw(&mut display)
                .unwrap();
            display.flush().unwrap();

            Timer::after(Duration::from_secs(1)).await;
        }

        display.clear_buffer();
        Text::with_baseline("emonHP", Point::new(48, 0), text_style, Baseline::Top)
            .draw(&mut display)
            .unwrap();

        Text::with_baseline("Shut down.", Point::new(37, 16), text_style, Baseline::Top)
            .draw(&mut display)
            .unwrap();

        display.flush().unwrap();

        PWR_OFF_SIG.signal(());
    }
}

#[embassy_executor::task]
async fn interrupt_handler(irq: embassy_stm32::Peri<'static, embassy_stm32::peripherals::PA12>) {
    let mut irq = Output::new(irq, Level::Low, Speed::Low);

    loop {
        while !(mbus_state() == MBusState::OverCurrentError) {
            IRQ_CH.wait().await;
        }
        irq.set_high();
        while mbus_state() == MBusState::OverCurrentError {
            IRQ_CH.wait().await;
        }
        irq.set_low();
    }
}

#[embassy_executor::task]
async fn mbus_en_handler(mut mbus_en: Output<'static>) {
    loop {
        MBUS_EN_SIG.wait().await;
        match mbus_state() {
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
async fn mbus_led_handler(mut mbus_led_g: Output<'static>, mut mbus_led_r: Output<'static>) {
    // LED: ENABLED -> GREEN; DISABLED -> YELLOW; OVERCURRENT -> FLASHING RED.
    // LEDs are active LOW.
    loop {
        // Create flashing timer if over current. If not in over current, future won't expire.
        let t_flash = async {
            if mbus_state() == MBusState::OverCurrentError {
                Timer::after(Duration::from_millis(MBUS_FAULT_FLASH_MS)).await;
            } else {
                pending::<()>().await;
            }
        };

        match select(MBUS_LED_SIG.wait(), t_flash).await {
            Either::First(()) => match mbus_state() {
                MBusState::Disabled => {
                    mbus_led_g.set_low();
                    mbus_led_r.set_low();
                }
                MBusState::Enabled => {
                    mbus_led_g.set_low();
                    mbus_led_r.set_high();
                }
                MBusState::OverCurrentError => {
                    mbus_led_g.set_high();
                    mbus_led_r.set_low();
                }
            },
            Either::Second(()) => {
                mbus_led_r.toggle();
            }
        }
    }
}

#[embassy_executor::task]
async fn mbus_oc_handler(mut mbus_oc: ExtiInput<'static, Async>) {
    loop {
        mbus_oc.wait_for_falling_edge().await;
        mbus_state_set(MBusState::OverCurrentError);
    }
}

fn mbus_state() -> MBusState {
    MBUS_STATE.load()
}

fn mbus_state_set(state: MBusState) {
    if mbus_state() == state {
        return;
    }

    MBUS_STATE.store(state);
    MBUS_EN_SIG.signal(());
    MBUS_LED_SIG.signal(());
    IRQ_CH.signal(());
}

#[embassy_executor::task]
async fn pwr_en_handler(mut pwr_en: Output<'static>) {
    loop {
        PWR_OFF_SIG.wait().await;
        pwr_en.set_low();
    }
}

#[embassy_executor::task]
async fn uart_rx_task(mut uart_rx: BufferedUartRx<'static>) {
    let mut ln = heapless::String::<64>::new();
    let mut byte = [0u8; 1];
    let mut overflowed = false;
    let mut ignore_lf = false;

    loop {
        match uart_rx.read(&mut byte).await {
            Ok(1) => {}
            Ok(_) | Err(_) => continue,
        }

        let ch = byte[0];
        UART_TX_CH.send(UartTxMsg::Echo(ch)).await;

        match ch {
            b'\r' | b'\n' => {
                // Accept either common line ending, but do not run CRLF twice.
                if ch == b'\n' && ignore_lf {
                    ignore_lf = false;
                    continue;
                }

                if overflowed {
                    UART_TX_CH
                        .send(UartTxMsg::Static(b"> Command too long\r\n"))
                        .await;
                } else {
                    if ln.ends_with('\r') {
                        ln.pop();
                    }
                    config_handler(ln.as_str()).await;
                }
                ln.clear();
                overflowed = false;
                ignore_lf = ch == b'\r';
            }

            // Handle delete and backspace
            8 | 127 if !overflowed => {
                if ln.pop().is_some() {
                    UART_TX_CH.send(UartTxMsg::Static(b"\x08 \x08")).await;
                }
            }

            b if b.is_ascii() && !overflowed && ln.push(b as char).is_err() => {
                overflowed = true;
                ignore_lf = false;
            }

            _ => {
                ignore_lf = false;
            }
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
async fn wdt_handler(mut wdt: IndependentWatchdog<'static, IWDG>) {
    wdt.unleash();
    loop {
        Timer::after_millis(250).await;
        wdt.pet();
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let wdt = IndependentWatchdog::new(p.IWDG, 1_000_000);

    // DHW; high-Z.
    let _opa1 = Input::new(p.PA2, Pull::None);
    let _opa1_pu = Input::new(p.PA3, Pull::None);
    // Pulse counting; soft pull down.
    let _opa2 = Input::new(p.PA4, Pull::Down);
    let _opa2_pu = Input::new(p.PA5, Pull::None);
    // OneWire; hard pull up.
    let _opa3 = Input::new(p.PA6, Pull::None);
    let _opa3_pu = Output::new(p.PA7, Level::High, Speed::Low);

    let mbus_oc = ExtiInput::new(p.PB0, p.EXTI0, Pull::Up, Irqs);
    let mbus_en = Output::new(p.PB1, Level::Low, Speed::Low);
    let mbus_led_g = Output::new(p.PB5, Level::Low, Speed::Low);
    let mbus_led_r = Output::new(p.PB4, Level::Low, Speed::Low);

    let pwr_en = Output::new(p.PB8, Level::High, Speed::Low);

    /* Initialise UART. Split into Rx and Tx parts for different tasks */
    let mut uart_cfg = embassy_stm32::usart::Config::default();
    uart_cfg.baudrate = UART_BAUD;

    let uart = BufferedUart::new(
        p.USART1,
        p.PB7,
        p.PB6,
        UART_TX_BUF.init([0; UART_TX_BUF_LEN]),
        UART_RX_BUF.init([0; UART_RX_BUF_LEN]),
        Irqs,
        uart_cfg,
    )
    .unwrap();

    let (uart_tx, uart_rx) = uart.split();

    spawner.spawn(wdt_handler(wdt)).unwrap();

    spawner.spawn(pwr_en_handler(pwr_en)).unwrap();
    spawner.spawn(interrupt_handler(p.PA12)).unwrap();
    spawner.spawn(mbus_oc_handler(mbus_oc)).unwrap();
    spawner.spawn(mbus_en_handler(mbus_en)).unwrap();
    spawner
        .spawn(mbus_led_handler(mbus_led_g, mbus_led_r))
        .unwrap();
    spawner.spawn(uart_tx_task(uart_tx)).unwrap();
    spawner.spawn(uart_rx_task(uart_rx)).unwrap();

    // Drive !FW pin LOW to indicate working firmware
    let _fw = Output::new(p.PB3, Level::Low, Speed::Low);
    mbus_state_set(MBusState::Enabled);

    spawner
        .spawn(display_handler(p.I2C1, p.PA9, p.PA10))
        .unwrap();

    config_handler("v").await;

    loop {
        core::future::pending::<()>().await;
    }
}
