# OpenBarista ESP32 Telemetry in Rust

Rust firmware for an ESP32 using the ESP-IDF ecosystem. It reads:

- PT100 temperature through a MAX31865 over SPI
- Pressure from an analog sensor on GPIO34

The firmware prints one line every 500 ms over the serial console:

```text
Temp: XX.XX C | Pressure: X.XX bar | Pressure (PSI): XX.XX | RawADC: XXXX | Volt: X.XX
```

## Quick start

This repo is now set up for a two-command workflow:

```sh
bash scripts/bootstrap.sh
bash scripts/flash.sh
```

What that does:

- `scripts/bootstrap.sh` installs the Rust-side tooling you actually need, with the locked versions that work with your current Rust toolchain.
- `scripts/bootstrap.sh` also installs the Espressif toolchain with `espup` and generates a repo-local environment file at `.esp/export-esp.sh`.
- `scripts/flash.sh` sources `.esp/export-esp.sh`, builds the firmware, flashes the board, and opens the serial monitor through the configured Cargo runner.

If you only want to compile without flashing:

```sh
bash scripts/build.sh
```

## Project layout

```text
.
├── .cargo/
│   └── config.toml
├── build.rs
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
└── src/
    ├── main.rs
    └── sensors/
        ├── mod.rs
        ├── pressure.rs
        └── temperature.rs
```

## Hardware mapping

### MAX31865

- CS: GPIO5
- SCLK: GPIO18
- MISO: GPIO19
- MOSI: GPIO23
- SPI mode: 1
- PT100 nominal resistance: 100.0 ohm
- Reference resistor: 430.0 ohm
- Wiring mode: 3-wire
- Software calibration offset: +4.0 C

### Pressure sensor

- ADC pin: GPIO34
- ADC mode: one-shot
- Attenuation: 12 dB
- Conversion assumptions:
  - 12-bit raw range: 0..4095
  - voltage derived directly from raw ADC counts using 3.3 V reference
  - 0 PSI = 0.35 V
  - 200 PSI = 4.5 V

## Important electrical note

The ESP32 ADC pin must never see more than 3.3 V. If your pressure sensor can really output up to 4.5 V, you need external scaling or conditioning before GPIO34. The code still applies the conversion formula you requested to the measured voltage.

## Prerequisites

You need the normal system build tools installed first. The helper script handles the Rust and Espressif-specific setup, but it does not use `apt`, `pacman`, or `dnf` for you.

Debian or Ubuntu:

```sh
sudo apt-get install git wget flex bison gperf python3 python3-pip python3-venv cmake ninja-build ccache libffi-dev libssl-dev dfu-util libusb-1.0-0 libudev-dev
```

Arch Linux:

```sh
sudo pacman -S --needed git wget flex bison gperf python python-pip cmake ninja ccache libffi openssl dfu-util libusb base-devel pkgconf
```

After that, use the repo bootstrap command instead of manually running `cargo install` lines from memory.

## Install flow details

`scripts/bootstrap.sh` intentionally does these steps in a safe order:

- ensures `~/.cargo/bin` is on your shell `PATH`
- installs `espup` with `cargo +stable install --locked espup`
- runs `espup install --targets esp32 --std --export-file "$PWD/.esp/export-esp.sh"`
- installs `ldproxy`, `espflash`, and `cargo-espflash` with `cargo install --locked ...`

The `--locked` part matters. Without it, crates like `espflash` can pull newer dependencies that require a newer `rustc` than the one you currently have, which is exactly the failure you hit.

The project is configured for ESP32 with target `xtensa-esp32-espidf` in `.cargo/config.toml`.

## Build

From the project root:

```sh
bash scripts/build.sh
```

## Flash

Build, flash, and open the serial monitor in one step:

```sh
bash scripts/flash.sh
```

If you want to run the raw commands yourself, the equivalent is:

```sh
source .esp/export-esp.sh
cargo run
```

## Behavior summary

- Temperature is read from the MAX31865 in one-shot mode.
- RTD resistance is converted to Celsius with the standard PT100 formula.
- A +4.0 C calibration offset is applied.
- Pressure is sampled from ADC1/GPIO34.
- Pressure voltage is computed the same way as the working Arduino sketch: `raw / 4095.0 * 3.3`.
- Negative PSI results are clamped to 0.
- Pressure is also converted to bar using `1 PSI = 0.0689476 bar`.

## If you need different pins

Update the pin selections in `src/main.rs` to match your wiring. Only `GPIO5` and `GPIO34` were fixed by your requirements; the SPI bus pins use the common ESP32 defaults in this project.