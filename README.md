# OpenBarista

OpenBarista is ESP32 firmware for espresso telemetry and on-device monitoring, written in Rust on top of ESP-IDF.

It samples:

- Brew temperature from a PT100 RTD through MAX31865 (SPI)
- Brew pressure from an analog transducer on ADC1

Then it exposes live data through:

- Serial logs
- An embedded HTTP dashboard served directly by the device

## Current Status

This repository is firmware-first and currently includes:

- Sensor sampling loop (temperature + pressure)
- BLE scale scanning, saved pairing, and live weight / flow telemetry
- Shared in-memory telemetry feed
- Wi-Fi provisioning flow with captive portal fallback
- Station-mode dashboard and settings pages
- Persistent Wi-Fi and device settings in ESP NVS
- Build metadata and stable board identity in the UI

## Runtime Behavior

At boot, the firmware:

1. Initializes telemetry state
2. Initializes Bluetooth scale support
3. Starts Wi-Fi setup logic
4. Tries to connect using saved credentials
5. Falls back to SoftAP provisioning if needed
6. Starts station HTTP services when connected
7. Continuously samples sensors and updates telemetry

Main telemetry loop rate is currently 50 ms.

Serial output example:

```text
Temp: 93.42 C | Pressure: 8.74 bar | Pressure (PSI): 126.79
```

## Wi-Fi Modes

### Station mode (normal operation)

If credentials are present and valid, OpenBarista joins your network and serves:

- `http://<device-ip>`
- `http://openbarista.local` (when mDNS is available)

### Provisioning mode (captive portal)

If no credentials are saved, or connection retries fail, the device starts:

- Open access point SSID: `OpenBarista`
- Captive portal / setup server on `http://192.168.4.1`

Credentials are validated, saved to NVS, and the device reboots.

## Web UI and API

The UI assets are embedded from `assets/` at compile time via `include_bytes!` / `include_str!`.

### Main routes

- `GET /` -> station dashboard
- `GET /settings` -> device settings page
- `GET /health` -> plain `ok`
- `GET /api/telemetry` -> latest telemetry snapshot JSON
- `GET /api/scale` -> scale status, saved pairing, and discovered devices JSON
- `GET /api/settings` -> current device settings JSON
- `POST /api/scale` -> scale scan / connect / disconnect / forget actions
- `POST /api/settings` -> update settings (and optionally Wi-Fi credentials)
- `GET /networks` -> known/safe network list for UI flow

### Provisioning routes

- `GET /` and captive-detection aliases -> captive setup page
- `GET /portal.css`, `GET /portal.js` -> captive assets
- `GET /status` -> connection/provisioning status JSON
- `GET /networks` -> scanned or known networks (mode dependent)
- `POST /connect` -> save credentials and reboot

## Settings and Persistence

Settings are stored in ESP NVS:

- Namespace `wifi`: SSID and password
- Namespace `settings`: device label and temperature offset
- Namespace `scale`: one saved BLE scale address, name, and address type

Current settings API supports:

- Device label updates
- Temperature offset updates
- Optional Wi-Fi credential updates
- One saved BLE scale with scan, connect, disconnect, and forget actions

Wi-Fi updates trigger a reboot to apply network changes.

## Bluetooth Scale Support

OpenBarista now includes BLE-only scale support on the ESP32 side.

- The station dashboard shows live scale weight and estimated flow.
- The settings page uses a simple pairing flow: Find Scales, tap the device, connect.
- The firmware saves one preferred scale in NVS and attempts to reconnect it on boot.
- Compatibility is best-effort generic BLE plus a standards-based weight characteristic path; exact behavior still depends on the scale's protocol.

## Hardware Configuration

### Controller

- ESP32

### Temperature path

- PT100 RTD probe
- MAX31865 RTD interface board (3-wire)
- SPI mode 1

Current pin mapping:

- CS: GPIO5
- SCLK: GPIO18
- MOSI: GPIO23
- MISO: GPIO19

### Pressure path

- Analog transducer on GPIO34 (ADC1)
- ADC attenuation: 12 dB

