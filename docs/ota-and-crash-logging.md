---
layout: default
title: OTA Updates & Crash Logging
nav_order: 8
---

# OTA Updates & Crash Logging

How to flash new firmware over Wi-Fi and diagnose crashes without pulling the
espresso machine out from under the counter.

This page covers three cooperating systems:

1. **OTA updates** — stream a new firmware image into the inactive flash slot
   over HTTP, validate it, and reboot into it.
2. **Crash logging** — persist panic messages, reset reasons, recent log lines
   and core dumps so a crash that reboots the device still leaves evidence.
3. **Health watchdog** — if the firmware hangs instead of panicking, the
   device restarts itself and logs the stall.

---

## How it fits together

```
             ┌──────────────────── 4 MB flash ────────────────────┐
 bootloader   nvs      otadata   ota_0      ota_1   coredump crashlog
 (0x1000)    (0x9000) (0xf000)  (0x20000)  (0x1F0000) (0x3C0000) (0x3F0000)
             creds,   boot       running    OTA       panic     event
             shots,   selection  firmware   target    register  ring
             settings                       slot      dump (ELF)
             └── unchanged by OTA ──┘  └─ crashlog is its own NVS partition ─┘
```

- An OTA upload is written to the **inactive** slot only. The running image is
  never touched, so a failed/interrupted upload cannot corrupt the running
  firmware.
- `nvs` keeps its offset from the old single-app partition table, so Wi-Fi
  credentials, settings and shot history survive a reflash.
- The crash/event log gets its own small NVS partition (`crashlog`, 64 KB at
  `0x3F0000`). Keeping it out of the default `nvs` partition means the ring's
  constant rewrites can never exhaust or fragment the storage holding Wi-Fi
  credentials, settings and shots. If the partition is missing (old table),
  the log degrades to RAM-only instead of failing.
- After a successful upload the device reboots into the new slot. The new
  image boots in *pending verify* state and has ~30 s to confirm itself
  healthy (`src/health.rs`). If it crashes, hangs or reboots before that, the
  **bootloader rolls back** to the previous slot automatically. A bad upload
  can't brick the machine.
- On panic, ESP-IDF prints the usual backtrace and (before rebooting) writes
  a core dump to the `coredump` partition. A Rust panic hook additionally
  persists the panic message to NVS (see below).

---

## OTA: flashing over Wi-Fi (dev builds)

OTA upload endpoints are compiled **only** when the firmware is built with
`CFG_OTA_ENABLED=1`. Never enable this on a production machine — the upload
page has no authentication.

### One-time setup

The two-slot partition table must be installed once over USB (you cannot OTA
from the old single-app layout):

```sh
CFG_OTA_ENABLED=1 cargo run
```

This flashes the new bootloader, partition table and app in one go. Wi-Fi
credentials survive because NVS keeps its old offset.

### Pushing a new firmware

```sh
# 1. Build the image (a release build fits comfortably in the 1.9 MB slot)
CFG_OTA_ENABLED=1 cargo build --release
espflash save-image --chip esp32 \
  target/xtensa-esp32-espidf/release/openbarista firmware.bin

# 2. Open http://<device-ip>/upload and select firmware.bin
```

Notes:

- The image is **streamed** chunk-by-chunk into flash — the device never
  holds more than a few KB in RAM.
- ESP-IDF validates the image (magic byte, segment layout, checksum) before
  switching slots. A truncated or wrong-chip image is rejected, not flashed.
- Flash erase takes 30–60 seconds and the upload request doesn't respond
  until it's done — that's normal. Leave the machine powered on.
- The hang watchdog is paused during flashing so the erase-induced stall
  doesn't trigger a reboot.

### Rollback behaviour

| Scenario | Result |
| -------- | ------ |
| Upload completes, image valid, device healthy after 30 s | Runs new firmware, slot marked valid |
| Upload interrupted / image invalid | Rejected; running firmware keeps running |
| New image crashes or reboots within first ~30 s | Bootloader rolls back to previous slot |
| New image hangs within first ~30 s | Hang watchdog reboots it → rollback on next boot |

To deliberately roll back (dev), simply power-cycle the device within 30 s of
an OTA reboot.

### USB flashing note

`cargo run` now passes `--partition-table partitions_two_ota.csv
--erase-parts otadata` to espflash, so a USB flash always lands in `ota_0`
and resets boot selection. If you previously flashed with the default
single-app table, expect a one-time full reflash.

---

## Crash logging

### What gets persisted

**NVS event ring** (`src/crash_log.rs`, dedicated `crashlog` NVS partition at
0x3F0000, namespace `crashlog`, 64 entries × 112 bytes, oldest overwritten):

- One `boot #N reset=<reason> fw=<build>` line per boot, with the ESP-IDF
  reset reason (`power-on`, `software`, `panic`, `brownout`, `wdt`, …)
- Rust **panic messages** — a panic hook writes `PANIC thread=…: msg at
  file:line` and flushes to NVS before the ESP-IDF panic handler reboots
- All `log` crate output at INFO level and above (Wi-Fi/BLE events, OTA
  progress, health events), mirrored by a tee logger
- OTA events (upload started, flashed N bytes, validation failures)

