---
layout: default
title: Building & Flashing
nav_order: 5
---

# Building & Flashing

How to compile OpenBarista and load it onto your ESP32.

---

## Quick Start

Once your [toolchain is set up]({{ site.baseurl }}/setup/):

```sh
cd OpenBarista
source .esp/export-esp.sh
cargo run
```

That's it. `cargo run` compiles the firmware, flashes it to the connected ESP32, and opens the serial monitor.

---

## Step by Step

### 1. Source the environment

```sh
source .esp/export-esp.sh
```

### 2. Build

```sh
cargo build
```

First build takes a while (~5–15 min) as it compiles ESP-IDF and all dependencies. Subsequent builds are incremental and much faster.

### 3. Flash

Connect your ESP32 via USB, then:

```sh
cargo run
```

Under the hood this runs `espflash flash --monitor` (configured in `.cargo/config.toml`).

If you want to flash without monitoring:

```sh
espflash flash target/xtensa-esp32-espidf/debug/openbarista
```

### 4. Monitor

If you flashed without `--monitor`, you can open the serial console separately:

```sh
espflash monitor
```

You should see output like:

```text
[main] Connectivity ready at http://192.168.1.42
[temp] Applied calibration offset: 0.000 C
```

---

## Release Build

For a smaller, faster binary:

```sh
cargo build --release
cargo run --release
```

---

## Build Metadata

`build.rs` exports `OPENBARISTA_BUILD_ID` using the git short SHA + epoch timestamp. This shows up in the web UI so you can tell which build is running on the device.

---

## Host-Side Tests

Math and telemetry logic can be tested on your development machine without hardware:

```sh
cargo +stable test --lib --target x86_64-unknown-linux-gnu
```

This runs the unit tests in `telemetry_math.rs` and other host-compatible modules.

---

## Headless UI Testing

You can run the station web UI locally with mock data — no ESP32 needed:

```sh
python3 scripts/headless_ui.py --port 4173
```

Then open `http://127.0.0.1:4173` in a browser. The mock server provides fake telemetry, scale, and settings responses so you can exercise the full UI.

Options:

```sh
# Bind to all interfaces
python3 scripts/headless_ui.py --host 0.0.0.0 --public-host 127.0.0.1 --port 4173

# Custom build/board identifiers
python3 scripts/headless_ui.py --build-id local-dev --board-id MOCK-BENCH
```

---

## Troubleshooting

**`error[E0463]: can't find crate for 'std'`**
You're building for the wrong target. Make sure you've sourced `.esp/export-esp.sh` and aren't overriding the target.

**`espflash: No serial ports detected`**
- Is the ESP32 plugged in via USB?
- On Linux, check `ls /dev/ttyUSB*` or `/dev/ttyACM*`
- You may need `dialout` group permissions (see [setup]({{ site.baseurl }}/setup/))

**`Connecting... Timeout`**
Hold the **BOOT** button on the ESP32 while espflash is trying to connect, then release after it starts uploading.

**Build is very slow**
First build compiles all of ESP-IDF from source — this is normal. Use `cargo build` (not `clean` + build) for incremental rebuilds.
