mod captive;
mod http;
mod nvs;
mod station;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_svc::ipv4::{self, Ipv4Addr, Mask, Subnet};
use esp_idf_hal::modem::WifiModemPeripheral;
#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::EspHttpServer,
    netif::{EspNetif, NetifConfiguration},
    nvs::EspDefaultNvsPartition,
    wifi::{
        AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration,
        Configuration as WifiConfig, EspWifi, WifiDriver,
    },
};

use crate::scale_ble::ScaleRuntime;
use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use self::captive::{run_captive_portal, start_captive_dns};
use self::http::{build_id, board_id};
use self::nvs::{read_device_settings, read_saved_credentials};
use self::station::{start_connecting_status_portal, start_station_http_server};

const AP_SSID: &str = "OpenBarista";
const AP_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const MAX_TEMP_OFFSET_ABS_C: f32 = 20.0;

#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
const MDNS_HOSTNAME: &str = "openbarista";

const CAPTIVE_PATHS: &[&str] = &[
    "/",
    "/generate_204",
    "/hotspot-detect.html",
    "/fwlink",
    "/connecttest.txt",
    "/ncsi.txt",
    "/redirect",
];

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct WifiStack {
    pub wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>,
    pub ip_addr: String,
    #[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
    pub mdns: EspMdns,
}

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

// ---------------------------------------------------------------------------
// WiFi setup entry point
// ---------------------------------------------------------------------------

pub fn setup_wifi<M>(
    modem: M,
    nvs_partition: EspDefaultNvsPartition,
    telemetry: SharedTelemetry,
    scale_runtime: Arc<ScaleRuntime>,
) -> Result<WifiRuntime>
where
    M: WifiModemPeripheral + 'static,
{
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_for_station_server = nvs_partition.clone();
    let initial_settings = read_device_settings(&nvs_for_station_server)?;
    let temperature_offset_c = Arc::new(Mutex::new(initial_settings.temperature_offset_c));
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
                scale_runtime,
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

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

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

#[cfg(any(esp_idf_comp_mdns_enabled, esp_idf_comp_espressif__mdns_enabled))]
fn start_mdns() -> Result<EspMdns> {
    let mut mdns = EspMdns::take()?;
    mdns.set_hostname(MDNS_HOSTNAME)?;
    mdns.set_instance_name("OpenBarista")?;
    mdns.add_service(None, "_http", "_tcp", 80, &[])?;
    Ok(mdns)
}

fn create_softap_netif(ap_gateway: Ipv4Addr) -> Result<EspNetif> {
    let mut ap_netif_conf = NetifConfiguration::wifi_default_router();
    ap_netif_conf.ip_configuration = Some(ipv4::Configuration::Router(ipv4::RouterConfiguration {
        subnet: Subnet {
            gateway: ap_gateway,
            mask: Mask(24),
        },
        dhcp_enabled: true,
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
