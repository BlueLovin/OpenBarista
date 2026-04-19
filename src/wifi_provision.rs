use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr as StdIpv4Addr, UdpSocket},
};

use anyhow::{anyhow, Result};
use embedded_svc::ipv4::{self, Ipv4Addr, Mask, Subnet};
use esp_idf_hal::modem::WifiModemPeripheral;
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::{
        server::{Configuration as HttpConfig, EspHttpServer},
        Method,
    },
    io::{Read, Write},
    netif::{EspNetif, NetifConfiguration},
    nvs::{EspDefaultNvsPartition, EspNvs},
    wifi::{
        AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration,
        Configuration as WifiConfig, EspWifi, WifiDriver,
    },
};

use crate::{
    scale_ble::{SavedScale, ScaleRuntime},
    web_assets,
};
use openbarista::telemetry_feed::SharedTelemetry;

const NVS_NAMESPACE: &str = "wifi";
const NVS_SSID_KEY: &str = "ssid";
const NVS_PASS_KEY: &str = "pass";
const SETTINGS_NAMESPACE: &str = "settings";
const SETTINGS_LABEL_KEY: &str = "label";
const SETTINGS_TEMP_OFFSET_KEY: &str = "temp_offset_c";
const SCALE_NAMESPACE: &str = "scale";
const SCALE_ADDR_KEY: &str = "addr";
const SCALE_NAME_KEY: &str = "name";
const SCALE_ADDR_TYPE_KEY: &str = "addr_type";

const AP_SSID: &str = "OpenBarista";
const MAX_SSID_LEN: usize = 32;
const MAX_PASS_LEN: usize = 64;
const MAX_LABEL_LEN: usize = 32;
const MAX_SCALE_NAME_LEN: usize = 48;
const MAX_SCALE_ADDR_LEN: usize = 17;
const MAX_SCALE_ADDR_TYPE_LEN: usize = 16;
const MAX_TEMP_OFFSET_ABS_C: f32 = 20.0;
const AP_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
const MDNS_HOSTNAME: &str = "openbarista";

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

// Station pages use uPlot which applies inline styles, so style-src allows 'unsafe-inline'.
fn station_response_headers<'a>(
    content_type: &'a str,
    cache_control: &'a str,
) -> [(&'a str, &'a str); 7] {
    [
        ("Content-Type", content_type),
        ("Cache-Control", cache_control),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'self'",
        ),
        ("Permissions-Policy", "geolocation=(), microphone=(), camera=()"),
    ]
}

use openbarista::sync_utils::lock_or_recover;

enum RequestBodyError {
    TooLarge,
    InvalidUtf8,
    Io(anyhow::Error),
}

fn read_request_body_utf8<R: Read>(
    reader: &mut R,
    max_body_len: usize,
) -> std::result::Result<String, RequestBodyError> {
    let mut body = Vec::with_capacity(max_body_len);

    loop {
        let mut buf = [0u8; 128];
        let n = reader
            .read(&mut buf)
            .map_err(|err| RequestBodyError::Io(anyhow!("request body read failed: {err:?}")))?;
        if n == 0 {
            break;
        }

        let remaining = max_body_len.saturating_sub(body.len());
        if n > remaining {
            return Err(RequestBodyError::TooLarge);
        }

        body.extend_from_slice(&buf[..n]);
    }

    let body_str = std::str::from_utf8(&body).map_err(|_| RequestBodyError::InvalidUtf8)?;
    Ok(body_str.to_owned())
}

#[derive(Clone)]
enum ProvisionStatus {
    Idle,
    Rebooting,
}

#[derive(Clone)]
struct ConnectProgress {
    stage: String,
    ssid: String,
    attempt: u8,
    total: u8,
    message: String,
}

impl ConnectProgress {
    fn new(ssid: String, total: u8) -> Self {
        Self {
            stage: "booting".to_owned(),
            ssid,
            attempt: 0,
            total,
            message: "Starting Wi-Fi services...".to_owned(),
        }
    }
}

#[derive(Clone)]
struct DeviceSettings {
    ssid: String,
    device_label: String,
    temperature_offset_c: f32,
}

fn connect_progress_json(progress: &ConnectProgress) -> String {
    format!(
        "{{\"stage\":\"{}\",\"ssid\":\"{}\",\"attempt\":{},\"total\":{},\"message\":\"{}\"}}",
        json_escape(&progress.stage),
        json_escape(&progress.ssid),
        progress.attempt,
        progress.total,
        json_escape(&progress.message),
    )
}

