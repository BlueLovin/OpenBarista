---
layout: default
title: Hardware & Components
nav_order: 2
---

# Hardware & Components

Everything you need to build an OpenBarista telemetry setup.

---

## Bill of Materials

### Core Controller

| Component | Notes | Approx. Cost |
|---|---|---|
| **ESP32 dev board** | Any ESP32-WROOM-32 or ESP32-DevKitC. Must have standard GPIO breakout. | ~$5–10 |

The ESP32 provides Wi-Fi, Bluetooth LE, SPI, and ADC — all used by OpenBarista.

---

### Temperature Sensing

| Component | Notes | Approx. Cost |
|---|---|---|
| **PT100 RTD probe** | 3-wire, rated for espresso-machine temperatures (up to ~200 °C). M4 thread or thermowell mount depending on your machine. | ~$5–15 |
| **MAX31865 RTD interface board** | Adafruit #3328 or generic breakout. Must support 3-wire RTD configuration. Ships with a 430 Ω reference resistor for PT100. | ~$8–15 |

The MAX31865 converts the PT100's resistance into a digital value over SPI. OpenBarista reads the raw RTD code and converts to °C using the Callendar-Van Dusen equation.

**Why PT100 + MAX31865?** Thermocouples are cheaper but noisier. A PT100 with a dedicated ADC gives stable, repeatable readings at espresso temperatures without calibration headaches.

---

### Pressure Sensing

| Component | Notes | Approx. Cost |
|---|---|---|
| **0–200 PSI pressure transducer** | Analog output (0.5–4.5 V typical). 1/8" NPT or BSP thread to match your machine's plumbing. Must be food-safe / rated for hot water. | ~$10–25 |
| **Voltage divider (if needed)** | ESP32 ADC pins are **3.3 V max**. If your transducer outputs above 3.3 V at operating pressure, you need a resistor divider to scale it down. See the [wiring guide]({{ site.baseurl }}/wiring/) for details. | ~$0.50 |

The transducer connects to GPIO34 (ADC1). OpenBarista's conversion model:

- Raw ADC → voltage: `raw / 4095 × 3.3`
- Voltage → PSI: linear scale from 0.35 V (zero) to 4.5 V (200 PSI)
- PSI → bar: `× 0.0689476`

**⚠️ Important:** Never drive an ESP32 ADC pin above 3.3 V. Use a voltage divider or ensure your transducer stays within range at maximum operating pressure.

---

### Bluetooth Scale (Optional)

| Component | Notes | Approx. Cost |
|---|---|---|
| **BLE-compatible coffee scale** | Any scale that exposes a standard BLE weight characteristic. Tested with generic BLE coffee scales. | ~$20–60 |

The scale connects wirelessly over Bluetooth LE. OpenBarista scans, pairs, and reads live weight. Flow rate is calculated from weight change over time.

One saved scale is persisted in NVS and automatically reconnected on boot.

---

### Power & Misc

| Component | Notes |
|---|---|
| **USB cable** | For programming and serial monitor. Micro-USB or USB-C depending on your ESP32 board. |
| **Breadboard / protoboard** | For initial wiring. Solder to perfboard once validated. |
| **Jumper wires** | Standard dupont jumpers for prototyping. |
| **Enclosure** | Optional. Any small project box that fits near your machine. Keep away from steam and direct water contact. |

---

## Where to Source

These are all common components available from:

- **Amazon** / **AliExpress** — ESP32 boards, MAX31865 breakouts, pressure transducers
- **Adafruit** / **SparkFun** — Higher-quality MAX31865 breakouts with documentation
- **Your local espresso parts supplier** — Fittings, T-pieces, food-safe transducers
- **Digi-Key** / **Mouser** — Precision resistors if you're building a voltage divider

---

## Compatibility Notes

- **ESP32 only** — ESP32-S2, S3, C3 are not currently targeted (different architecture).
- **3-wire PT100** — The firmware defaults to 3-wire RTD mode. 2-wire or 4-wire probes need a config change in the source.
- **BLE scales** — Compatibility is best-effort. Scales must expose weight data via standard BLE GATT characteristics. Not all consumer scales do.
