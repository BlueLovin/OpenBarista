use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr as StdIpv4Addr, UdpSocket},
};

use anyhow::{anyhow, Result};
use embedded_svc::http::Headers;
use embedded_svc::ipv4::{self, Ipv4Addr, Mask, Subnet};
use esp_idf_hal::modem::Modem;
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::{
        server::{Configuration as HttpConfig, EspHttpServer},
        Method,
    },
    io::Write,
    netif::{EspNetif, NetifConfiguration, NetifStack},
    nvs::{EspDefaultNvsPartition, EspNvs},
    wifi::{
        AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration,
        Configuration as WifiConfig, EspWifi, WifiDriver,
    },
};

use crate::web_assets;

const NVS_NAMESPACE: &str = "wifi";
const NVS_SSID_KEY: &str = "ssid";
const NVS_PASS_KEY: &str = "pass";

const AP_SSID: &str = "OpenBarista";
const AP_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
const MDNS_HOSTNAME: &str = "openbarista";
const PROVISION_TASK_STACK_SIZE: usize = 32 * 1024;

// Captive portal detection paths used by Android, iOS, Windows, macOS
const CAPTIVE_PATHS: &[&str] = &[
    "/",
    "/generate_204",
    "/hotspot-detect.html",
    "/fwlink",
    "/connecttest.txt",
    "/ncsi.txt",
    "/redirect",
];

fn response_headers<'a>(content_type: &'a str, cache_control: &'a str) -> [(&'a str, &'a str); 7] {
    [
                ("Content-Type", content_type),
                ("Cache-Control", cache_control),
                ("X-Content-Type-Options", "nosniff"),
                ("X-Frame-Options", "DENY"),
                ("Referrer-Policy", "no-referrer"),
                (
                        "Content-Security-Policy",
                        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'",
                ),
                ("Permissions-Policy", "geolocation=(), microphone=(), camera=()"),
        ]
}

#[derive(Clone)]
enum ProvisionStatus {
    Idle,
    Rebooting,
}

/// Holds the active WiFi driver and optional mDNS handle.
/// Both must remain alive for the duration of the program.
pub struct WifiStack {
    #[allow(dead_code)]
    pub wifi: BlockingWifi<EspWifi<'static>>,
    /// Station-mode IP address as a string (e.g. "192.168.1.42").
    pub ip_addr: String,
    #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
    #[allow(dead_code)]
    pub mdns: EspMdns,
}

/// Initialises WiFi.
///
/// - If valid credentials are stored in NVS the device connects to the saved
///   network, starts mDNS, and returns immediately.
/// - If no credentials exist (or the connection repeatedly fails), the device
///   starts a SoftAP named "OpenBarista" and serves a captive-portal setup
///   page at 192.168.4.1.  Once the user submits credentials the device saves
///   them to NVS and calls `esp_restart()`; this function never returns in that
///   path.
pub fn setup_wifi(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<WifiStack> {
    // Read any previously saved credentials from NVS.
    let (saved_ssid, saved_pass) = read_saved_credentials(&nvs_partition)?;

    let driver = WifiDriver::new(modem, sysloop.clone(), Some(nvs_partition.clone()))?;
    let sta_netif = EspNetif::new(NetifStack::Sta)?;
    let ap_netif = create_softap_netif(AP_GATEWAY)?;
    let esp_wifi = EspWifi::wrap_all(driver, sta_netif, ap_netif)?;
    let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)?;

    if let (Some(ssid), Some(pass)) = (saved_ssid, saved_pass) {
        println!(
            "[wifi] Saved credentials for '{}'. Scanning to detect auth method...",
            ssid
        );

        // Start briefly in STA mode with no target SSID just to scan, so we
        // can use the exact auth method the AP advertises.
        wifi.set_configuration(&WifiConfig::Client(ClientConfiguration::default()))?;
        wifi.start()?;
        let auth = if pass.is_empty() {
            AuthMethod::None
        } else {
            scan_for_auth(&mut wifi, &ssid)
        };
        wifi.stop()?;
        println!("[wifi] Connecting to '{}' with auth {:?}...", ssid, auth);

        let h_ssid = ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow!("Saved SSID is too long (max 32 chars)"))?;
        let h_pass = pass
            .as_str()
            .try_into()
            .map_err(|_| anyhow!("Saved password is too long (max 64 chars)"))?;

        wifi.set_configuration(&WifiConfig::Client(ClientConfiguration {
            ssid: h_ssid,
            password: h_pass,
            auth_method: auth,
            ..Default::default()
        }))?;
        wifi.start()?;

        if try_connect(&mut wifi, &ssid) {
            wifi.wait_netif_up()?;
            let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;
            println!(
                "[wifi] Connected to '{}'. IP: {} | http://openbarista.local | http://{}",
                ssid, ip, ip
            );
            #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
            let mdns = start_mdns()?;
            return Ok(WifiStack {
                wifi,
                ip_addr: ip.to_string(),
                #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
                mdns,
            });
        }

        println!("[wifi] Could not connect after retries. Starting provisioning portal...");
        wifi.stop()?;
    } else {
        println!("[wifi] No saved credentials. Starting provisioning portal...");
    }

    run_captive_portal_on_dedicated_task(wifi, nvs_partition)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn read_saved_credentials(
    nvs_partition: &EspDefaultNvsPartition,
) -> Result<(Option<String>, Option<String>)> {
    let nvs = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)?;
    let mut ssid_buf = [0u8; 33];
    let mut pass_buf = [0u8; 65];

    let ssid = nvs
        .get_str(NVS_SSID_KEY, &mut ssid_buf)?
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let pass = nvs
        .get_str(NVS_PASS_KEY, &mut pass_buf)?
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok((ssid, pass))
}

