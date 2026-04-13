---
layout: default
title: Bluetooth Scale
nav_order: 7
---

# Bluetooth Scale

OpenBarista can pair with a BLE (Bluetooth Low Energy) coffee scale for live extraction weight and flow rate.

---

## What You Need

Any BLE-compatible coffee scale that exposes weight data via standard GATT characteristics. This is a generic BLE implementation — it doesn't target a specific brand.

**Known to work:** Generic BLE coffee/espresso scales that advertise a weight measurement characteristic.

**May not work:** Scales that use proprietary app-only protocols or don't expose weight via standard BLE services.

---

## Pairing a Scale

1. Open the **Settings** page (`http://<device-ip>/settings`)
2. In the Bluetooth Scale section, tap **Find Scales**
3. The ESP32 scans for nearby BLE devices (~10 seconds)
4. Discovered scales appear in a list — tap the one you want
5. The firmware connects and begins reading weight data
6. The scale is saved automatically — it will reconnect on next boot

Only **one scale** can be saved at a time.

---

## What You See on the Dashboard

Once paired:

- **Extraction Weight** card shows live weight in grams
- **Flow** stat shows estimated flow rate in g/s
- **Scale Sync** status shows the connected scale name
- The **Live Profile** chart includes weight and flow traces

Weight updates come in as fast as the scale sends them (typically every 100–200 ms). Flow rate is calculated using an exponential moving average of weight change over time.

---

## Managing the Scale

From the Settings page:

| Action | What it does |
|---|---|
| **Find Scales** | Starts a BLE scan for nearby devices |
| **Connect** | Pairs with the selected scale |
| **Disconnect** | Drops the active BLE connection |
| **Forget** | Removes the saved scale from NVS — stops auto-reconnect |

---

## Auto-Reconnect

On boot, if a scale was previously saved, the firmware automatically attempts to reconnect. If the scale is off or out of range, the dashboard works normally without weight data — it just shows `--` for weight and flow.

---

## How It Works Internally

- BLE stack: **NimBLE** (not Bluedroid) via the `esp32-nimble` crate
- The firmware runs a dedicated BLE worker thread with a 32 KB stack
- Scan/connect operations include timeouts and cleanup to avoid hanging
- A watchdog races against `connect()` — if the scale disconnects mid-handshake, the watchdog fires after 12 seconds and cleans up
- Weight values are sanitized (clamped to ≥ 0, NaN/Inf → 0) before reaching telemetry

---

## Troubleshooting

**Scale not found during scan**
- Make sure the scale is powered on and in pairing/discoverable mode
- Move the scale closer to the ESP32 (BLE range is ~10 m in open air, less through walls)
- Some scales go to sleep quickly — wake it up right before scanning

**Scale connects but no weight data**
- The scale may use a non-standard BLE protocol
- Check the serial monitor for BLE characteristic discovery logs

**Scale disconnects frequently**
- Low battery on the scale
- BLE interference from other devices (Wi-Fi and BLE share the 2.4 GHz band)
- Keep the ESP32 and scale within a few meters of each other

**"BLE runtime unavailable" in serial output**
- Bluetooth initialization failed. Check that `CONFIG_BT_NIMBLE_ENABLED=y` is set in `sdkconfig.defaults` and rebuild.
