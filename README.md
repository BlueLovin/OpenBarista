# OpenBarista

OpenBarista is an ESP32-based espresso telemetry and profiling firmware project written in Rust on top of ESP-IDF.

The goal is to measure the two signals that matter most during a shot:

- Brew temperature from a PT100 RTD probe through a MAX31865 interface board
- Brew pressure from an analog pressure transducer connected to the ESP32 ADC

The firmware streams those readings over the serial console every 500 ms so the machine can be profiled, calibrated, and monitored while testing hardware changes.

## What this repository is

This repo contains the embedded firmware, build scripts, and toolchain setup needed to run the telemetry stack on an ESP32.

It is not a desktop dashboard or data logger by itself. Its current job is to:

- initialize the SPI temperature interface
- sample the pressure sensor through ADC1
- convert raw sensor data into Celsius, PSI, and bar
- print telemetry to the serial monitor in a stable loop

Current serial output format:

```text
Temp: 93.42 C | Pressure: 8.74 bar | Pressure (PSI): 126.79
```

## Project goal

The practical goal of OpenBarista is espresso profiling.

That means giving you a repeatable live view of:

- whether the brew water is near the target temperature
- how fast pressure builds
- what peak pressure the system reaches
- whether hardware or control changes improve shot consistency

This firmware is the sensor and telemetry layer for that work.

## Hardware

The project is currently set up for the following hardware arrangement.

### Core controller

- ESP32

### Temperature path

- PT100 RTD temperature probe
- MAX31865 RTD-to-digital amplifier board
- 3-wire RTD configuration

Configuration used by the firmware:

- PT100 nominal resistance: 100.0 ohm
- Reference resistor: 431.0 ohm
- Calibration offset: +4.0 C
- SPI mode: 1

ESP32 pin mapping for the MAX31865:

- CS: GPIO5
- SCLK: GPIO18
- MOSI: GPIO23
- MISO: GPIO19

### Pressure path

- Analog pressure sensor / pressure transducer
- Sensor output connected to ESP32 GPIO34
- ADC1 one-shot mode
- 12 dB attenuation

Pressure conversion model used in firmware:

- Raw ADC range: 0..4095
- Voltage calculation: `raw / 4095.0 * 3.3`
- 0 PSI reference voltage: 0.35 V
- Full-scale pressure: 200 PSI
- Full-scale sensor voltage used in conversion: 4.5 V
- PSI to bar conversion: `1 PSI = 0.0689476 bar`

Important electrical constraint:

The ESP32 ADC input must not be driven above 3.3 V. If the pressure sensor can output more than 3.3 V, you need external scaling or signal conditioning before feeding GPIO34.

## Repository layout

```text
.
├── build.rs
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
├── scripts/
│   ├── bootstrap.sh
│   ├── build.sh
│   └── flash.sh
└── src/
    ├── main.rs
    └── sensors/
        ├── mod.rs
        ├── pressure.rs
        └── temperature.rs
```

Key files:

- `src/main.rs`: board setup, sensor initialization, main telemetry loop
- `src/sensors/temperature.rs`: MAX31865 driver and PT100 temperature conversion
- `src/sensors/pressure.rs`: ADC pressure sampling and PSI/bar conversion
- `scripts/bootstrap.sh`: installs the required Rust and Espressif tooling
- `scripts/build.sh`: builds the firmware
- `scripts/flash.sh`: flashes the board and opens the serial monitor

## Software stack

- Rust
- ESP-IDF
- `esp-idf-hal`
- `esp-idf-svc`
- `espflash`
- `espup`

The project target is configured for ESP32 with `xtensa-esp32-espidf`.

## Prerequisites

Install the normal system dependencies first. The repo scripts handle the Rust and Espressif-specific setup, but they do not install OS packages for you.

Debian or Ubuntu:

```sh
sudo apt-get install git wget flex bison gperf python3 python3-pip python3-venv cmake ninja-build ccache libffi-dev libssl-dev dfu-util libusb-1.0-0 libudev-dev
```

Arch Linux:

```sh
sudo pacman -S --needed git wget flex bison gperf python python-pip cmake ninja ccache libffi openssl dfu-util libusb base-devel pkgconf
```

You also need a working Rust installation with `cargo` and `rustup` available.

## Setup

From the repository root:

```sh
bash scripts/bootstrap.sh
```

What the bootstrap script does:

- ensures `~/.cargo/bin` is on your shell `PATH`
- installs `espup` if it is missing
- installs the Espressif Rust toolchain named `esp`
- writes a repo-local environment file at `.esp/export-esp.sh`
- installs `ldproxy`, `espflash`, and `cargo-espflash`

After bootstrap, open a new shell or source your shell rc file if the script updated your `PATH`.

## Build

To compile the firmware only:

```sh
bash scripts/build.sh
```

That script:

- loads `.esp/export-esp.sh`
- selects the ESP toolchain environment
- runs `cargo build`

## Flash and monitor

To build, flash, and open the serial monitor:

```sh
bash scripts/flash.sh
```

That script:

- loads `.esp/export-esp.sh`
- runs `cargo run`

The Cargo runner is configured to flash the board and attach a serial monitor, so this is the normal development workflow.

## Firmware behavior

On each loop iteration, the firmware:

- reads the PT100 through the MAX31865 over SPI
- applies the configured temperature calibration offset
- reads the pressure sensor from ADC1 on GPIO34
- converts the pressure reading into PSI and bar
- prints one telemetry line over serial
- waits 500 ms before the next sample

## Notes on calibration

Current calibration and conversion values are hard-coded in the firmware.

Temperature:

- PT100 nominal resistance: 100.0 ohm
- Reference resistor: 431.0 ohm
- Offset: +4.0 C

Pressure:

- zero-voltage offset: 0.35 V
- full-scale voltage: 4.5 V
- full-scale pressure: 200 PSI

If your hardware changes, update the values in the sensor modules before treating the readings as authoritative.

## Development workflow summary

Typical first-time setup:

```sh
bash scripts/bootstrap.sh
bash scripts/flash.sh
```

Typical edit-build-test cycle after setup:

```sh
bash scripts/build.sh
bash scripts/flash.sh
```

## Current scope

This repository currently focuses on embedded acquisition and live telemetry. If you later add logging, networking, shot storage, or a UI, those would sit on top of the firmware provided here.