Conversion model (shared with host-side tests):

- Raw voltage: `raw / 4095.0 * 3.3`
- Zero reference: `0.35 V`
- Full-scale voltage: `4.5 V`
- Full-scale pressure: `200 PSI`
- PSI -> bar: `1 PSI = 0.0689476 bar`

Important: do not drive ESP32 ADC pins above 3.3 V.

## Toolchain and Build

Target and runner are configured in `.cargo/config.toml`:

- Target: `xtensa-esp32-espidf`
- Linker: `ldproxy`
- Runner: `espflash flash --monitor`

Toolchain channel is pinned in `rust-toolchain.toml`:

- `esp`

`build.rs` exports `OPENBARISTA_BUILD_ID` using git short SHA + epoch.

## Setup

Install system dependencies before running the bootstrap script.

### Fedora

```sh
sudo dnf install -y cmake python3 git gcc g++ \
  openssl-devel libudev-devel ninja-build dfu-util
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Debian / Ubuntu

```sh
sudo apt update && sudo apt install -y \
  cmake python3 python3-venv git build-essential \
  libssl-dev libudev-dev ninja-build dfu-util
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

On Debian/Ubuntu-family systems, `esp-idf-sys` needs the `venv` package for the
active Python version so it can create the managed ESP-IDF virtualenv. If
`cargo build` fails with an `ensurepip is not available` error, install
`python3-venv` (or the versioned package such as `python3.13-venv`).

Then run:

```sh
bash scripts/bootstrap.sh
```

What bootstrap does:

- Ensures `~/.cargo/bin` is on your PATH
- Installs the host Rust toolchain `stable-<host-triple>` for desktop tooling
- Installs `espup` (if missing)
- Installs Espressif Rust toolchain named `esp`
- Installs the full Xtensa LLVM/libclang payload needed by `bindgen`
- Generates `.esp/export-esp.sh`
- Installs `ldproxy`, `espflash`, `cargo-espflash`

If an older ESP toolchain install fails with a `Unable to find libclang` panic
from `bindgen`, rerun `bash scripts/bootstrap.sh` to refresh the LLVM install
and regenerate `.esp/export-esp.sh`.

## Headless UI with Mock Data

You can run the station UI locally without flashing hardware:

```sh
python3 scripts/headless_ui.py --port 4173
```

Then open `http://127.0.0.1:4173` or point Playwright at that URL.

What the mock server provides:

- The same station/settings HTML, CSS, and JS from `assets/station/`
- Mock implementations of `/api/telemetry`, `/api/scale`, `/api/settings`, `/networks`, and `/health`
- In-memory settings and Bluetooth scale actions so UI flows can be exercised end-to-end

Useful options:

```sh
python3 scripts/headless_ui.py --host 0.0.0.0 --public-host 127.0.0.1 --port 4173
python3 scripts/headless_ui.py --build-id local-dev --board-id MOCK-BENCH
```

## Build and Flash

After environment setup, use normal Cargo flow:

```sh
cargo build
cargo run
```

Because the runner is configured, `cargo run` flashes and opens monitor output.

If you need explicit env export in your shell session:

```sh
source .esp/export-esp.sh
```

## Host-Side Tests

Math and telemetry logic include host-runnable tests.

Example:

```sh
cargo +stable test --lib --target x86_64-unknown-linux-gnu
```

## Project Layout

```text
.
├── assets/
│   ├── portal/
│   └── station/
├── main/
│   └── idf_component.yml
├── scripts/
│   └── bootstrap.sh
├── src/
│   ├── lib.rs
│   ├── main.rs
│   ├── telemetry_feed.rs
│   ├── telemetry_math.rs
│   ├── web_assets.rs
│   ├── wifi_provision.rs
│   └── sensors/
│       ├── mod.rs
│       ├── pressure.rs
│       └── temperature.rs
├── build.rs
├── Cargo.toml
└── rust-toolchain.toml
```

## Notes

- Firmware binary builds are intended for xtensa targets.
- The project currently focuses on embedded sensing, connectivity, and local web UX.
- No desktop app or cloud backend is required for core operation.
