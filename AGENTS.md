# OpenBarista — ESP-IDF Rust firmware

## Commands
- Build: `cargo build` (default target: `xtensa-esp32-espidf`)
- Flash+monitor: `cargo run` (runner: `espflash flash --monitor --partition-table partitions_two_ota.csv --erase-parts otadata`)
- Source env before Cargo: `source .esp/export-esp.sh` (needed for LLVM/libclang; rerun `bash scripts/bootstrap.sh` if missing)
- Host lib tests: `cargo test --lib --target $(rustc -vV | awk '/host:/ {print $2}')`
- Headless UI (no hardware): `python3 scripts/headless_ui.py --port 4173`
- OTA image: `CFG_OTA_ENABLED=1 cargo build --release && espflash save-image --chip esp32 target/xtensa-esp32-espidf/release/openbarista firmware.bin`

## Environment
- Toolchain: `esp` (rust-toolchain.toml)
- `.cargo/config.toml`: target `xtensa-esp32-espidf`, linker `ldproxy`, `ESP_IDF_COMPONENT_MANAGER=true`
- `build-std = ["std", "panic_abort"]` in `[unstable]`
- ESP-IDF v5.4.3 + managed mDNS (espressif/mdns via main/idf_component.yml)
- Profiles: dev `opt-level = "z"` (slow compile — debug builds are size-optimized), release `opt-level = "s"`
- Dependency pin: `embedded-svc = "=0.29.0"` to match what esp-idf-svc 0.52 pulls in

## Hardware & Sensors
- **Temperature**: PT100 RTD via MAX31865 (SPI). CS:5, SCLK:18, MOSI:23, MISO:19.
- **Pressure**: Analog transducer on ADC1 (GPIO34) with 12dB attenuation.
- **Conversion**: `raw / 4095.0 * 3.3` -> `(V - 0.35V) / (4.5V - 0.35V) * 250 PSI` (1 PSI = 0.0689476 bar).
- **Gotcha**: Wi-Fi credential updates trigger a device reboot.

## UI & Mocking
- **Mock Server**: `python3 scripts/headless_ui.py --port 4173`
- Provides mock `/api/telemetry`, `/api/scale`, `/api/settings`, `/networks`, and `/health` endpoints.
- Useful for testing UI flows without physical hardware.

## OTA Upload (dev only)
`CFG_OTA_ENABLED=1` enables `GET /upload`, `POST /api/firmware-upload` and `POST /api/test-panic`. The firmware is streamed into the *inactive* OTA slot (two-slot table in `partitions_two_ota.csv`, 4 MB flash required), validated by ESP-IDF, then the device reboots into it. NVS (credentials, settings, shots, logs) is preserved. OTA rollback is enabled: if the new image doesn't confirm itself healthy within ~30 s of uptime (`src/health.rs`), the bootloader rolls back to the previous slot.

Steps: `espflash save-image` the new build, make sure the currently flashed firmware was built with `CFG_OTA_ENABLED=1`, then visit http://device-ip/upload. Flash erase takes ~30–60 s — don't power off mid-flash.

NOTE: switching to the two-OTA partition table requires **one** final USB flash (`cargo run`); you cannot OTA from an old single-app build.

## Crash Logging & Health
- Panic messages, boot count and reset reasons persist to NVS (`src/crash_log.rs`); panic register/task dumps persist to the `coredump` partition (ELF).
- `GET /api/logs` — persisted event ring; `GET /api/coredump` — raw ELF core dump (feed to `espcoredump`/gdb); `POST /api/coredump/erase` — reclaim it.
- Hidden dev panel on the settings page: click the Build ID 5×.
- Hang watchdog (`src/health.rs`): if the main loop stalls >60 s (except during OTA flash), the device logs it and reboots itself — no more unplugging the machine.