The ring is flushed to NVS every 10 s and on panic, so at most a few seconds
of context are lost on a hard crash.

**Core dumps** (`coredump` partition, ELF format): full register and task
state of every panic, written by ESP-IDF before reboot.

### Fetching the evidence

| Endpoint | Purpose |
| -------- | ------- |
| `GET /api/logs` | JSON: boot count, reset reason, coredump size, event ring |
| `GET /api/coredump` | Raw ELF core dump (404 if none stored) |
| `POST /api/coredump/erase` | Erase the stored core dump |
| `GET /api/diagnostics` | Reset reason, heap stats, uptime |

Example:

```sh
curl http://openbarista.local/api/logs | python3 -m json.tool
curl -o coredump.elf http://openbarista.local/api/coredump
```

### Analyzing a core dump

The dump is an ELF core file. To decode it you need the ELF from the build
that crashed and ESP-IDF's `espcoredump` tool:

```sh
# Inside an ESP-IDF environment (source export.sh):
espcoredump.py info_core_dump -t b64 -c "$(base64 -w0 coredump.elf)" \
    -r target/xtensa-esp32-espidf/release/openbarista

# Or open it in gdb directly:
xtensa-esp-elf-gdb target/xtensa-esp32-espidf/release/openbarista -ex \
  "target remote | espcoredump.py gdb_core_dump -t b64 -c $(base64 -w0 coredump.elf)"
```

Keep the `.elf` from the exact build (`target/.../release/openbarista`) if you
want symbolized backtraces — the raw `firmware.bin` alone is not enough.

### Hidden developer panel in the UI

The settings page has a crude log viewer, deliberately hidden:

1. Open **Settings**
2. Click the **Build ID** five times (within ~2 s per click window)
3. A "Device Logs" panel appears with the event ring, core dump
   download/erase buttons, and — on dev builds — a **Test panic** button that
   crashes the firmware on purpose so you can verify the whole
   panic → persist → reboot → inspect loop end-to-end

The headless mock server (`python3 scripts/headless_ui.py`) serves mock
`/api/logs`, `/api/coredump` and `/api/test-panic` responses, so the panel
can be exercised without hardware.

---

## Health watchdog

ESP-IDF already reboots on panic (`CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT`).
But a **hang** (deadlocked mutex, stuck driver) never panics — it just stops,
which is how you end up unplugging the machine.

`src/health.rs` adds a second line of defence:

- The main sensor loop feeds a heartbeat every iteration (~50 ms).
- A monitor thread checks it every 5 s. If it's been silent for more than
  60 s — and no OTA flash is in progress — the device records
  `health: main loop stalled for Ns, restarting` to the crash log and
  reboots itself.
- The same module confirms the OTA slot as valid once the firmware has been
  alive for 30 s with a live heartbeat (the rollback mechanism above).
- Known limitation: while paused for an OTA flash, a hang *inside* the flash
  driver itself (e.g. corrupted flash) is not caught — the upload client just
  times out and the machine keeps running whatever was there before.

---

## Configuration reference

| File | Setting | Effect |
| ---- | ------- | ------ |
| `partitions_two_ota.csv` | layout | 2 × 0x1D0000 app slots, NVS @ 0x9000, coredump @ 0x3C0000, dedicated `crashlog` NVS @ 0x3F0000 (4 MB flash) |
| `sdkconfig.defaults` | `CONFIG_PARTITION_TABLE_CUSTOM` | use the custom table |
| `sdkconfig.defaults` | `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` | rollback on unconfirmed images |
| `sdkconfig.defaults` | `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH` | persist core dumps |
| `sdkconfig.defaults` | `CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT` | reboot on panic |
| `build.rs` | `CFG_OTA_ENABLED` env var | compiles `/upload`, `/api/firmware-upload`, `/api/test-panic` |

Source files: `src/ota_flash.rs`, `src/crash_log.rs`, `src/health.rs`.

---

## Troubleshooting

**OTA upload returns "No inactive OTA app partition found"**
The running firmware was flashed with an old (single-app) partition table.
Do a one-time `CFG_OTA_ENABLED=1 cargo run` over USB.

**OTA upload returns "Firmware image validation failed"**
The uploaded `.bin` is truncated, built for the wrong chip, or not an
app image. Rebuild with `espflash save-image --chip esp32`. Note that
`espflash save-image` takes the **ELF** as input, not a previous `.bin`.

**The device keeps rolling back after an OTA update**
Your new firmware isn't reaching the 30 s healthy mark. Check
`GET /api/logs` — if it panics early, the panic line will be there. If the
log shows nothing after `boot #N`, the image is dying before the crash log
initializes (or hanging — the watchdog log line would appear on the *next*
boot's log).

**`/api/logs` shows fewer entries than expected**
The ring keeps the most recent 64 lines; entries are overwritten in order.
Panics and boots are never dropped in favor of nothing — but a chatty boot
can push older lines out.

**Upload page loads but flashes nothing / browser hangs**
Flash erase takes 30–60 s before the HTTP response is sent. If you get
impatient and close the tab mid-upload, the inactive slot is simply left
invalid — nothing breaks, just retry.