fn build_id() -> &'static str {
    option_env!("OPENBARISTA_BUILD_ID").unwrap_or("dev")
}

fn board_id() -> String {
    let mut mac = [0u8; 6];
    let err = unsafe { esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if err != 0 {
        return "unknown".to_owned();
    }
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn read_device_settings(nvs_partition: &EspDefaultNvsPartition) -> Result<DeviceSettings> {
    let nvs_wifi = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)?;
    let mut ssid_buf = [0u8; 33];
    let ssid = nvs_wifi
        .get_str(NVS_SSID_KEY, &mut ssid_buf)?
        .unwrap_or("")
        .to_owned();

    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    let mut label_buf = [0u8; MAX_LABEL_LEN + 1];
    let device_label = nvs_settings
        .get_str(SETTINGS_LABEL_KEY, &mut label_buf)?
        .unwrap_or("OpenBarista")
        .to_owned();

    let mut temp_offset_buf = [0u8; 24];
    let temperature_offset_c = nvs_settings
        .get_str(SETTINGS_TEMP_OFFSET_KEY, &mut temp_offset_buf)?
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0);

    Ok(DeviceSettings {
        ssid,
        device_label,
        temperature_offset_c,
    })
}

fn save_device_label(nvs_partition: &EspDefaultNvsPartition, device_label: &str) -> Result<()> {
    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    nvs_settings.set_str(SETTINGS_LABEL_KEY, device_label)?;
    Ok(())
}

fn save_temperature_offset(
    nvs_partition: &EspDefaultNvsPartition,
    temperature_offset_c: f32,
) -> Result<()> {
    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    nvs_settings.set_str(
        SETTINGS_TEMP_OFFSET_KEY,
        &format!("{temperature_offset_c:.3}"),
    )?;
    Ok(())
}

fn read_saved_scale(nvs_partition: &EspDefaultNvsPartition) -> Result<Option<SavedScale>> {
    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;

    let mut address_buf = [0u8; MAX_SCALE_ADDR_LEN + 1];
    let mut name_buf = [0u8; MAX_SCALE_NAME_LEN + 1];
    let mut addr_type_buf = [0u8; MAX_SCALE_ADDR_TYPE_LEN + 1];

    let address = nvs_scale
        .get_str(SCALE_ADDR_KEY, &mut address_buf)?
        .unwrap_or("")
        .trim()
        .to_owned();

    if address.is_empty() {
        return Ok(None);
    }

    let name = nvs_scale
        .get_str(SCALE_NAME_KEY, &mut name_buf)?
        .unwrap_or("Saved scale")
        .trim()
        .to_owned();
    let addr_type = nvs_scale
        .get_str(SCALE_ADDR_TYPE_KEY, &mut addr_type_buf)?
        .unwrap_or("public")
        .trim()
        .to_owned();

    Ok(Some(SavedScale {
        address,
        name,
        addr_type,
    }))
}

fn save_saved_scale(
    nvs_partition: &EspDefaultNvsPartition,
    saved_scale: &SavedScale,
) -> Result<()> {
    if saved_scale.address.is_empty()
        || saved_scale.address.len() > MAX_SCALE_ADDR_LEN
        || saved_scale.name.len() > MAX_SCALE_NAME_LEN
        || saved_scale.addr_type.len() > MAX_SCALE_ADDR_TYPE_LEN
    {
        return Err(anyhow!("Scale settings are invalid or out of range."));
    }

    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;
    nvs_scale.set_str(SCALE_ADDR_KEY, &saved_scale.address)?;
    nvs_scale.set_str(SCALE_NAME_KEY, &saved_scale.name)?;
    nvs_scale.set_str(SCALE_ADDR_TYPE_KEY, &saved_scale.addr_type)?;
    Ok(())
}

fn clear_saved_scale(nvs_partition: &EspDefaultNvsPartition) -> Result<()> {
    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;
    nvs_scale.remove(SCALE_ADDR_KEY)?;
    nvs_scale.remove(SCALE_NAME_KEY)?;
    nvs_scale.remove(SCALE_ADDR_TYPE_KEY)?;
    Ok(())
}

