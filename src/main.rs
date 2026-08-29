#[cfg(not(target_arch = "xtensa"))]
compile_error!("OpenBarista firmware must be built for an xtensa target.");

#[cfg(not(target_arch = "xtensa"))]
fn main() {
    println!("OpenBarista firmware binary is only supported on xtensa targets.");
}

mod scale_ble;
mod sensors;
mod web_assets;
mod wifi_provision;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use embedded_hal::spi::MODE_1;
use esp_idf_hal::adc::attenuation::DB_12;
use esp_idf_hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::spi;
use esp_idf_hal::spi::{SpiDeviceDriver, SpiDriver};
use esp_idf_hal::units::FromValueType;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use openbarista::shot_recorder::ShotRecorder;
use openbarista::shot_store::NvsShotStore;
use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use crate::sensors::pressure::PressureSensor;
use crate::sensors::temperature::Max31865;

// Minimum plausible wall-clock timestamp (2020-01-01T00:00:00Z).
// Before SNTP syncs the RTC, SystemTime::now() on the ESP32 returns the
// device uptime in seconds (a few hundred at most), which looks like a date
// in early 1970 in the history UI.  Any timestamp below this sentinel is
// treated as "unsynced" and clamped to 0 so the UI shows "Unknown time"
// rather than a nonsensical 1970 date.
const MIN_PLAUSIBLE_UNIX_TS: u64 = 1_577_836_800; // 2020-01-01

fn get_unix_timestamp() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if ts < MIN_PLAUSIBLE_UNIX_TS { 0 } else { ts }
}

