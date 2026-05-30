---
layout: default
title: Web Dashboard
nav_order: 6
---

# Web Dashboard

OpenBarista serves a live web UI directly from the ESP32. No app install, no cloud account — just open a browser.

---

## Accessing the Dashboard

Once the ESP32 is connected to your Wi-Fi network, open:

- **`http://<device-ip>`** — shown in the serial monitor at boot
- **`http://openbarista.local`** — if your network supports mDNS

You'll see the brew dashboard with live-updating metrics.

---

## Dashboard Overview

The main dashboard shows:

### Key Metrics (top row)

| Card | Value | Source |
|---|---|---|
| **Pressure** | Current brew pressure in bar | Analog transducer on ADC |
| **Boiler Temp** | Current temperature in °C | PT100 via MAX31865 |
| **Extraction Weight** | Live weight in grams | BLE scale (if paired) |

### Live Recording Indicator

When a shot is in progress a recording badge appears at the top of the dashboard with a running timer. Shots are detected automatically when pressure ≥ 0.5 bar and temperature ≥ 70 °C, or can be started manually.

### Live Profile Chart

A real-time chart showing pressure, flow, and weight over time. You can configure:

- **Profile** — select a target pressure profile
- **Window** — chart time window (20s, 40s, 60s, 120s)

### Secondary Stats

- **PSI** — pressure in PSI
- **Flow** — estimated flow rate (g/s, derived from weight)
- **Peak Bar** — highest pressure seen during the current extraction
- **Avg Bar** — average pressure during extraction

### Scale Status

Shows whether a BLE scale is connected and syncing weight data.

### Shot Timer

A running timer for the current extraction, with a **START EXTRACTION** button to begin tracking.

---

## Shot History

Accessible at `http://<device-ip>/history` or via the **History** nav link.

The history page shows all recorded shots stored on the device (up to 10; oldest is overwritten when full).

### List View

- List of past shots with timestamp, duration, max pressure, yield, and avg temperature
- Summary analytics across all shots (avg duration, avg yield, avg max pressure, avg temp)

### Detail View

Tap any shot to open a detail view with:

- A full pressure / flow / weight chart for the entire shot
- Per-shot metrics matching the list view
- **Replay** — re-runs the chart animation at real speed
- **Delete** — removes the shot from NVS

### Shot Detection

Shots are recorded automatically. Detection criteria:

- Pressure ≥ **0.5 bar**
- Temperature ≥ **70 °C**

3 seconds of pre-shot data are prepended to every recording so the ramp-up is captured. End-of-shot is debounced over 2 seconds to avoid false stops.

Shots can also be started and stopped manually from the main dashboard.

---

## Wi-Fi Provisioning

If the ESP32 has no saved Wi-Fi credentials (first boot), or if it can't connect:

1. The device starts an open access point: **`OpenBarista`**
2. Connect to that network from your phone or laptop
3. A captive portal opens at `http://192.168.4.1`
4. Select your Wi-Fi network and enter the password
5. The device validates, saves credentials to NVS, and reboots
6. After reboot it connects to your network and serves the dashboard

---

## Settings Page

Accessible at `http://<device-ip>/settings` or via the ⚙ Settings link.

Settings you can change:

- **Device label** — a friendly name for your machine
- **Temperature offset** — calibration offset in °C (added to raw readings)
- **Wi-Fi credentials** — update SSID/password (triggers a reboot)
- **Bluetooth scale** — scan, pair, disconnect, or forget a BLE scale

Settings are persisted in ESP NVS and survive reboots.

---

## API Endpoints

The ESP32 exposes a JSON API for programmatic access:

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/health` | Returns `ok` — use for connectivity checks |
| `GET` | `/api/telemetry` | Latest telemetry snapshot (temp, pressure, weight, flow) |
| `GET` | `/api/scale` | Scale status, saved pairing, discovered devices |
| `GET` | `/api/settings` | Current device settings |
| `GET` | `/api/shots` | List of shot summaries, newest first |
| `GET` | `/api/shot?id=N` | Full point data for a single shot |
| `POST` | `/api/scale` | Scale actions: scan, connect, disconnect, forget |
| `POST` | `/api/settings` | Update settings (label, temp offset, Wi-Fi) |
| `POST` | `/api/shots` | Shot actions: `start`, `save`, `delete` |
| `GET` | `/networks` | Available Wi-Fi networks |

Example:

```sh
curl http://openbarista.local/api/telemetry
```

```json
{
  "temperature_c": 93.42,
  "pressure_bar": 8.74,
  "pressure_psi": 126.79,
  "weight_g": 36.2,
  "flow_gps": 2.1
}
```

---

## Security Notes

- The web server only serves allowlisted paths — no directory traversal
- Security headers are set on all responses
- The provisioning portal validates SSID (≤32 chars) and password (≤64 chars) before saving
- There is no authentication on the API — the device should be on a trusted local network