/// Holds the active WiFi driver and optional mDNS handle.
/// Both must remain alive for the duration of the program.
pub struct WifiStack {
    /// Shared reference kept alive so WiFi stays connected; also used for
    /// on-demand network scans from the station HTTP server.
    pub wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>,
    /// Station-mode IP address as a string (e.g. "192.168.1.42").
    pub ip_addr: String,
    #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
    pub mdns: EspMdns,
}

struct CaptiveDnsServer {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for CaptiveDnsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Holds all connectivity components that must stay alive for the lifetime
/// of the firmware.
pub struct WifiRuntime {
    pub stack: WifiStack,
    pub station_http_server: EspHttpServer<'static>,
    temperature_offset_c: Arc<Mutex<f32>>,
}

impl WifiRuntime {
    pub fn ip_addr(&self) -> &str {
        &self.stack.ip_addr
    }

    fn log_keepalive_state(&self) {
        let wifi_refs = Arc::strong_count(&self.stack.wifi);
        let http_server_size = core::mem::size_of_val(&self.station_http_server);
        #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
        let mdns_size = core::mem::size_of_val(&self.stack.mdns);
        println!(
            "[wifi] Station services online at http://{} (wifi refs: {}, http server bytes: {}).",
            self.stack.ip_addr, wifi_refs, http_server_size
        );
        #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
        println!("[wifi] mDNS keepalive handle active ({} bytes).", mdns_size);
    }

    pub fn temperature_offset_c(&self) -> f32 {
        *lock_or_recover(&self.temperature_offset_c)
    }
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
pub fn setup_wifi<M>(
    modem: M,
    nvs_partition: EspDefaultNvsPartition,
    telemetry: SharedTelemetry,
    scale_runtime: Arc<ScaleRuntime>,
) -> Result<WifiRuntime, anyhow::Error>
where
    M: WifiModemPeripheral + 'static,
{
    // --- WiFi provisioning & mDNS -------------------------------------------
    // On first boot this will start a SoftAP named "OpenBarista" and serve a
    // captive portal at 192.168.4.1 so the user can enter their home WiFi
    // credentials.  On subsequent boots the device connects to the saved
    // network and advertises itself as http://openbarista.local via mDNS.
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_for_station_server = nvs_partition.clone();
    let initial_settings = read_device_settings(&nvs_for_station_server)?;
    let temperature_offset_c = Arc::new(Mutex::new(initial_settings.temperature_offset_c));
    // Read any previously saved credentials from NVS.
    let (saved_ssid, saved_pass) = read_saved_credentials(&nvs_partition)?;

    let driver = WifiDriver::new(modem, sysloop.clone(), Some(nvs_partition.clone()))?;
    let sta_netif = create_station_netif()?;
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

        let client_config = ClientConfiguration {
            ssid: h_ssid,
            password: h_pass,
            auth_method: auth,
            ..Default::default()
        };
        let ap_config = AccessPointConfiguration {
            ssid: AP_SSID.try_into().map_err(|_| anyhow!("AP SSID error"))?,
            auth_method: AuthMethod::None,
            channel: 6,
            ..Default::default()
        };

        wifi.set_configuration(&WifiConfig::Mixed(client_config, ap_config))?;

        // The Rust wrapper doesn't expose failure_retry_cnt, but WiFi 6 routers
        // often send "Association refused temporarily" with a comeback timer.
        // Patch the raw C config so the driver retries internally instead of
        // failing immediately back to the application.
        unsafe {
            let mut raw_cfg: esp_idf_svc::sys::wifi_config_t = core::mem::zeroed();
            esp_idf_svc::sys::esp_wifi_get_config(
                esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA,
                &mut raw_cfg,
            );
            raw_cfg.sta.failure_retry_cnt = 3;
            esp_idf_svc::sys::esp_wifi_set_config(
                esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA,
                &mut raw_cfg,
            );
        }

        wifi.start()?;

        let connect_progress = Arc::new(Mutex::new(ConnectProgress::new(ssid.clone(), 5)));
        {
            let mut progress = lock_or_recover(&connect_progress);
            progress.stage = "connecting".to_owned();
            progress.message = format!("Trying '{}'...", ssid);
        }
        let connect_dns_thread = start_captive_dns(AP_GATEWAY)?;
        let connect_server = start_connecting_status_portal(
            nvs_partition.clone(),
            connect_progress.clone(),
            build_id().to_owned(),
            board_id(),
        )?;

        let mut connected = false;
        for attempt in 1..=5 {
            {
                let mut progress = lock_or_recover(&connect_progress);
                progress.stage = "connecting".to_owned();
                progress.attempt = attempt;
                progress.message = format!("Connecting to '{}' (attempt {attempt}/5)...", ssid);
            }
            println!("[wifi] Connect attempt {attempt}/5 to '{ssid}'...");
            if wifi.connect().is_ok() {
                connected = true;
                break;
            }
            thread::sleep(Duration::from_secs(3));
        }

        if connected {
            {
                let mut progress = lock_or_recover(&connect_progress);
                progress.stage = "connected".to_owned();
                progress.message = format!("Connected to '{}'. Bringing dashboard online...", ssid);
            }
            drop(connect_server);
            drop(connect_dns_thread);
            wifi.wait_netif_up()?;
            let ps_err = unsafe {
                esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE)
            };
            if ps_err != 0 {
                println!("[wifi] Warning: could not disable WiFi power save (err={ps_err}).");
            }
            let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;
            println!(
                "[wifi] Connected to '{}'. IP: {} | http://openbarista.local | http://{}",
                ssid, ip, ip
            );
            let ip_addr = ip.to_string();
            let wifi = Arc::new(Mutex::new(wifi));
            let stack = WifiStack {
                wifi: wifi.clone(),
                ip_addr: ip_addr.clone(),
                #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
                mdns: start_mdns()?,
            };
            let station_http_server = start_station_http_server(
                &ip_addr,
                telemetry,
                nvs_for_station_server,
                wifi,
                temperature_offset_c.clone(),
                scale_runtime.clone(),
            )?;

            let runtime = WifiRuntime {
                stack,
                station_http_server,
                temperature_offset_c,
            };
            runtime.log_keepalive_state();
            return Ok(runtime);
        }

