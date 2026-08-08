# emonHP Firmware

This contains the firmware for the [emonHP](https://github.com/openenergymonitor/emonhp) heatpump monitoring system.

## Getting started

> [!NOTE]
> If you buy a system from the [OpenEnergyMonitor shop](https://shop.openenergymonitor.org) the Raspberry Pi will be configured and the firmware installed already.

### Raspberry Pi

After cloning the repository, in `scripts/` run `sudo setup_pi.sh`. This will enable thr 

### emonHP Firmware

> [!TIP]
> Most users will use pre-built binaries from releases. You only need to compile from source for features or fixes that are not in a release, or if you developing the firmware.

#### Uploading

When you have the firmware binary available:

- If you have the Raspberry Pi attached to the GPIO pins, in `script` run `flash_pi.sh <path to .elf>`.
- If you have an external debugger, run `openocd -f openocd.cfg -c program <path to .elf> verify reset exit`.

#### Compiling

You will need to have the [Rust compiler installed](https://rust-lang.org/tools/install/).

To build the firmware, run `cargo build`. This will build the debug version of the firmware. To build the release version, run `cargo build --release`.

> [!TIP]
> It is strongly recommended to compile the firmware on a reasonably power device. While it is possible to compile on the Raspberry Pi, it will take around 10-15 minutes on the Raspberry Pi 4 with 1 GB.

## Getting in contact

Issues can be reported:

- As a [GitHub issue](https://github.com/openenergymonitor/emonhp/issues).
- On the [OpenEnergyMonitor forums](https://community.openenergymonitor.org).

Please include as much information as possible (run the `v` command on the serial link, including at least:

- The emonHP hardware and firmware version.
- A full description, including a reproduction if possible, of the issue.

## Contributing

Contributions are welcome! Small PRs can be accepted at any time. Please get in touch before making _large_ changes.

> [!NOTE]
> Please bear in mind that this is an open source project and PRs and enhancements may not be addressed quickly, or at all. This is no comment on the quality of the contribution, and please feel free to fork as you like!