/// Attempts to connect to a WiFi network up to 5 times.
/// Returns `true` on success.
fn try_connect(wifi: &mut BlockingWifi<EspWifi<'static>>, ssid: &str) -> bool {
    for attempt in 1..=5 {
        println!("[wifi] Connect attempt {attempt}/5 to '{ssid}'...");
        if wifi.connect().is_ok() {
            println!("[wifi] Connected to '{ssid}'.");
            return true;
        }
        thread::sleep(Duration::from_secs(3));
    }
    false
}

#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
fn start_mdns() -> Result<EspMdns> {
    let mut mdns = EspMdns::take()?;
    mdns.set_hostname(MDNS_HOSTNAME)?;
    mdns.set_instance_name("OpenBarista")?;
    // Advertise an HTTP service so discovery tools can find it.
    mdns.add_service(None, "_http", "_tcp", 80, &[])?;
    Ok(mdns)
}

/// Starts a SoftAP with a captive portal HTTP server and blocks until the user
/// submits WiFi credentials.  Saves the credentials to NVS and calls
/// `esp_restart()` — this function never actually returns.
fn run_captive_portal(
    mut wifi: BlockingWifi<EspWifi<'static>>,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<WifiStack> {
    let ap_config = AccessPointConfiguration {
        ssid: AP_SSID.try_into().map_err(|_| anyhow!("AP SSID error"))?,
        auth_method: AuthMethod::None,
        channel: 6,
        ..Default::default()
    };

    wifi.set_configuration(&WifiConfig::Mixed(
        ClientConfiguration::default(),
        ap_config.clone(),
    ))?;
    wifi.start()?;

    // Route hostname lookups to the AP so Android/iOS captive checks can
    // resolve and hit the portal endpoints.
    let _dns_thread = start_captive_dns(AP_GATEWAY)?;

    let ap_ip = wifi.wifi().ap_netif().get_ip_info()?.ip;

    println!(
        "[wifi] SoftAP '{}' started. Connect and visit http://{}",
        AP_SSID, ap_ip
    );

    // Credentials shared between the POST handler task and our polling loop.
    // Nearby SSIDs cache for the setup page.
    let networks_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let networks_for_handler = networks_cache.clone();
    let scan_requested: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    let scan_requested_for_handler = scan_requested.clone();

    let status: Arc<Mutex<ProvisionStatus>> = Arc::new(Mutex::new(ProvisionStatus::Idle));
    let status_for_handler = status.clone();
    // Move NVS partition into the POST handler so it can save directly.
    let nvs_for_handler = nvs_partition;

    let server_config = HttpConfig {
        stack_size: 10240,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config)?;

    // Register the setup page on all common captive-portal detection paths so
    // that phones show the "Sign in to network" prompt automatically.
    for path in CAPTIVE_PATHS {
        server.fn_handler(path, Method::Get, |req| {
            let asset = web_assets::captive_index();
            let headers = response_headers(asset.content_type, asset.cache_control);
            req.into_response(200, Some("OK"), &headers)?
                .write_all(asset.body)?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    server.fn_handler("/portal.css", Method::Get, |req| {
        let asset = web_assets::captive_static("/portal.css")
            .ok_or_else(|| anyhow!("missing /portal.css asset"))?;
        let headers = response_headers(asset.content_type, asset.cache_control);
        req.into_response(200, Some("OK"), &headers)?
            .write_all(asset.body)?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/portal.js", Method::Get, |req| {
        let asset = web_assets::captive_static("/portal.js")
            .ok_or_else(|| anyhow!("missing /portal.js asset"))?;
        let headers = response_headers(asset.content_type, asset.cache_control);
        req.into_response(200, Some("OK"), &headers)?
            .write_all(asset.body)?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/networks", Method::Get, move |req| {
        *scan_requested_for_handler.lock().unwrap() = true;
        let networks = networks_for_handler.lock().unwrap().clone();
        let payload = networks_json(&networks);
        let headers = response_headers("application/json; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(payload.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/connect", Method::Post, move |mut req| {
        let content_len = req.content_len().unwrap_or(0).min(512) as usize;
        let mut body = vec![0u8; content_len];
        let mut offset = 0;
        while offset < content_len {
            let n = req.read(&mut body[offset..])?;
            if n == 0 {
                break;
            }
            offset += n;
        }
        body.truncate(offset);

        let body_str = std::str::from_utf8(&body).unwrap_or("");
        let ssid = parse_form_field(body_str, "ssid").unwrap_or_default();
        let pass = parse_form_field(body_str, "password").unwrap_or_default();

        if ssid.is_empty() {
            let body =
                b"<html><body><p>SSID cannot be empty.</p><a href='/'>Go back</a></body></html>";
            let headers = response_headers("text/html; charset=utf-8", "no-store");
            req.into_response(400, Some("Bad Request"), &headers)?
                .write_all(body)?;
        } else {
            let nvs = EspNvs::new(nvs_for_handler.clone(), NVS_NAMESPACE, true)?;
            nvs.set_str(NVS_SSID_KEY, &ssid)?;
            nvs.set_str(NVS_PASS_KEY, &pass)?;
            println!("[wifi] Credentials for '{}' saved. Rebooting...", ssid);
            let success_page = web_assets::captive_success();
            let headers = response_headers(success_page.content_type, success_page.cache_control);
            req.into_response(200, Some("OK"), &headers)?
                .write_all(success_page.body)?;
            *status_for_handler.lock().unwrap() = ProvisionStatus::Rebooting;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    // Poll loop: refresh nearby SSIDs and reboot once credentials are saved.
    loop {
        thread::sleep(Duration::from_millis(100));

        if matches!(*status.lock().unwrap(), ProvisionStatus::Rebooting) {
            // Give the HTTP response enough time to flush before restarting.
            thread::sleep(Duration::from_millis(1500));
            drop(server);
            unsafe { esp_idf_svc::sys::esp_restart() };
        }

        if *scan_requested.lock().unwrap() {
            *scan_requested.lock().unwrap() = false;
            refresh_network_cache(&mut wifi, &networks_cache);
        }
    }
}

fn run_captive_portal_on_dedicated_task(
    wifi: BlockingWifi<EspWifi<'static>>,
    nvs_partition: EspDefaultNvsPartition,
) -> Result<WifiStack> {
    let builder = thread::Builder::new()
        .name("wifi-provision".to_owned())
        .stack_size(PROVISION_TASK_STACK_SIZE);

    let handle = builder
        .spawn(move || {
            if let Err(err) = run_captive_portal(wifi, nvs_partition) {
                println!("[wifi] Provisioning task failed: {err:?}");
            }
        })
        .map_err(|err| anyhow!("Failed to spawn provisioning task: {err}"))?;

    println!(
        "[wifi] Provisioning task started (stack {} bytes).",
        PROVISION_TASK_STACK_SIZE
    );

    let _ = handle.join();
    Err(anyhow!("Provisioning task exited unexpectedly"))
}

pub fn start_station_http_server(ip_addr: &str) -> Result<EspHttpServer<'static>> {
    let html = web_assets::station_index_html(ip_addr);
    let mut server = EspHttpServer::new(&HttpConfig::default())?;

    server.fn_handler("/", Method::Get, move |req| {
        let headers = response_headers("text/html; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(html.as_bytes())?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/station.css", Method::Get, |req| {
        let asset = web_assets::station_css();
        let headers = response_headers(asset.content_type, asset.cache_control);
        req.into_response(200, Some("OK"), &headers)?
            .write_all(asset.body)?;
        Ok::<_, anyhow::Error>(())
    })?;

    server.fn_handler("/health", Method::Get, |req| {
        let headers = response_headers("text/plain; charset=utf-8", "no-store");
        req.into_response(200, Some("OK"), &headers)?
            .write_all(b"ok")?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(server)
}

/// Scans visible networks and returns the auth method advertised by
/// `target_ssid`. Falls back to `WPA2WPA3Personal` if not found.
fn scan_for_auth(wifi: &mut BlockingWifi<EspWifi<'static>>, target_ssid: &str) -> AuthMethod {
    match wifi.scan_n::<32>() {
        Ok((found, _)) => {
            for ap in found {
                if ap.ssid.as_str().eq_ignore_ascii_case(target_ssid) {
                    let auth = ap.auth_method.unwrap_or(AuthMethod::WPA2WPA3Personal);
                    println!("[wifi] Scan: '{}' uses auth {:?}", target_ssid, auth);
                    return auth;
                }
            }
            println!(
                "[wifi] '{}' not in scan, defaulting to WPA2WPA3Personal",
                target_ssid
            );
        }
        Err(e) => println!("[wifi] Scan error: {e:?}"),
    }
    AuthMethod::WPA2WPA3Personal
}

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
            *cache.lock().unwrap() = names;
        }
        Err(err) => {
            println!("[wifi] Scan failed: {err:?}");
        }
    }
}

fn networks_json(items: &[String]) -> String {
    let mut out = String::from("[");
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(item));
        out.push('"');
    }
    out.push(']');
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn create_softap_netif(ap_gateway: Ipv4Addr) -> Result<EspNetif> {
    let mut ap_netif_conf = NetifConfiguration::wifi_default_router();
    ap_netif_conf.ip_configuration = Some(ipv4::Configuration::Router(ipv4::RouterConfiguration {
        subnet: Subnet {
            gateway: ap_gateway,
            mask: Mask(24),
        },
        dhcp_enabled: true,
        // Force DNS handed to clients to the AP itself for captive portal flow.
        dns: Some(ap_gateway),
        secondary_dns: None,
    }));

    Ok(EspNetif::new_with_conf(&ap_netif_conf)?)
}

fn start_captive_dns(ap_gateway: Ipv4Addr) -> Result<thread::JoinHandle<()>> {
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
                        let _ = socket.send_to(&reply, peer);
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

    // Walk QNAME labels.
    let mut idx = 12usize;
    while idx < query.len() {
        let label_len = query[idx] as usize;
        idx += 1;
        if label_len == 0 {
            break;
        }
        idx = idx.checked_add(label_len)?;
    }

    // QTYPE + QCLASS
    if idx + 4 > query.len() {
        return None;
    }
    let question_end = idx + 4;

    let mut reply = Vec::with_capacity(12 + (question_end - 12) + 16);
    // ID
    reply.extend_from_slice(&query[0..2]);
    // Flags: standard response, recursion available false, no error
    reply.extend_from_slice(&0x8180u16.to_be_bytes());
    // QDCOUNT, ANCOUNT, NSCOUNT, ARCOUNT
    reply.extend_from_slice(&1u16.to_be_bytes());
    reply.extend_from_slice(&1u16.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    // Original question
    reply.extend_from_slice(&query[12..question_end]);
    // Answer name pointer to original QNAME at offset 12
    reply.extend_from_slice(&[0xC0, 0x0C]);
    // TYPE A, CLASS IN
    reply.extend_from_slice(&1u16.to_be_bytes());
    reply.extend_from_slice(&1u16.to_be_bytes());
    // TTL 30s
    reply.extend_from_slice(&30u32.to_be_bytes());
    // RDLENGTH + RDATA
    reply.extend_from_slice(&4u16.to_be_bytes());
    reply.extend_from_slice(&ap_gateway.octets());

    Some(reply)
}

// ---------------------------------------------------------------------------
// URL-encoded form helpers
// ---------------------------------------------------------------------------

fn parse_form_field(body: &str, key: &str) -> Option<String> {
    body.split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| url_decode(v))
}

/// Decodes a percent-encoded URL component (`+` → space, `%XX` → byte).
/// Handles UTF-8 encoded multibyte sequences correctly.
fn url_decode(s: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let src = s.as_bytes();
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'+' => {
                bytes.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < src.len() => {
                // SAFETY: src[i+1..i+3] are ASCII hex digits or we fall through.
                if let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&src[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    bytes.push(byte);
                    i += 3;
                } else {
                    bytes.push(b'%');
                    i += 1;
                }
            }
            b => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
