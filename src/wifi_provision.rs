use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr as StdIpv4Addr, UdpSocket},
};

use anyhow::{anyhow, Result};
use embedded_svc::ipv4::{self, Ipv4Addr, Mask, Subnet};
use embedded_svc::http::Headers;
use esp_idf_hal::modem::Modem;
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
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
use esp_idf_svc::mdns::EspMdns;

const NVS_NAMESPACE: &str = "wifi";
const NVS_SSID_KEY: &str = "ssid";
const NVS_PASS_KEY: &str = "pass";

const AP_SSID: &str = "OpenBarista";
const AP_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
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

const PORTAL_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>OpenBarista Setup</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
      background: #1a0a00; min-height: 100vh;
      display: flex; align-items: center; justify-content: center; padding: 1rem;
    }
    .card {
      background: #fff; border-radius: 12px; padding: 2rem;
      max-width: 400px; width: 100%; box-shadow: 0 8px 32px rgba(0,0,0,0.4);
    }
    h1 { color: #b85c00; margin-bottom: 0.25rem; font-size: 1.6rem; }
    .subtitle { color: #666; font-size: 0.9rem; margin-bottom: 1.5rem; }
    label { display: block; font-size: 0.85rem; font-weight: 600; color: #333; margin-bottom: 0.25rem; }
        .row { display: flex; gap: 0.5rem; margin-bottom: 0.75rem; }
    input {
      width: 100%; padding: 0.65rem 0.75rem;
      border: 1.5px solid #ddd; border-radius: 6px; font-size: 1rem; margin-bottom: 1rem;
    }
        select {
            width: 100%; padding: 0.65rem 0.75rem;
            border: 1.5px solid #ddd; border-radius: 6px; font-size: 1rem;
            background: #fff;
        }
    input:focus { outline: none; border-color: #b85c00; }
        select:focus { outline: none; border-color: #b85c00; }
    button {
      width: 100%; padding: 0.75rem; background: #b85c00; color: white;
      border: none; border-radius: 6px; font-size: 1rem; font-weight: 600; cursor: pointer;
    }
        .btn-secondary {
            width: auto;
            padding: 0.6rem 0.8rem;
            background: #eee;
            color: #333;
            border: 1px solid #ddd;
            font-size: 0.85rem;
        }
    button:hover { background: #9a4d00; }
        .btn-secondary:hover { background: #e5e5e5; }
        .status {
            font-size: 0.85rem;
            color: #666;
            margin-top: -0.5rem;
            margin-bottom: 0.9rem;
        }
  </style>
</head>
<body>
  <div class="card">
    <h1>&#9749; OpenBarista</h1>
    <p class="subtitle">Connect your device to your home WiFi network.</p>
    <form method="POST" action="/connect">
            <label for="networkSelect">Nearby Networks</label>
            <div class="row">
                <select id="networkSelect" aria-label="Nearby networks">
                    <option value="">Scanning...</option>
                </select>
                <button class="btn-secondary" type="button" onclick="refreshNetworks()">Refresh</button>
            </div>
      <label for="ssid">WiFi Network Name (SSID)</label>
      <input type="text" id="ssid" name="ssid" required maxlength="32"
             autocomplete="off" autocorrect="off" spellcheck="false">
            <p id="netStatus" class="status">Scanning nearby networks...</p>
      <label for="password">Password</label>
      <input type="password" id="password" name="password" maxlength="64" autocomplete="off">
      <button type="submit">Connect Device</button>
    </form>
  </div>
    <script>
        const select = document.getElementById('networkSelect');
        const ssidInput = document.getElementById('ssid');
        const status = document.getElementById('netStatus');

        select.addEventListener('change', () => {
            if (select.value) {
                ssidInput.value = select.value;
            }
        });

        async function refreshNetworks() {
            status.textContent = 'Refreshing list...';
            try {
                const resp = await fetch('/networks', { cache: 'no-store' });
                const items = JSON.parse(await resp.text());
                select.innerHTML = '';

                if (!Array.isArray(items) || items.length === 0) {
                    const opt = document.createElement('option');
                    opt.value = '';
                    opt.textContent = 'No networks found';
                    select.appendChild(opt);
                    status.textContent = 'No networks found. You can still type SSID manually.';
                    return;
                }

                const placeholder = document.createElement('option');
                placeholder.value = '';
                placeholder.textContent = 'Select a network';
                select.appendChild(placeholder);

                items.forEach((ssid) => {
                    const opt = document.createElement('option');
                    opt.value = ssid;
                    opt.textContent = ssid;
                    select.appendChild(opt);
                });

                status.textContent = `Found ${items.length} network(s).`;
            } catch (e) {
                status.textContent = 'Could not load networks right now. Enter SSID manually.';
            }
        }

        refreshNetworks();
    </script>
</body>
</html>"#;

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>OpenBarista &#8212; Connecting</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
      background: #1a0a00; min-height: 100vh;
      display: flex; align-items: center; justify-content: center; padding: 1rem;
    }
    .card {
      background: #fff; border-radius: 12px; padding: 2rem;
      max-width: 400px; width: 100%; box-shadow: 0 8px 32px rgba(0,0,0,0.4); text-align: center;
    }
    h1 { color: #b85c00; margin-bottom: 1rem; }
    p { color: #444; line-height: 1.7; margin-bottom: 0.75rem; }
    code {
      background: #f5f0eb; padding: 0.15rem 0.4rem;
      border-radius: 4px; font-family: monospace; color: #b85c00;
    }
  </style>
</head>
<body>
  <div class="card">
    <h1>&#9749; Connecting&hellip;</h1>
    <p>Credentials saved. The device is restarting.</p>
    <p>
      Reconnect your phone or laptop to your home WiFi, then visit<br>
      <code>http://openbarista.local</code>
    </p>
    <p style="color:#888;font-size:0.85rem">This may take up to 30 seconds.</p>
  </div>
</body>
</html>"#;

#[derive(Clone)]
enum ProvisionStatus {
    Idle,
    Validating,
    Failed(String),
    Rebooting,
}

/// Holds the active WiFi driver and optional mDNS handle.
/// Both must remain alive for the duration of the program.
pub struct WifiStack {
    #[allow(dead_code)]
    pub wifi: BlockingWifi<EspWifi<'static>>,
    #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
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
        println!("[wifi] Saved credentials found for '{}'. Connecting...", ssid);

        let h_ssid = ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow!("Saved SSID is too long (max 32 chars)"))?;
        let h_pass = pass
            .as_str()
            .try_into()
            .map_err(|_| anyhow!("Saved password is too long (max 64 chars)"))?;
        let auth = if pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        };

        wifi.set_configuration(&WifiConfig::Client(ClientConfiguration {
            ssid: h_ssid,
            password: h_pass,
            auth_method: auth,
            ..Default::default()
        }))?;
        wifi.start()?;

        if try_connect(&mut wifi, &ssid) {
            wifi.wait_netif_up()?;
            println!("[wifi] Connected. Advertising as {MDNS_HOSTNAME}.local ...");
            #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
            let mdns = start_mdns()?;
            return Ok(WifiStack {
                wifi,
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
    let credentials: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let creds_for_handler = credentials.clone();

    // Nearby SSIDs cache for the setup page.
    let networks_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let networks_for_handler = networks_cache.clone();
    let scan_requested: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    let scan_requested_for_handler = scan_requested.clone();

    let status: Arc<Mutex<ProvisionStatus>> = Arc::new(Mutex::new(ProvisionStatus::Idle));
    let status_for_handler = status.clone();

    let server_config = HttpConfig {
        stack_size: 10240,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config)?;

    // Register the setup page on all common captive-portal detection paths so
    // that phones show the "Sign in to network" prompt automatically.
    for path in CAPTIVE_PATHS {
        server.fn_handler(path, Method::Get, |req| {
            req.into_ok_response()?.write_all(PORTAL_HTML.as_bytes())?;
            Ok::<_, anyhow::Error>(())
        })?;
    }

    server.fn_handler("/networks", Method::Get, move |req| {
        *scan_requested_for_handler.lock().unwrap() = true;
        let networks = networks_for_handler.lock().unwrap().clone();
        let payload = networks_json(&networks);
        req.into_ok_response()?.write_all(payload.as_bytes())?;
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
            req.into_ok_response()?.write_all(
                b"<html><body><p>SSID cannot be empty.</p><a href='/'>Go back</a></body></html>",
            )?;
        } else {
            *creds_for_handler.lock().unwrap() = Some((ssid, pass));

            {
                let mut state = status_for_handler.lock().unwrap();
                *state = ProvisionStatus::Validating;
            }

            // Wait for validation to finish before responding.
            let mut response = String::from(
                "<html><body><p>Validating WiFi credentials...</p><p>Please wait.</p></body></html>",
            );
            for _ in 0..250 {
                thread::sleep(Duration::from_millis(100));
                let state = status_for_handler.lock().unwrap().clone();
                match state {
                    ProvisionStatus::Validating | ProvisionStatus::Idle => {}
                    ProvisionStatus::Rebooting => {
                        response = SUCCESS_HTML.to_owned();
                        break;
                    }
                    ProvisionStatus::Failed(message) => {
                        response = error_html(&message);
                        break;
                    }
                }
            }

            req.into_ok_response()?.write_all(response.as_bytes())?;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    // Poll for submitted credentials, then save and restart.
    loop {
        thread::sleep(Duration::from_millis(100));

        let validating = matches!(*status.lock().unwrap(), ProvisionStatus::Validating);
        if !validating && *scan_requested.lock().unwrap() {
            *scan_requested.lock().unwrap() = false;
            refresh_network_cache(&mut wifi, &networks_cache);
        }

        if let Some((ssid, pass)) = credentials.lock().unwrap().take() {
            println!("[wifi] Credentials received for '{}'. Validating...", ssid);

            let is_valid = validate_station_credentials(&mut wifi, &ap_config, &ssid, &pass);
            if !is_valid {
                println!("[wifi] Credentials failed for '{}'.", ssid);
                *status.lock().unwrap() = ProvisionStatus::Failed(
                    "Could not connect to that network. Check SSID/password and try again."
                        .to_owned(),
                );
                restore_portal_mode(&mut wifi, &ap_config)?;
                continue;
            }

            println!("[wifi] Credentials valid for '{}'. Saving to NVS...", ssid);
            *status.lock().unwrap() = ProvisionStatus::Rebooting;

            let nvs = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)?;
            nvs.set_str(NVS_SSID_KEY, &ssid)?;
            nvs.set_str(NVS_PASS_KEY, &pass)?;
            drop(nvs);
            drop(server);

            // Give the HTTP response a moment to fully transmit before restart.
            thread::sleep(Duration::from_millis(500));

            println!("[wifi] Restarting device...");
            unsafe { esp_idf_svc::sys::esp_restart() };
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

fn validate_station_credentials(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ap_config: &AccessPointConfiguration,
    ssid: &str,
    pass: &str,
) -> bool {
    let auth = if pass.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };

    for attempt in 1..=3 {
        println!("[wifi] Validation attempt {attempt}/3 for '{ssid}'");
        let _ = wifi.stop();

        if wifi
            .set_configuration(&WifiConfig::Mixed(
                ClientConfiguration {
                    ssid: match ssid.try_into() {
                        Ok(v) => v,
                        Err(_) => return false,
                    },
                    password: match pass.try_into() {
                        Ok(v) => v,
                        Err(_) => return false,
                    },
                    auth_method: auth,
                    ..Default::default()
                },
                ap_config.clone(),
            ))
            .is_err()
        {
            continue;
        }
        if wifi.start().is_err() {
            continue;
        }

        if wifi.connect().is_ok() && wifi.wait_netif_up().is_ok() {
            return true;
        }

        thread::sleep(Duration::from_secs(2));
    }

    false
}

fn restore_portal_mode(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ap_config: &AccessPointConfiguration,
) -> Result<()> {
    let _ = wifi.stop();
    wifi.set_configuration(&WifiConfig::Mixed(
        ClientConfiguration::default(),
        ap_config.clone(),
    ))?;
    wifi.start()?;
    Ok(())
}

fn refresh_network_cache(wifi: &mut BlockingWifi<EspWifi<'static>>, cache: &Arc<Mutex<Vec<String>>>) {
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

fn error_html(message: &str) -> String {
    format!(
        "<html><body><h3>Connection failed</h3><p>{}</p><p><a href='/'>Try again</a></p></body></html>",
        message
    )
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
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                }
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