        println!("[wifi] Could not connect after retries. Starting provisioning portal...");
        {
            let mut progress = lock_or_recover(&connect_progress);
            progress.stage = "failed".to_owned();
            progress.message = "Could not connect after retries. Staying in setup mode.".to_owned();
        }
        drop(connect_server);
        drop(connect_dns_thread);
        wifi.stop()?;
    } else {
        println!("[wifi] No saved credentials. Starting provisioning portal...");
    }

    run_captive_portal(wifi, nvs_partition)?;
    Err(anyhow!(
        "Provisioning portal returned unexpectedly without reboot"
    ))
}

fn start_connecting_status_portal(
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
        let max_body_len = 512usize;
        let body_str = match read_request_body_utf8(&mut req, max_body_len) {
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

        let success_html = web_assets::captive_success_html(&build_for_handler, &board_for_handler);
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
    let pass = nvs.get_str(NVS_PASS_KEY, &mut pass_buf)?.map(str::to_owned);

    Ok((ssid, pass))
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
) -> Result<()> {
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
    let dns_thread = start_captive_dns(AP_GATEWAY)?;

    let ap_ip = wifi.wifi().ap_netif().get_ip_info()?.ip;

    println!(
        "[wifi] SoftAP '{}' started. Connect and visit http://{}",
        AP_SSID, ap_ip
    );

    let build_id_value = build_id().to_owned();
    let board_id_value = board_id();

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
    let build_id_for_handler = build_id_value.clone();
    let board_id_for_handler = board_id_value.clone();

    let server_config = HttpConfig {
        stack_size: 10240,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config)?;

    // Register the setup page on all common captive-portal detection paths so
    // that phones show the "Sign in to network" prompt automatically.
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
        let max_body_len = 512usize;
        let body_str = match read_request_body_utf8(&mut req, max_body_len) {
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

    // Poll loop: refresh nearby SSIDs and reboot once credentials are saved.
    loop {
        thread::sleep(Duration::from_millis(100));

        if matches!(*lock_or_recover(&status), ProvisionStatus::Rebooting) {
            // Give the HTTP response enough time to flush before restarting.
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

pub fn start_station_http_server(
    ip_addr: &str,
    telemetry: SharedTelemetry,
    nvs_partition: EspDefaultNvsPartition,
    _wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>,
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
        // Active scans while connected can destabilize some AP/security combinations
        // (for example MMIE errors). In station mode we serve the saved SSID only.
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
        let max_body_len = 256usize;
        let body_str = match read_request_body_utf8(&mut req, max_body_len) {
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
    let build_for_post = build_id_value.clone();
    let board_for_post = board_id_value.clone();
    let temperature_offset_for_post = temperature_offset_c.clone();
    server.fn_handler("/api/settings", Method::Post, move |mut req| {
        let max_body_len = 512usize;
        let body_str = match read_request_body_utf8(&mut req, max_body_len) {
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
            || parsed_temperature_offset_c.abs() > MAX_TEMP_OFFSET_ABS_C
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
            *lock_or_recover(cache) = names;
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

fn settings_json(
    settings: &DeviceSettings,
    ip_addr: &str,
    build_id: &str,
    board_id: &str,
    ok: bool,
    message: &str,
    rebooting: bool,
) -> String {
    format!(
        "{{\"ok\":{},\"message\":\"{}\",\"rebooting\":{},\"ssid\":\"{}\",\"device_label\":\"{}\",\"temperature_offset_c\":{:.3},\"ip_addr\":\"{}\",\"build_id\":\"{}\",\"board_id\":\"{}\"}}",
        if ok { "true" } else { "false" },
        json_escape(message),
        if rebooting { "true" } else { "false" },
        json_escape(&settings.ssid),
        json_escape(&settings.device_label),
        settings.temperature_offset_c,
        json_escape(ip_addr),
        json_escape(build_id),
        json_escape(board_id),
    )
}

fn telemetry_json(
    seq: u64,
    temperature_c: f32,
    pressure_bar: f32,
    pressure_psi: f32,
    scale_connected: bool,
    weight_g: f32,
    flow_gps: f32,
) -> String {
    let temperature_c = sanitize_telemetry_value(temperature_c);
    let pressure_bar = sanitize_telemetry_value(pressure_bar);
    let pressure_psi = sanitize_telemetry_value(pressure_psi);
    let weight_g = sanitize_telemetry_value(weight_g);
    let flow_gps = sanitize_telemetry_value(flow_gps);

    format!(
        "{{\"seq\":{},\"temperature_c\":{:.3},\"pressure_bar\":{:.3},\"pressure_psi\":{:.3},\"scale_connected\":{},\"weight_g\":{:.3},\"flow_gps\":{:.3}}}",
        seq,
        temperature_c,
        pressure_bar,
        pressure_psi,
        if scale_connected { "true" } else { "false" },
        weight_g,
        flow_gps,
    )
}

fn action_result_json(ok: bool, message: &str) -> String {
    format!(
        "{{\"ok\":{},\"message\":\"{}\"}}",
        if ok { "true" } else { "false" },
        json_escape(message),
    )
}

fn scale_status_json(snapshot: &crate::scale_ble::ScaleStatusSnapshot) -> String {
    let saved_scale = snapshot.saved_scale.as_ref().map_or_else(
        || "null".to_owned(),
        |saved| {
            format!(
                "{{\"address\":\"{}\",\"name\":\"{}\",\"addr_type\":\"{}\"}}",
                json_escape(&saved.address),
                json_escape(&saved.name),
                json_escape(&saved.addr_type),
            )
        },
    );

    let mut devices_json = String::from("[");
    for (idx, device) in snapshot.devices.iter().enumerate() {
        if idx > 0 {
            devices_json.push(',');
        }
        devices_json.push_str(&format!(
            "{{\"address\":\"{}\",\"name\":\"{}\",\"address_type\":\"{}\",\"rssi\":{},\"protocol_hint\":\"{}\",\"saved\":{}}}",
            json_escape(&device.address),
            json_escape(&device.name),
            json_escape(&device.address_type),
            device.rssi,
            json_escape(&device.protocol_hint),
            if device.saved { "true" } else { "false" },
        ));
    }
    devices_json.push(']');

    let battery_json = snapshot
        .battery_percent
        .map(|battery| battery.to_string())
        .unwrap_or_else(|| "null".to_owned());

    format!(
        "{{\"available\":{},\"state\":\"{}\",\"message\":\"{}\",\"connected_name\":\"{}\",\"connected_address\":\"{}\",\"protocol\":\"{}\",\"weight_g\":{:.3},\"flow_gps\":{:.3},\"battery_percent\":{},\"saved_scale\":{},\"devices\":{}}}",
        if snapshot.available { "true" } else { "false" },
        json_escape(&snapshot.state),
        json_escape(&snapshot.message),
        json_escape(&snapshot.connected_name),
        json_escape(&snapshot.connected_address),
        json_escape(&snapshot.protocol),
        sanitize_telemetry_value(snapshot.weight_g),
        sanitize_telemetry_value(snapshot.flow_gps),
        battery_json,
        saved_scale,
        devices_json,
    )
}

fn sanitize_telemetry_value(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
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

fn create_station_netif() -> Result<EspNetif> {
    let mut sta_netif_conf = NetifConfiguration::wifi_default_client();
    sta_netif_conf.ip_configuration = Some(ipv4::Configuration::Client(
        ipv4::ClientConfiguration::DHCP(ipv4::DHCPClientSettings {
            hostname: Some(
                "openbarista"
                    .try_into()
                    .map_err(|_| anyhow!("Invalid hostname"))?,
            ),
        }),
    ));

    Ok(EspNetif::new_with_conf(&sta_netif_conf)?)
}

fn start_captive_dns(ap_gateway: Ipv4Addr) -> Result<CaptiveDnsServer> {
    let socket = UdpSocket::bind((StdIpv4Addr::UNSPECIFIED, 53))
        .map_err(|e| anyhow!("Failed to bind captive DNS on :53: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| anyhow!("Failed to set DNS socket timeout: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    let handle = thread::Builder::new()
        .name("captive-dns".into())
        .spawn(move || {
            let mut rx = [0u8; 512];

            loop {
                if stop_for_thread.load(Ordering::Acquire) {
                    break;
                }
                match socket.recv_from(&mut rx) {
                    Ok((len, peer)) => {
                        if let Some(reply) = build_dns_reply(&rx[..len], ap_gateway) {
                            if let Err(err) = socket.send_to(&reply, peer) {
                                println!("[wifi] Captive DNS send error: {err}");
                            }
                        }
                    }
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        if stop_for_thread.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(err) => {
                        println!("[wifi] Captive DNS stopped: {err}");
                        break;
                    }
                }
            }
        })?;

    Ok(CaptiveDnsServer {
        stop,
        handle: Some(handle),
    })
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
    let qtype = u16::from_be_bytes([query[idx], query[idx + 1]]);
    let question_end = idx + 4;

    // Answer for A (1) and ANY (255); otherwise return NOERROR with ANCOUNT = 0.
    let answer_count: u16 = if qtype == 1 || qtype == 255 { 1 } else { 0 };

    let mut reply =
        Vec::with_capacity(12 + (question_end - 12) + if answer_count == 1 { 16 } else { 0 });
    // ID
    reply.extend_from_slice(&query[0..2]);
    // Flags: standard response, recursion desired and available, no error (0x8180)
    reply.extend_from_slice(&0x8180u16.to_be_bytes());
    // QDCOUNT, ANCOUNT, NSCOUNT, ARCOUNT
    reply.extend_from_slice(&1u16.to_be_bytes());
    reply.extend_from_slice(&answer_count.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes());
    // Original question
    reply.extend_from_slice(&query[12..question_end]);

    if answer_count == 1 {
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
    }
    Some(reply)
}

// ---------------------------------------------------------------------------
// URL-encoded form helpers
// ---------------------------------------------------------------------------

fn parse_form_field(body: &str, key: &str) -> Option<String> {
    form_urlencoded::parse(body.as_bytes()).find_map(|(k, v)| {
        if k == key {
            Some(v.into_owned())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_form_field_basic() {
        let body = "ssid=my%20network&password=secret+pass";
        assert_eq!(
            parse_form_field(body, "ssid"),
            Some("my network".to_string())
        );
        assert_eq!(
            parse_form_field(body, "password"),
            Some("secret pass".to_string())
        );
    }

    #[test]
    fn test_parse_form_field_missing_key() {
        let body = "ssid=myssid&password=secret";
        assert_eq!(parse_form_field(body, "unknown"), None);
    }

    #[test]
    fn test_parse_form_field_empty_password_for_open_network() {
        // Models an open network configuration where password is intentionally empty.
        let body = "ssid=open_network&password=&mode=open";
        let password = parse_form_field(body, "password");
        assert_eq!(password, Some(String::new()));
    }
}
