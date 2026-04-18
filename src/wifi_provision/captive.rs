use std::io::ErrorKind;
use std::net::{Ipv4Addr as StdIpv4Addr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_svc::ipv4::Ipv4Addr;
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};

use openbarista::sync_utils::lock_or_recover;

use crate::web_assets;

use super::http::*;
use super::nvs::*;
use super::{ProvisionStatus, CAPTIVE_PATHS};

/// Starts a SoftAP with a captive portal HTTP server and blocks until the user
/// submits WiFi credentials. Saves the credentials to NVS and calls
/// `esp_restart()` — this function never actually returns.
pub(super) fn run_captive_portal(
    mut wifi: BlockingWifi<EspWifi<'static>>,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<()> {
    use esp_idf_svc::wifi::{AccessPointConfiguration, AuthMethod, Configuration as WifiConfig, ClientConfiguration};

    let ap_config = AccessPointConfiguration {
        ssid: super::AP_SSID
            .try_into()
            .map_err(|_| anyhow!("AP SSID error"))?,
        auth_method: AuthMethod::None,
        channel: 6,
        ..Default::default()
    };

    wifi.set_configuration(&WifiConfig::Mixed(
        ClientConfiguration::default(),
        ap_config.clone(),
    ))?;
    wifi.start()?;

    let dns_thread = start_captive_dns(super::AP_GATEWAY)?;
    let ap_ip = wifi.wifi().ap_netif().get_ip_info()?.ip;

    println!(
        "[wifi] SoftAP '{}' started. Connect and visit http://{}",
        super::AP_SSID, ap_ip
    );

    let build_id_value = build_id().to_owned();
    let board_id_value = board_id();

    let networks_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let networks_for_handler = networks_cache.clone();
    let scan_requested: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    let scan_requested_for_handler = scan_requested.clone();

    let status: Arc<Mutex<ProvisionStatus>> = Arc::new(Mutex::new(ProvisionStatus::Idle));
    let status_for_handler = status.clone();
    let nvs_for_handler = nvs_partition;
    let build_id_for_handler = build_id_value.clone();
    let board_id_for_handler = board_id_value.clone();

    let server_config = HttpConfig {
        stack_size: 10240,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config)?;

    for path in CAPTIVE_PATHS {
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

    server.fn_handler("/networks", Method::Get, move |req| {
        *lock_or_recover(&scan_requested_for_handler) = true;
        let networks = lock_or_recover(&networks_for_handler).clone();
        let payload = networks_json(&networks);
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    let status_for_get = status.clone();
    server.fn_handler("/status", Method::Get, move |req| {
        let state = lock_or_recover(&status_for_get);
        let (stage, message) = match &*state {
            ProvisionStatus::Idle => ("provisioning", "Waiting for Wi-Fi credentials."),
            ProvisionStatus::Rebooting => ("rebooting", "Saved credentials. Rebooting now..."),
        };
        let payload = format!(
            "{{\"stage\":\"{}\",\"ssid\":\"\",\"attempt\":0,\"total\":5,\"message\":\"{}\"}}",
            stage, message,
        );
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

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
        } else if ssid.len() > MAX_SSID_LEN || pass.len() > MAX_PASS_LEN {
            let body =
                b"<html><body><p>SSID/password too long.</p><a href='/'>Go back</a></body></html>";
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(body)?;
        } else {
            let nvs = EspNvs::new(nvs_for_handler.clone(), NVS_NAMESPACE, true)?;
            nvs.set_str(NVS_SSID_KEY, &ssid)?;
            nvs.set_str(NVS_PASS_KEY, &pass)?;
            println!("[wifi] Credentials for '{}' saved. Rebooting...", ssid);
            let success_html =
                web_assets::captive_success_html(&build_id_for_handler, &board_id_for_handler);
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(200, Some("OK"), &headers)?
                .write_all(success_html.as_bytes())?;
            *lock_or_recover(&status_for_handler) = ProvisionStatus::Rebooting;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    loop {
        thread::sleep(Duration::from_millis(100));

        if matches!(*lock_or_recover(&status), ProvisionStatus::Rebooting) {
            thread::sleep(Duration::from_millis(1500));
            drop(server);
            drop(dns_thread);
            unsafe { esp_idf_svc::sys::esp_restart() };
        }

        let should_scan = {
            let mut requested = lock_or_recover(&scan_requested);
            let value = *requested;
            *requested = false;
            value
        };

        if should_scan {
            refresh_network_cache(&mut wifi, &networks_cache);
        }
    }
}

// ---------------------------------------------------------------------------
// DNS server for captive portal
// ---------------------------------------------------------------------------

pub(super) fn start_captive_dns(
    ap_gateway: Ipv4Addr,
) -> Result<thread::JoinHandle<()>> {
    let socket = UdpSocket::bind((StdIpv4Addr::UNSPECIFIED, 53))
        .map_err(|e| anyhow!("Failed to bind captive DNS on :53: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| anyhow!("Failed to set DNS socket timeout: {e}"))?;

    let handle = thread::spawn(move || {
        let mut rx = [0u8; 512];

        loop {
            match socket.recv_from(&mut rx) {
                Ok((len, peer)) => {
                    if let Some(reply) = build_dns_reply(&rx[..len], ap_gateway) {
                        if let Err(err) = socket.send_to(&reply, peer) {
                            println!("[wifi] Captive DNS send error: {err}");
                        }
                    }
                }
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => {
                    println!("[wifi] Captive DNS stopped: {err}");
                    break;
                }
            }
        }
    });

    Ok(handle)
}

fn build_dns_reply(query: &[u8], ap_gateway: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }

    let flags = u16::from_be_bytes([query[2], query[3]]);
    let is_query = (flags & 0x8000) == 0;
    let qd_count = u16::from_be_bytes([query[4], query[5]]);
    if !is_query || qd_count == 0 {
        return None;
    }

    let mut idx = 12usize;
    while idx < query.len() {
        let label_len = query[idx] as usize;
        idx += 1;
        if label_len == 0 {
            break;
        }
        idx = idx.checked_add(label_len)?;
    }

    if idx + 4 > query.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([query[idx], query[idx + 1]]);
    let question_end = idx + 4;

    let answer_count: u16 = if qtype == 1 || qtype == 255 { 1 } else { 0 };

    let mut reply =
        Vec::with_capacity(12 + (question_end - 12) + if answer_count == 1 { 16 } else { 0 });
    reply.extend_from_slice(&query[0..2]);
    reply.extend_from_slice(&0x8180u16.to_be_bytes());
    reply.extend_from_slice(&1u16.to_be_bytes());
    reply.extend_from_slice(&answer_count.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    reply.extend_from_slice(&query[12..question_end]);

    if answer_count == 1 {
        reply.extend_from_slice(&[0xC0, 0x0C]);
        reply.extend_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&30u32.to_be_bytes());
        reply.extend_from_slice(&4u16.to_be_bytes());
        reply.extend_from_slice(&ap_gateway.octets());
    }
    Some(reply)
}

// ---------------------------------------------------------------------------
// WiFi scan helpers
// ---------------------------------------------------------------------------

fn refresh_network_cache(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    cache: &Arc<Mutex<Vec<String>>>,
) {
    match wifi.scan_n::<16>() {
        Ok((found, _total)) => {
            let mut names: Vec<String> = found
                .into_iter()
                .map(|ap| ap.ssid.to_string())
                .filter(|name| !name.is_empty())
                .collect();
            names.sort();
            names.dedup();
            *lock_or_recover(cache) = names;
        }
        Err(err) => {
            println!("[wifi] Scan failed: {err:?}");
        }
    }
}