fn main() -> Result<()> {
    // Ensure the ESP-IDF sys crate's patches are linked in, so that the correct
    // symbols are available for the ESP-IDF components we use.
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take()?;
    let (wifi_modem, bluetooth_modem) = peripherals.modem.split();
    let pins = peripherals.pins;
    let nvs_partition = EspDefaultNvsPartition::take()?;

    // Crash log + panic hook must come first: from here on, any panic gets
    // persisted to NVS before the ESP-IDF panic handler reboots the device.
    openbarista::crash_log::init(nvs_partition.clone(), env!("OPENBARISTA_BUILD_ID"));
    openbarista::crash_log::install_panic_hook();
    openbarista::crash_log::init_logger();

    let telemetry = SharedTelemetry::new();

    let shot_store = match NvsShotStore::new(nvs_partition.clone()) {
        Ok(store) => Arc::new(Mutex::new(store)) as openbarista::shot_store::SharedShotStore,
        Err(err) => {
            println!("[shots] Failed to open NVS shot store: {err:#}");
            return Err(err);
        }
    };
    let shot_recorder = Arc::new(Mutex::new(ShotRecorder::new()));

    let scale_runtime = match scale_ble::ScaleRuntime::try_new(
        bluetooth_modem,
        Some(nvs_partition.clone()),
        telemetry.clone(),
    ) {
        Ok(runtime) => Arc::new(runtime),
        Err(err) => {
            println!("[scale] BLE runtime unavailable: {err:?}");
            Arc::new(scale_ble::ScaleRuntime::disabled(format!(
                "Bluetooth scale support is unavailable right now: {err}"
            )))
        }
    };

    // `mut` is needed when the dev-only OTA handlers are registered below.
    #[allow(unused_mut)]
    let mut wifi_runtime = wifi_provision::setup_wifi(
        wifi_modem,
        nvs_partition,
        telemetry.clone(),
        scale_runtime.clone(),
        shot_store.clone(),
        shot_recorder.clone(),
    )?;
    println!(
        "[main] Connectivity ready at http://{}",
        wifi_runtime.ip_addr()
    );

    // Hang watchdog: restarts the device if the main loop ever stalls
    // ("have to unplug it"). Started only after provisioning completes,
    // because setup_wifi legitimately blocks in the captive portal until the
    // user configures credentials.
    openbarista::health::start_monitor();

    #[cfg(ota_enabled)]
    {
        use esp_idf_svc::http::Method;
        use esp_idf_svc::io::Write;

        // Serve the OTA upload page and its assets (the station server does
        // not route the captive-portal static assets by default).
        for path in ["/portal.css", "/upload.css", "/upload.js"] {
            wifi_runtime.station_http_server.fn_handler(
                path,
                Method::Get,
                move |req| {
                    let Some(asset) = web_assets::captive_static(path) else {
                        let hdrs = wifi_provision::response_headers(
                            "text/plain; charset=utf-8",
                            "no-store",
                        );
                        req.into_response(404, Some("Not Found"), &hdrs)?
                            .write_all(b"not found")?;
                        return Ok::<_, anyhow::Error>(());
                    };
                    let hdrs = wifi_provision::station_response_headers(
                        asset.content_type,
                        asset.cache_control,
                    );
                    req.into_response(200, Some("OK"), &hdrs)?
                        .write_all(asset.body)?;
                    Ok::<_, anyhow::Error>(())
                },
            )?;
        }

        wifi_runtime
            .station_http_server
            .fn_handler("/upload", Method::Get, move |req| {
                let html = web_assets::upload_html();
                let hdrs = wifi_provision::station_response_headers(
                    "text/html; charset=utf-8",
                    "no-store",
                );
                req.into_response(200, Some("OK"), &hdrs)?
                    .write_all(html.as_bytes())?;
                Ok::<_, anyhow::Error>(())
            })?;

        // Handle firmware upload: stream body chunks straight into the
        // inactive OTA slot (never buffering the whole image in RAM), let
        // ESP-IDF validate it, switch slots and reboot. NVS is untouched.
        wifi_runtime
            .station_http_server
            .fn_handler("/api/firmware-upload", Method::Post, move |mut req| {
                use embedded_svc::http::Headers as _;
                use openbarista::ota_flash::{OtaWriter, WriteError};

                // Flash erase stalls the main loop for tens of seconds; the
                // hang monitor must not fire during that window.
                openbarista::health::pause_hang_monitor();
                openbarista::crash_log::record("ota: firmware upload started");
                openbarista::crash_log::flush();

                let expected_size = req
                    .content_len()
                    .unwrap_or(0)
                    .min(openbarista::ota_flash::FIRMWARE_MAX as u64)
                    as usize;
                let mut writer = match OtaWriter::begin(expected_size) {
                    Ok(writer) => writer,
                    Err(err) => {
                        openbarista::health::resume_hang_monitor();
                        openbarista::crash_log::record(&format!("ota: begin failed: {err:#}"));
                        openbarista::crash_log::flush();
                        let hdrs = wifi_provision::response_headers(
                            "application/json; charset=utf-8",
                            "no-store",
                        );
                        let payload = format!(
                            "{{\"ok\":false,\"error\":\"{}\"}}",
                            json_escape(&err.to_string())
                        );
                        req.into_response(400, Some("Bad Request"), &hdrs)?
                            .write_all(payload.as_bytes())?;
                        return Ok::<_, anyhow::Error>(());
                    }
                };

                let mut buf = [0u8; 2048];
                loop {
                    let n = req
                        .read(&mut buf)
                        .map_err(|e| anyhow::anyhow!("Failed to read upload body: {e:?}"));
                    let n = match n {
                        Ok(n) => n,
                        Err(err) => {
                            writer.abort();
                            openbarista::health::resume_hang_monitor();
                            openbarista::crash_log::record(&format!(
                                "ota: body read failed: {err:#}"
                            ));
                            openbarista::crash_log::flush();
                            return Err(err);
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    match writer.write(&buf[..n]) {
                        Ok(()) => {}
                        Err(WriteError::TooLarge) => {
                            openbarista::health::resume_hang_monitor();
                            openbarista::crash_log::record("ota: rejected, firmware too large");
                            openbarista::crash_log::flush();
                            // Drain the rest of the body before responding, so
                            // the client sees the 413 JSON instead of a reset
                            // connection while it is still streaming.
                            drain_body(&mut req);
                            let hdrs = wifi_provision::response_headers(
                                "application/json; charset=utf-8",
                                "no-store",
                            );
                            req.into_response(413, Some("Payload Too Large"), &hdrs)?
                                .write_all(b"{\"ok\":false,\"error\":\"Firmware too large\"}")?;
                            return Ok::<_, anyhow::Error>(());
                        }
                        Err(WriteError::Flash(err)) => {
                            openbarista::health::resume_hang_monitor();
                            openbarista::crash_log::record(&format!("ota: flash write failed: {err}"));
                            openbarista::crash_log::flush();
                            drain_body(&mut req);
                            let hdrs = wifi_provision::response_headers(
                                "application/json; charset=utf-8",
                                "no-store",
                            );
                            let payload = format!(
                                "{{\"ok\":false,\"error\":\"{}\"}}",
                                json_escape(&err)
                            );
                            req.into_response(500, Some("Internal Server Error"), &hdrs)?
                                .write_all(payload.as_bytes())?;
                            return Ok::<_, anyhow::Error>(());
                        }
                    }
                }

                match writer.finish() {
                    Ok(bytes) => {
                        openbarista::crash_log::record(&format!(
                            "ota: flashed {bytes} bytes, rebooting into new slot"
                        ));
                        openbarista::crash_log::flush();
                        let hdrs = wifi_provision::response_headers(
                            "application/json; charset=utf-8",
                            "no-store",
                        );
                        req.into_response(200, Some("OK"), &hdrs)?
                            .write_all(
                                b"{\"ok\":true,\"message\":\"Firmware flashed. Rebooting...\"}",
                            )?;
                        println!("[ota] Flashed {bytes} bytes, rebooting into new slot");
                        // Spawn a thread so the response has time to flush.
                        // The hang monitor stays paused: we are rebooting by choice.
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_millis(2000));
                            unsafe { esp_idf_svc::sys::esp_restart() };
                        });
                    }
                    Err(err) => {
                        // finish() consumed the writer; the update was validated
                        // and rejected, so no boot switch happened. Resume hang
                        // detection and keep running the current firmware.
                        openbarista::health::resume_hang_monitor();
                        openbarista::crash_log::record(&format!("ota: validation failed: {err:#}"));
                        openbarista::crash_log::flush();
                        let hdrs = wifi_provision::response_headers(
                            "application/json; charset=utf-8",
                            "no-store",
                        );
                        let payload = format!(
                            "{{\"ok\":false,\"error\":\"{}\"}}",
                            json_escape(&err.to_string())
                        );
                        req.into_response(400, Some("Bad Request"), &hdrs)?
                            .write_all(payload.as_bytes())?;
                    }
                }

                Ok::<_, anyhow::Error>(())
            })?;

        // Dev-only: crash the device on demand to verify the panic log and
        // automatic reboot end-to-end. Inspect afterwards via GET /api/logs.
        wifi_runtime
            .station_http_server
            .fn_handler("/api/test-panic", Method::Post, move |mut req| {
                let mut sink = [0u8; 256];
                while req.read(&mut sink).map_err(|e| anyhow::anyhow!("{e:?}"))? != 0 {}
                let hdrs = wifi_provision::response_headers(
                    "application/json; charset=utf-8",
                    "no-store",
                );
                req.into_response(200, Some("OK"), &hdrs)?.write_all(
                    b"{\"ok\":true,\"message\":\"Panicking now; the device will reboot and log it.\"}",
                )?;
                openbarista::crash_log::record("manual panic test requested via /api/test-panic");
                openbarista::crash_log::flush();
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    panic!("manual test panic via /api/test-panic");
                });
                Ok::<_, anyhow::Error>(())
            })?;
    }

    let temp_sensor_bus = SpiDriver::new::<spi::SPI2>(
        peripherals.spi2,
        pins.gpio18,
        pins.gpio23,
        Some(pins.gpio19),
        &spi::config::DriverConfig::new(),
    )?;

    let temp_sensor_spi_config = spi::config::Config::new()
        .baudrate(1.MHz().into())
        .data_mode(MODE_1);

    let temp_sensor_device =
        SpiDeviceDriver::new(temp_sensor_bus, Some(pins.gpio5), &temp_sensor_spi_config)?;
    let mut temperature_sensor = Max31865::new(temp_sensor_device)?;

    let pressure_sensor_adc = AdcDriver::new(peripherals.adc1)?;
    let pressure_sensor_adc_config = AdcChannelConfig {
        attenuation: DB_12,
        ..Default::default()
    };
    let pressure_sensor_channel = AdcChannelDriver::new(
        &pressure_sensor_adc,
        pins.gpio34,
        &pressure_sensor_adc_config,
    )?;
    let mut pressure_sensor = PressureSensor::new(pressure_sensor_channel);

    let mut applied_temperature_offset_c = wifi_runtime.temperature_offset_c();
    temperature_sensor.set_calibration_offset_c(applied_temperature_offset_c);
    println!("[temp] Applied calibration offset: {applied_temperature_offset_c:.3} C");

    loop {
        let configured_temperature_offset_c = wifi_runtime.temperature_offset_c();
        if (configured_temperature_offset_c - applied_temperature_offset_c).abs() > 1e-6 {
            temperature_sensor.set_calibration_offset_c(configured_temperature_offset_c);
            applied_temperature_offset_c = configured_temperature_offset_c;
            println!("[temp] Applied calibration offset: {configured_temperature_offset_c:.3} C");
        }

        let temperature = temperature_sensor.read_temperature_c()?;
        let pressure = pressure_sensor.read()?;

        telemetry.update(temperature.temperature_c, pressure.bar, pressure.psi);

        let snapshot = telemetry.snapshot();
        let unix_ts = get_unix_timestamp();

        if let Some(shot) = lock_or_recover(&shot_recorder).update(&snapshot, unix_ts) {
            if let Err(e) = lock_or_recover(&shot_store).save(shot) {
                println!("[shots] Failed to save shot: {e:#}");
            }
        }
        telemetry.update_recording_active(lock_or_recover(&shot_recorder).is_active());

        // Heartbeat + persisted log: the health monitor restarts the device if
        // this loop ever stalls for HANG_TIMEOUT; the crash log is flushed to
        // NVS every ~10 s so a hard crash leaves a trace behind.
        openbarista::health::feed();
        openbarista::health::confirm_running_slot_valid();
        openbarista::crash_log::periodic_flush();

        FreeRtos::delay_ms(50);
    }
}

/// Reads and discards the remaining request body so the connection can be
/// finished cleanly and the client actually receives the error response we
/// send instead of a connection reset mid-upload.
#[cfg(ota_enabled)]
fn drain_body(req: &mut impl esp_idf_svc::io::Read) {
    let mut sink = [0u8; 2048];
    loop {
        match req.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// Escape a string for safe inclusion in a JSON string value.
pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
