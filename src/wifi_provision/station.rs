use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};

use openbarista::sync_utils::lock_or_recover;

use crate::scale_ble::{SavedScale, ScaleRuntime};
use crate::web_assets;

use super::http::*;
use super::nvs::*;
use super::ConnectProgress;

pub(super) fn start_station_http_server(
    ip_addr: &str,
    telemetry: openbarista::telemetry_feed::SharedTelemetry,
    nvs_partition: EspDefaultNvsPartition,
    _wifi: Arc<Mutex<esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>>>,
    temperature_offset_c: Arc<Mutex<f32>>,
    scale_runtime: Arc<ScaleRuntime>,
) -> Result<EspHttpServer<'static>> {
    let saved_scale = read_saved_scale(&nvs_partition)?;
    scale_runtime.apply_saved_scale(saved_scale);
    if let Err(err) = scale_runtime.connect_saved_scale() {
        println!("[scale] startup connect failed: {err:#}");
    }

    let build_id_value = build_id().to_owned();
    let board_id_value = board_id();
    let html = web_assets::station_index_html(ip_addr, &build_id_value, &board_id_value);
    let settings_html = web_assets::settings_index_html(ip_addr, &build_id_value, &board_id_value);
    let mut server = EspHttpServer::new(&HttpConfig::default())?;

    server.fn_handler("/", Method::Get, move |req| {
        let headers = station_response_headers("text/html; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(html.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/settings", Method::Get, move |req| {
        let headers = station_response_headers("text/html; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(settings_html.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let nvs_for_networks = nvs_partition.clone();
    server.fn_handler("/networks", Method::Get, move |req| {
        let settings = read_device_settings(&nvs_for_networks)?;
        let networks = if settings.ssid.is_empty() {
            Vec::new()
        } else {
            vec![settings.ssid]
        };
        let payload = networks_json(&networks);
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let static_routes: [(&str, fn() -> web_assets::StaticAsset); 7] = [
        ("/base.css", web_assets::base_css),
        ("/station.css", web_assets::station_css),
        ("/station.js", web_assets::station_js),
        ("/settings.css", web_assets::settings_css),
        ("/settings.js", web_assets::settings_js),
        ("/uplot.min.js", web_assets::uplot_js),
        ("/uplot.min.css", web_assets::uplot_css),
    ];

    for (path, asset_fn) in static_routes {
        server.fn_handler(path, Method::Get, move |req| {
            let asset = asset_fn();
            let headers = station_response_headers(asset.content_type, asset.cache_control);
            req.into_response(200, Some("OK"), &headers)?
                .write_all(asset.body)?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    let telemetry_for_handler = telemetry.clone();
    server.fn_handler("/api/telemetry", Method::Get, move |req| {
        let snapshot = telemetry_for_handler.snapshot();
        let payload = telemetry_json(
            snapshot.seq,
            snapshot.temperature_c,
            snapshot.pressure_bar,
            snapshot.pressure_psi,
            snapshot.scale_connected,
            snapshot.weight_g,
            snapshot.flow_gps,
        );
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let scale_for_get = scale_runtime.clone();
    server.fn_handler("/api/scale", Method::Get, move |req| {
        let payload = scale_status_json(&scale_for_get.snapshot());
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let nvs_for_scale_post = nvs_partition.clone();
    let scale_for_post = scale_runtime.clone();
    server.fn_handler("/api/scale", Method::Post, move |mut req| {
        let body_str = match read_request_body_utf8(&mut req, 256) {
            Ok(body) => body,
            Err(RequestBodyError::TooLarge) => {
                let payload = action_result_json(false, "Request body too large.");
                let headers = response_headers("application/json; charset=utf-8", "no-store");
                req.into_response(413, Some("Payload Too Large"), &headers)?
                    .write_all(payload.as_bytes())?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::InvalidUtf8) => {
                let payload = action_result_json(false, "Request body must be valid UTF-8.");
                let headers = response_headers("application/json; charset=utf-8", "no-store");
                req.into_response(400, Some("Bad Request"), &headers)?
                    .write_all(payload.as_bytes())?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::Io(err)) => return Err(err),
        };

        let action = parse_form_field(&body_str, "action")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let address = parse_form_field(&body_str, "address")
            .unwrap_or_default()
            .trim()
            .to_owned();

        let result = match action.as_str() {
            "scan" => scale_for_post.start_scan().map(str::to_owned),
            "connect" => {
                let snapshot = scale_for_post.snapshot();
                let saved_scale = snapshot
                    .devices
                    .iter()
                    .find(|device| device.address.eq_ignore_ascii_case(&address))
                    .map(|device| SavedScale {
                        address: device.address.clone(),
                        name: device.name.clone(),
                        addr_type: device.address_type.clone(),
                    })
                    .or_else(|| {
                        snapshot.saved_scale.and_then(|saved_scale| {
                            if address.is_empty()
                                || saved_scale.address.eq_ignore_ascii_case(&address)
                            {
                                Some(saved_scale)
                            } else {
                                None
                            }
                        })
                    });

                let saved_scale = saved_scale
                    .ok_or_else(|| anyhow!("Scan first, then tap a device from the list."))?;
                save_saved_scale(&nvs_for_scale_post, &saved_scale)?;
                scale_for_post.apply_saved_scale(Some(saved_scale.clone()));
                scale_for_post.connect_address(&saved_scale.address)
            }
            "disconnect" => scale_for_post.disconnect().map(str::to_owned),
            "forget" => {
                clear_saved_scale(&nvs_for_scale_post)?;
                scale_for_post.forget_saved_scale();
                let _ = scale_for_post.disconnect();
                Ok("Saved scale forgotten.".to_owned())
            }
            _ => Err(anyhow!("Unsupported scale action.")),
        };

        let (status_code, reason_phrase, payload) = match result {
            Ok(message) => (200, Some("OK"), action_result_json(true, &message)),
            Err(err) => (400, None, action_result_json(false, &err.to_string())),
        };

        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(status_code, reason_phrase, &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/health", Method::Get, |req| {
        let headers = response_headers("text/plain; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(b"ok")?;
        Ok::<_, anyhow::Error>(())
    })?;

    let nvs_for_get = nvs_partition.clone();
    let ip_for_get = ip_addr.to_owned();
    let build_for_get = build_id_value.clone();
    let board_for_get = board_id_value.clone();
    server.fn_handler("/api/settings", Method::Get, move |req| {
        let settings = read_device_settings(&nvs_for_get)?;
        let payload = settings_json(
            &settings,
            &ip_for_get,
            &build_for_get,
            &board_for_get,
            true,
            "ok",
            false,
        );
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let nvs_for_post = nvs_partition.clone();
    let ip_for_post = ip_addr.to_owned();
    let build_for_post = build_id_value;
    let board_for_post = board_id_value;
    let temperature_offset_for_post = temperature_offset_c;
    server.fn_handler("/api/settings", Method::Post, move |mut req| {
        let body_str = match read_request_body_utf8(&mut req, 512) {
            Ok(body) => body,
            Err(RequestBodyError::TooLarge) => {
                let payload = settings_json(
                    &read_device_settings(&nvs_for_post)?,
                    &ip_for_post,
                    &build_for_post,
                    &board_for_post,
                    false,
                    "Request body too large.",
                    false,
                );
                let headers = response_headers("application/json; charset=utf-8", "no-store");
                req.into_response(413, Some("Payload Too Large"), &headers)?
                    .write_all(payload.as_bytes())?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::InvalidUtf8) => {
                let payload = settings_json(
                    &read_device_settings(&nvs_for_post)?,
                    &ip_for_post,
                    &build_for_post,
                    &board_for_post,
                    false,
                    "Request body must be valid UTF-8.",
                    false,
                );
                let headers = response_headers("application/json; charset=utf-8", "no-store");
                req.into_response(400, Some("Bad Request"), &headers)?
                    .write_all(payload.as_bytes())?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::Io(err)) => return Err(err),
        };

        let existing_settings = read_device_settings(&nvs_for_post)?;

        let wifi_update_requested = matches!(
            parse_form_field(&body_str, "wifi_update").as_deref(),
            Some("1") | Some("true") | Some("on")
        );
        let requested_ssid = if wifi_update_requested {
            parse_form_field(&body_str, "ssid").unwrap_or_default()
        } else {
            String::new()
        };
        let pass = if wifi_update_requested {
            parse_form_field(&body_str, "password").unwrap_or_default()
        } else {
            String::new()
        };
        let ssid = if wifi_update_requested && requested_ssid.is_empty() {
            existing_settings.ssid.clone()
        } else {
            requested_ssid
        };
        let device_label = parse_form_field(&body_str, "device_label")
            .unwrap_or_else(|| "OpenBarista".to_owned())
            .trim()
            .to_owned();
        let offset_str = parse_form_field(&body_str, "temperature_offset_c")
            .unwrap_or_else(|| "0".to_owned())
            .trim()
            .to_owned();
        let device_label = if device_label.is_empty() {
            "OpenBarista".to_owned()
        } else {
            device_label
        };
        let parsed_temperature_offset_c = match offset_str.parse::<f32>() {
            Ok(value) => value,
            Err(_) => {
                let payload = settings_json(
                    &read_device_settings(&nvs_for_post)?,
                    &ip_for_post,
                    &build_for_post,
                    &board_for_post,
                    false,
                    "Temperature offset must be a number.",
                    false,
                );
                let headers = response_headers("application/json; charset=utf-8", "no-store");
                req.into_response(400, Some("Bad Request"), &headers)?
                    .write_all(payload.as_bytes())?;
                return Ok::<_, anyhow::Error>(());
            }
        };

        let wifi_change_requested = wifi_update_requested && (!ssid.is_empty() || !pass.is_empty());

        if wifi_update_requested && wifi_change_requested && ssid.is_empty() {
            let payload = settings_json(
                &existing_settings,
                &ip_for_post,
                &build_for_post,
                &board_for_post,
                false,
                "No current Wi-Fi network is saved yet. Select a network first.",
                false,
            );
            let headers = response_headers("application/json; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(payload.as_bytes())?;
            return Ok::<_, anyhow::Error>(());
        }

        if (wifi_change_requested && (ssid.len() > MAX_SSID_LEN || pass.len() > MAX_PASS_LEN))
            || device_label.len() > MAX_LABEL_LEN
            || !parsed_temperature_offset_c.is_finite()
            || parsed_temperature_offset_c.abs() > super::MAX_TEMP_OFFSET_ABS_C
        {
            let payload = settings_json(
                &read_device_settings(&nvs_for_post)?,
                &ip_for_post,
                &build_for_post,
                &board_for_post,
                false,
                "One or more fields are invalid or out of range.",
                false,
            );
            let headers = response_headers("application/json; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(payload.as_bytes())?;
            return Ok::<_, anyhow::Error>(());
        }

        let mut rebooting = false;
        save_device_label(&nvs_for_post, &device_label)?;
        save_temperature_offset(&nvs_for_post, parsed_temperature_offset_c)?;
        *lock_or_recover(&temperature_offset_for_post) = parsed_temperature_offset_c;

        if wifi_change_requested {
            let nvs_wifi = EspNvs::new(nvs_for_post.clone(), NVS_NAMESPACE, true)?;
            nvs_wifi.set_str(NVS_SSID_KEY, &ssid)?;
            nvs_wifi.set_str(NVS_PASS_KEY, &pass)?;
            rebooting = true;
        }

        let updated = read_device_settings(&nvs_for_post)?;
        let payload = settings_json(
            &updated,
            &ip_for_post,
            &build_for_post,
            &board_for_post,
            true,
            if rebooting {
                "Settings saved. Rebooting to apply network changes."
            } else if wifi_update_requested {
                "No Wi-Fi changes requested."
            } else {
                "Device settings saved."
            },
            rebooting,
        );
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;

        if rebooting {
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(1200));
                unsafe { esp_idf_svc::sys::esp_restart() };
            });
        }

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(server)
}

pub(super) fn start_connecting_status_portal(
    nvs_partition: EspDefaultNvsPartition,
    progress: Arc<Mutex<ConnectProgress>>,
    build_id_value: String,
    board_id_value: String,
) -> Result<EspHttpServer<'static>> {
    let nvs_for_connect = nvs_partition.clone();
    let progress_for_status = progress.clone();
    let server_config = HttpConfig {
        stack_size: 10240,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config)?;

    for path in super::CAPTIVE_PATHS {
        let build_id_for_page = build_id_value.clone();
        let board_id_for_page = board_id_value.clone();
        server.fn_handler(path, Method::Get, move |req| {
            let html = web_assets::captive_index_html(&build_id_for_page, &board_id_for_page);
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(200, Some("OK"), &headers)?
                .write_all(html.as_bytes())?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    for path in ["/portal.css", "/portal.js"] {
        server.fn_handler(path, Method::Get, move |req| {
            let asset =
                web_assets::captive_static(path).ok_or_else(|| anyhow!("missing {path} asset"))?;
            let headers = response_headers(asset.content_type, asset.cache_control);
            req.into_response(200, Some("OK"), &headers)?
                .write_all(asset.body)?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    let static_routes: [(&str, fn() -> web_assets::StaticAsset); 2] = [
        ("/base.css", web_assets::base_css),
        ("/settings.css", web_assets::settings_css),
    ];

    for (path, asset_fn) in static_routes {
        server.fn_handler(path, Method::Get, move |req| {
            let asset = asset_fn();
            let headers = response_headers(asset.content_type, asset.cache_control);
            req.into_response(200, Some("OK"), &headers)?
                .write_all(asset.body)?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    server.fn_handler("/status", Method::Get, move |req| {
        let payload = connect_progress_json(&lock_or_recover(&progress_for_status));
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/networks", Method::Get, move |req| {
        let settings = read_device_settings(&nvs_for_connect)?;
        let networks = if settings.ssid.is_empty() {
            Vec::new()
        } else {
            vec![settings.ssid]
        };
        let payload = networks_json(&networks);
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let nvs_for_handler = nvs_partition;
    let build_for_handler = build_id_value;
    let board_for_handler = board_id_value;
    let progress_for_connect = progress;
    server.fn_handler("/connect", Method::Post, move |mut req| {
        let body_str = match read_request_body_utf8(&mut req, 512) {
            Ok(body) => body,
            Err(RequestBodyError::TooLarge) => {
                let headers = response_headers("text/html; charset=utf-8", "no-store");
                req.into_response(413, Some("Payload Too Large"), &headers)?.write_all(
                    b"<html><body><p>Request body too large.</p><a href='/'>Go back</a></body></html>",
                )?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::InvalidUtf8) => {
                let headers = response_headers("text/html; charset=utf-8", "no-store");
                req.into_response(400, Some("Bad Request"), &headers)?.write_all(
                    b"<html><body><p>Request body must be valid UTF-8.</p><a href='/'>Go back</a></body></html>",
                )?;
                return Ok::<_, anyhow::Error>(());
            }
            Err(RequestBodyError::Io(err)) => return Err(err),
        };

        let ssid = parse_form_field(&body_str, "ssid").unwrap_or_default();
        let pass = parse_form_field(&body_str, "password").unwrap_or_default();

        if ssid.is_empty() {
            let body =
                b"<html><body><p>SSID cannot be empty.</p><a href='/'>Go back</a></body></html>";
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(body)?;
            return Ok::<_, anyhow::Error>(());
        }
        if ssid.len() > MAX_SSID_LEN || pass.len() > MAX_PASS_LEN {
            let body =
                b"<html><body><p>SSID/password too long.</p><a href='/'>Go back</a></body></html>";
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(body)?;
            return Ok::<_, anyhow::Error>(());
        }

        let nvs = EspNvs::new(nvs_for_handler.clone(), NVS_NAMESPACE, true)?;
        nvs.set_str(NVS_SSID_KEY, &ssid)?;
        nvs.set_str(NVS_PASS_KEY, &pass)?;
        {
            let mut state = lock_or_recover(&progress_for_connect);
            state.stage = "rebooting".to_owned();
            state.ssid = ssid.clone();
            state.message = format!("Saved credentials for '{}'. Rebooting...", ssid);
        }

        let success_html =
            web_assets::captive_success_html(&build_for_handler, &board_for_handler);
        let headers = response_headers("text/html; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(success_html.as_bytes())?;

        thread::spawn(|| {
            thread::sleep(Duration::from_millis(1200));
            unsafe { esp_idf_svc::sys::esp_restart() };
        });

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(server)
}
