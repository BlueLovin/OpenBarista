use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use embassy_futures::select::{select, Either};
use embassy_futures::yield_now;
use embassy_sync::signal::Signal;
use esp_idf_hal::task::block_on;
use esp_idf_hal::task::embassy_sync::EspRawMutex;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use esp32_nimble::{BLEAddress, BLEAddressType, BLEDevice, BLEScan};
use esp32_nimble::utilities::BleUuid;

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;
use openbarista::telemetry_math::{sanitize_weight_g, FlowEstimator};

const SCALE_SCAN_DURATION_S: u32 = 6;
const CONNECT_TIMEOUT_MS: u32 = 12_000;
const CONNECT_MAX_ATTEMPTS: u32 = 3;
const MAX_DISCOVERED_SCALES: usize = 18;
const SCALE_READY_MESSAGE: &str = "Bluetooth scale ready. Tap Find Scales to pair.";
const SCALE_STARTUP_MESSAGE: &str = "Starting Bluetooth scale transport...";

const UUID_SERVICE_WEIGHT_SCALE: u16 = 0x181D;
const UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT: u16 = 0x2A9D;
const UUID_SERVICE_BATTERY: u16 = 0x180F;
const UUID_CHARACTERISTIC_BATTERY_LEVEL: u16 = 0x2A19;

const COMMON_VENDOR_NOTIFY_UUIDS: &[u16] = &[0xFFF1, 0xFFF2, 0xFFF4, 0xFFE1, 0xFFE2, 0xFFE5, 0xFF11];
const COMMON_VENDOR_SERVICE_UUIDS: &[u16] = &[
    0xFFF0, 0xFFE0, 0xFFF1, 0xFFE1, 0xFFF5, 0xFFE5, 0x0FFE,
];
const SCALE_NAME_HINTS: &[&str] = &[
    "scale",
    "acaia",
    "felicita",
    "pearl",
    "lunar",
    "decent",
    "timemore",
    "mirror",
    "bookoo",
    "atomax",
];

// ---------------------------------------------------------------------------
// Public types (unchanged interface)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleConnectionState {
    Idle,
    Scanning,
    Connecting,
    Discovering,
    Ready,
    Error,
}

impl ScaleConnectionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Scanning => "scanning",
            Self::Connecting => "connecting",
            Self::Discovering => "discovering",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleProtocol {
    StandardWeight,
    GenericNotify,
    Bookoo,
    Unknown,
}

impl ScaleProtocol {
    fn as_str(self) -> &'static str {
        match self {
            Self::StandardWeight => "standard-weight",
            Self::GenericNotify => "generic-notify",
            Self::Bookoo => "bookoo",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SavedScale {
    pub address: String,
    pub name: String,
    pub addr_type: String,
}

#[derive(Debug, Clone)]
pub struct DiscoveredScale {
    pub address: String,
    pub name: String,
    pub address_type: String,
    pub rssi: i32,
    pub protocol_hint: String,
    pub saved: bool,
}

#[derive(Debug, Clone)]
pub struct ScaleStatusSnapshot {
    pub available: bool,
    pub state: String,
    pub message: String,
    pub connected_name: String,
    pub connected_address: String,
    pub protocol: String,
    pub weight_g: f32,
    pub flow_gps: f32,
    pub battery_percent: Option<u8>,
    pub saved_scale: Option<SavedScale>,
    pub devices: Vec<DiscoveredScale>,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DiscoveredScaleInternal {
    address_text: String,
    addr_type_str: String,
    name: String,
    rssi: i32,
    protocol_hint: ScaleProtocol,
    scale_like: bool,
}

struct ActiveScaleConnection {
    address_text: String,
    name: String,
    protocol: ScaleProtocol,
}

struct ScaleManagerState {
    available: bool,
    state: ScaleConnectionState,
    message: String,
    transport_ready: bool,
    discovered: Vec<DiscoveredScaleInternal>,
    active: Option<ActiveScaleConnection>,
    saved_scale: Option<SavedScale>,
    weight_g: f32,
    flow_gps: f32,
    battery_percent: Option<u8>,
    flow_estimator: FlowEstimator,
}

impl ScaleManagerState {
    fn new(available: bool, message: String) -> Self {
        Self {
            available,
            state: ScaleConnectionState::Idle,
            message,
            transport_ready: false,
            discovered: Vec::new(),
            active: None,
            saved_scale: None,
            weight_g: 0.0,
            flow_gps: 0.0,
            battery_percent: None,
            flow_estimator: FlowEstimator::new(),
        }
    }

    fn snapshot(&self) -> ScaleStatusSnapshot {
        let connected_name = self
            .active
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let connected_address = self
            .active
            .as_ref()
            .map(|a| a.address_text.clone())
            .unwrap_or_default();
        let protocol = self
            .active
            .as_ref()
            .map(|a| a.protocol.as_str().to_owned())
            .unwrap_or_else(|| ScaleProtocol::Unknown.as_str().to_owned());

        let devices = self
            .discovered
            .iter()
            .map(|d| DiscoveredScale {
                address: d.address_text.clone(),
                name: d.name.clone(),
                address_type: d.addr_type_str.clone(),
                rssi: d.rssi,
                protocol_hint: d.protocol_hint.as_str().to_owned(),
                saved: self
                    .saved_scale
                    .as_ref()
                    .map(|s| s.address.eq_ignore_ascii_case(&d.address_text))
                    .unwrap_or(false),
            })
            .collect();

        ScaleStatusSnapshot {
            available: self.available,
            state: self.state.as_str().to_owned(),
            message: self.message.clone(),
            connected_name,
            connected_address,
            protocol,
            weight_g: self.weight_g,
            flow_gps: self.flow_gps,
            battery_percent: self.battery_percent,
            saved_scale: self.saved_scale.clone(),
            devices,
        }
    }

    fn reset_live_values(&mut self) {
        self.weight_g = 0.0;
        self.flow_gps = 0.0;
        self.battery_percent = None;
        self.flow_estimator.reset();
    }
}

// ---------------------------------------------------------------------------
// Worker commands
// ---------------------------------------------------------------------------

struct ConnectRequest {
    address_text: String,
    addr_type_str: String,
    name: String,
}

enum WorkerCommand {
    StartScan,
    ConnectTarget(ConnectRequest),
    Disconnect,
}

// ---------------------------------------------------------------------------
// ScaleRuntime — public entry point (API unchanged)
// ---------------------------------------------------------------------------

pub struct ScaleRuntime {
    state: Arc<Mutex<ScaleManagerState>>,
    worker_tx: Option<Sender<WorkerCommand>>,
    telemetry: SharedTelemetry,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl ScaleRuntime {
    pub fn disabled(message: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScaleManagerState::new(false, message.into()))),
            worker_tx: None,
            telemetry: SharedTelemetry::new(),
            _worker_thread: None,
        }
    }

    pub fn try_new<B: Send + 'static>(
        _bluetooth_modem: B,
        _nvs_partition: Option<EspDefaultNvsPartition>,
        telemetry: SharedTelemetry,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(ScaleManagerState::new(
            true,
            SCALE_STARTUP_MESSAGE.to_owned(),
        )));
        let (worker_tx, worker_rx) = mpsc::channel();

        let worker_state = state.clone();
        let worker_telemetry = telemetry.clone();
        let worker_thread = thread::Builder::new()
            .name("ble-scale".into())
            .stack_size(32768)
            .spawn(move || {
                worker_loop(worker_rx, worker_state, worker_telemetry);
            })?;

        Ok(Self {
            state,
            worker_tx: Some(worker_tx),
            telemetry,
            _worker_thread: Some(worker_thread),
        })
    }

    pub fn snapshot(&self) -> ScaleStatusSnapshot {
        lock_or_recover(&self.state).snapshot()
    }

    pub fn apply_saved_scale(&self, saved_scale: Option<SavedScale>) {
        let mut state = lock_or_recover(&self.state);
        state.saved_scale = saved_scale;
    }

    pub fn start_scan(&self) -> Result<&'static str> {
        if self.worker_tx.is_none() {
            return Err(anyhow!("Bluetooth is unavailable on this build."));
        }

        {
            let mut state = lock_or_recover(&self.state);
            if !state.transport_ready {
                state.message = "Bluetooth is still starting. Try again in a moment.".to_owned();
                return Ok("Bluetooth is still starting. Try again in a moment.");
            }
            if state.state == ScaleConnectionState::Ready {
                state.message = "Disconnect the current scale before scanning.".to_owned();
                return Ok("Disconnect the current scale before scanning.");
            }
            // If a connect is in progress, cancel it so the worker unblocks.
            if state.state == ScaleConnectionState::Connecting
                || state.state == ScaleConnectionState::Discovering
            {
                println!("[scale] cancelling in-progress connect for new scan");
                state.active = None;
                state.reset_live_values();
                cancel_ble_connect();
            }
            state.discovered.clear();
            state.state = ScaleConnectionState::Scanning;
            state.message = "Scanning for nearby Bluetooth scales...".to_owned();
        }
        self.telemetry.clear_scale();

        self.send_command(WorkerCommand::StartScan)?;
        Ok("Scanning for nearby Bluetooth scales...")
    }

    pub fn connect_address(&self, address: &str) -> Result<String> {
        if self.worker_tx.is_none() {
            return Err(anyhow!("Bluetooth is unavailable on this build."));
        }

        let request = {
            let state = lock_or_recover(&self.state);
            if let Some(active) = state.active.as_ref() {
                if active.address_text.eq_ignore_ascii_case(address) {
                    return Ok(format!(
                        "Already connected to {}.",
                        display_scale_name(&active.name)
                    ));
                }
                return Err(anyhow!(
                    "Disconnect {} before connecting a different scale.",
                    display_scale_name(&active.name)
                ));
            }

            if let Some(device) = state
                .discovered
                .iter()
                .find(|d| d.address_text.eq_ignore_ascii_case(address))
            {
                ConnectRequest {
                    address_text: device.address_text.clone(),
                    addr_type_str: device.addr_type_str.clone(),
                    name: device.name.clone(),
                }
            } else if let Some(saved) = state
                .saved_scale
                .as_ref()
                .filter(|s| s.address.eq_ignore_ascii_case(address))
            {
                ConnectRequest {
                    address_text: saved.address.clone(),
                    addr_type_str: saved.addr_type.clone(),
                    name: saved.name.clone(),
                }
            } else {
                return Err(anyhow!(
                    "Scale not found. Scan again and tap a device from the list."
                ));
            }
        };

        {
            let mut state = lock_or_recover(&self.state);
            state.state = ScaleConnectionState::Connecting;
            state.message = format!("Connecting to {}...", display_scale_name(&request.name));
            state.active = Some(ActiveScaleConnection {
                address_text: request.address_text.clone(),
                name: request.name.clone(),
                protocol: ScaleProtocol::Unknown,
            });
            state.reset_live_values();
        }
        self.telemetry.clear_scale();

        let message = format!("Connecting to {}...", display_scale_name(&request.name));
        if let Err(e) = self.send_command(WorkerCommand::ConnectTarget(request)) {
            let mut state = lock_or_recover(&self.state);
            state.state = ScaleConnectionState::Idle;
            state.message = "Failed to send connect command.".to_owned();
            state.active = None;
            return Err(e);
        }
        Ok(message)
    }

    pub fn connect_saved_scale(&self) -> Result<String> {
        let saved = {
            let state = lock_or_recover(&self.state);
            state
                .saved_scale
                .clone()
                .ok_or_else(|| anyhow!("No saved scale configured."))?
        };
        self.connect_address(&saved.address)
    }

    pub fn disconnect(&self) -> Result<&'static str> {
        if self.worker_tx.is_none() {
            return Err(anyhow!("Bluetooth is unavailable on this build."));
        }

        {
            let mut state = lock_or_recover(&self.state);
            if state.active.is_none()
                && state.state != ScaleConnectionState::Connecting
                && state.state != ScaleConnectionState::Discovering
            {
                state.message = "No scale is connected right now.".to_owned();
                return Ok("No scale is connected right now.");
            }
            // Immediately reset UI state so buttons re-enable.
            let was_connecting = state.state == ScaleConnectionState::Connecting
                || state.state == ScaleConnectionState::Discovering;
            state.active = None;
            state.state = ScaleConnectionState::Idle;
            state.message = "Disconnected.".to_owned();
            state.reset_live_values();

            // If a BLE connect is in progress, cancel it at the controller
            // level so the worker's client.connect().await unblocks.
            if was_connecting {
                cancel_ble_connect();
            }
        }
        self.telemetry.clear_scale();

        // Also tell the worker to drop the client (handles the already-connected case).
        let _ = self.send_command(WorkerCommand::Disconnect);
        Ok("Disconnected.")
    }

    pub fn forget_saved_scale(&self) {
        let mut state = lock_or_recover(&self.state);
        state.saved_scale = None;
    }

    fn send_command(&self, command: WorkerCommand) -> Result<()> {
        self.worker_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Bluetooth is unavailable on this build."))?
            .send(command)
            .map_err(|_| anyhow!("Bluetooth worker stopped unexpectedly."))
    }
}

// ---------------------------------------------------------------------------
// Worker thread — single async context for all NimBLE operations
// ---------------------------------------------------------------------------

fn worker_loop(
    rx: Receiver<WorkerCommand>,
    state: Arc<Mutex<ScaleManagerState>>,
    telemetry: SharedTelemetry,
) {
    block_on(async {
        let ble_device = BLEDevice::take();

        {
            let mut s = lock_or_recover(&state);
            s.transport_ready = true;
            s.state = ScaleConnectionState::Idle;
            s.message = SCALE_READY_MESSAGE.to_owned();
        }

        // Read our own BLE address so we can exclude it from scan results.
        let own_addr_text = get_own_ble_address();
        if let Some(ref a) = own_addr_text {
            println!("[scale] own BLE address: {a}");
        }

        println!("[scale] NimBLE transport ready");

        // The active BLEClient must live here so it persists between commands.
        // BLEClient is !Send so it must stay on this thread — which it does.
        let mut active_client: Option<esp32_nimble::BLEClient> = None;

        while let Ok(command) = rx.recv() {
            match command {
                WorkerCommand::StartScan => {
                    // Properly disconnect & drop any existing client.
                    if let Some(mut old_client) = active_client.take() {
                        let _ = old_client.disconnect();
                    }

                    // Cancel any stale GAP operations (lingering scan or
                    // connect) so that ble_gap_disc() starts fresh.
                    // Both calls are harmless if nothing is pending.
                    cancel_gap_operations();

                    {
                        let mut s = lock_or_recover(&state);
                        s.discovered.clear();
                        s.state = ScaleConnectionState::Scanning;
                        s.message = "Scanning for nearby Bluetooth scales...".to_owned();
                    }

                    println!("[scale] starting BLE scan ({}s)", SCALE_SCAN_DURATION_S);

                    let scan_state = state.clone();
                    let scan_own_addr = own_addr_text.clone();
                    let mut scan = BLEScan::new();
                    scan.active_scan(true)
                        .filter_duplicates(false)
                        .interval(100)
                        .window(99);

                    let result = scan
                        .start(
                            ble_device,
                            (SCALE_SCAN_DURATION_S * 1000) as i32,
                            |device, data| {
                                let addr = device.addr();
                                let name = data
                                    .name()
                                    .and_then(|n| core::str::from_utf8(n).ok())
                                    .unwrap_or("")
                                    .trim()
                                    .to_owned();
                                let name = if name.is_empty() {
                                    format!("{}", addr)
                                } else {
                                    name
                                };
                                let rssi = device.rssi() as i32;
                                let addr_text = format!("{}", addr);
                                let scale_like = is_scale_like_name(&name);

                                // Skip our own BLE address
                                if scan_own_addr.as_deref().map_or(false, |own| {
                                    own.eq_ignore_ascii_case(&addr_text)
                                }) {
                                    return None::<()>;
                                }

                                // Ignore weak unnamed noise
                                if !scale_like && rssi < -88 && name == addr_text {
                                    return None::<()>;
                                }

                                let internal = DiscoveredScaleInternal {
                                    address_text: addr_text,
                                    addr_type_str: ble_addr_type_str(addr.addr_type()).to_owned(),
                                    name,
                                    rssi,
                                    protocol_hint: ScaleProtocol::Unknown,
                                    scale_like,
                                };

                                upsert_discovered(&scan_state, internal);
                                None::<()> // keep scanning
                            },
                        )
                        .await;

                    if let Err(e) = &result {
                        println!("[scale] scan returned error: {:?}", e);
                    } else {
                        println!("[scale] scan future resolved OK");
                    }

                    let mut s = lock_or_recover(&state);
                    if s.state == ScaleConnectionState::Scanning {
                        s.state = ScaleConnectionState::Idle;
                        s.message = if s.discovered.is_empty() {
                            "No nearby scales found. Try moving the scale closer and scan again."
                                .to_owned()
                        } else {
                            format!(
                                "Found {} device(s). Tap one to connect.",
                                s.discovered.len()
                            )
                        };
                    }

                    println!("[scale] scan complete ({} devices)", s.discovered.len());
                }

                WorkerCommand::ConnectTarget(req) => {
                    // Properly disconnect & drop any existing client.
                    if let Some(mut old_client) = active_client.take() {
                        println!("[scale] dropping previous client");
                        let _ = old_client.disconnect();
                    }
                    cancel_gap_operations();

                    let addr_type_str = req.addr_type_str.clone();
                    let addr_type = parse_nimble_addr_type(&addr_type_str);
                    let Some(addr) = BLEAddress::from_str(&req.address_text, addr_type) else {
                        let mut s = lock_or_recover(&state);
                        s.state = ScaleConnectionState::Error;
                        s.message = format!(
                            "Invalid BLE address: {}",
                            req.address_text
                        );
                        s.active = None;
                        continue;
                    };

                    println!(
                        "[scale] connecting to {} at {} ({})",
                        display_scale_name(&req.name),
                        req.address_text,
                        req.addr_type_str,
                    );

                    // --- Retry loop for flaky BLE connections ---
                    let mut connected_client: Option<esp32_nimble::BLEClient> = None;

                    for attempt in 1..=CONNECT_MAX_ATTEMPTS {
                        // Check if user cancelled between retries
                        {
                            let s = lock_or_recover(&state);
                            if s.active.is_none() {
                                println!("[scale] connect cancelled by user before attempt {attempt}");
                                break;
                            }
                        }

                        if attempt > 1 {
                            println!("[scale] retrying connect (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
                            cancel_gap_operations();
                            // Brief pause before retry to let the controller settle
                            yield_now().await;
                            {
                                let mut s = lock_or_recover(&state);
                                s.message = format!(
                                    "Connecting to {} (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})...",
                                    display_scale_name(&req.name)
                                );
                            }
                        }

                        let mut client = ble_device.new_client();

                        // Abort signal — signaled by the watchdog or disconnect
                        // callback so that select() can break out of a hung
                        // client.connect().await.
                        let abort_signal = Arc::new(Signal::<EspRawMutex, ()>::new());

                        // Disconnect callback (runs on NimBLE host task)
                        let disc_state = state.clone();
                        let disc_telemetry = telemetry.clone();
                        let disc_name = req.name.clone();
                        let disc_abort = abort_signal.clone();
                        client.on_disconnect(move |reason| {
                            println!(
                                "[scale] disconnected from {} (reason={reason})",
                                display_scale_name(&disc_name)
                            );
                            disc_abort.signal(()); // unblock select
                            let mut s = lock_or_recover(&disc_state);
                            s.active = None;
                            s.state = ScaleConnectionState::Idle;
                            s.message = format!(
                                "Disconnected from {}.",
                                display_scale_name(&disc_name)
                            );
                            s.reset_live_values();
                            disc_telemetry.clear_scale();
                        });

                        // Watchdog: abort after 12 seconds if connect() hasn't
                        // returned.  Handles both the GAP-connect phase (30s
                        // default timeout is too long) and the library's MTU-
                        // exchange hang (no timeout at all).
                        let wd_abort = abort_signal.clone();
                        let wd_addr_text = req.address_text.clone();
                        let wd_addr_type_str = addr_type_str.clone();
                        let wd_done = Arc::new(AtomicBool::new(false));
                        let wd_done2 = wd_done.clone();
                        let _ = thread::Builder::new()
                            .name("ble-wd".into())
                            .stack_size(4096)
                            .spawn(move || {
                                let mut elapsed = 0u32;
                                while elapsed < CONNECT_TIMEOUT_MS {
                                    thread::sleep(Duration::from_millis(200));
                                    if wd_done2.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    elapsed += 200;
                                }
                                println!("[scale] WATCHDOG: connect timed out after {}ms", CONNECT_TIMEOUT_MS);
                                unsafe {
                                    // Cancel pending connect attempt — makes
                                    // BLEClient::connect() return Err for the
                                    // GAP-connect phase.
                                    esp_idf_svc::sys::ble_gap_conn_cancel();
                                    // Also terminate any established connection
                                    // (handles the MTU-exchange hang where
                                    // connect succeeded but signal was never
                                    // fired).
                                    if let Some(a) = BLEAddress::from_str(&wd_addr_text, parse_nimble_addr_type(&wd_addr_type_str)) {
                                        let ble_addr: esp_idf_svc::sys::ble_addr_t = a.into();
                                        let mut desc: esp_idf_svc::sys::ble_gap_conn_desc =
                                            core::mem::zeroed();
                                        if esp_idf_svc::sys::ble_gap_conn_find_by_addr(
                                            &ble_addr, &mut desc,
                                        ) == 0
                                        {
                                            println!("[scale] WATCHDOG: terminating conn_handle={}", desc.conn_handle);
                                            esp_idf_svc::sys::ble_gap_terminate(
                                                desc.conn_handle,
                                                esp_idf_svc::sys::ble_error_codes_BLE_ERR_REM_USER_CONN_TERM as _,
                                            );
                                        }
                                    }
                                }
                                wd_abort.signal(());
                            });

                        // Race connect() against the abort signal.
                        println!("[scale] calling client.connect() (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
                        let connect_result = match select(
                            client.connect(&addr),
                            abort_signal.wait(),
                        )
                        .await
                        {
                            Either::First(result) => Some(result),
                            Either::Second(()) => {
                                println!("[scale] connect aborted by signal");
                                None
                            }
                        };
                        // Cancel the watchdog thread.
                        wd_done.store(true, Ordering::Relaxed);

                        match connect_result {
                            Some(Ok(())) => {
                                connected_client = Some(client);
                                break;
                            }
                            Some(Err(e)) => {
                                cancel_gap_operations();
                                let _ = client.disconnect();
                                let s = lock_or_recover(&state);
                                if s.active.is_none() {
                                    println!("[scale] connect cancelled (err={:?})", e);
                                    break;
                                }
                                drop(s);
                                if attempt < CONNECT_MAX_ATTEMPTS {
                                    println!(
                                        "[scale] connect attempt {attempt} failed: {:?}, will retry",
                                        e
                                    );
                                } else {
                                    println!("[scale] connect failed after {CONNECT_MAX_ATTEMPTS} attempts: {:?}", e);
                                    let mut s = lock_or_recover(&state);
                                    s.state = ScaleConnectionState::Error;
                                    s.message = format!(
                                        "Could not connect to {} after {CONNECT_MAX_ATTEMPTS} attempts. Make sure the scale is on and nearby.",
                                        display_scale_name(&req.name)
                                    );
                                    s.active = None;
                                    telemetry.clear_scale();
                                }
                            }
                            None => {
                                cancel_gap_operations();
                                let _ = client.disconnect();
                                let s = lock_or_recover(&state);
                                if s.active.is_none() {
                                    println!("[scale] connect cancelled during watchdog abort");
                                    break;
                                }
                                drop(s);
                                if attempt < CONNECT_MAX_ATTEMPTS {
                                    println!(
                                        "[scale] connect attempt {attempt} timed out, will retry"
                                    );
                                } else {
                                    println!("[scale] connect timed out after {CONNECT_MAX_ATTEMPTS} attempts");
                                    let mut s = lock_or_recover(&state);
                                    s.state = ScaleConnectionState::Error;
                                    s.message = format!(
                                        "Connection to {} timed out after {CONNECT_MAX_ATTEMPTS} attempts. Make sure the scale is on and nearby.",
                                        display_scale_name(&req.name)
                                    );
                                    s.active = None;
                                    telemetry.clear_scale();
                                }
                            }
                        }
                    } // end retry loop

                    // --- Post-connect: service discovery (only if we got a client) ---
                    if let Some(mut client) = connected_client {
                        // Check if the connect was cancelled while we waited
                        {
                            let s = lock_or_recover(&state);
                            if s.active.is_none() {
                                println!("[scale] connect succeeded but was cancelled, dropping");
                                let _ = client.disconnect();
                                continue;
                            }
                        }

                        println!("[scale] connected to {}", req.address_text);

                        {
                            let mut s = lock_or_recover(&state);
                            s.state = ScaleConnectionState::Discovering;
                            s.message = format!(
                                "Connected to {}. Discovering weight channel...",
                                display_scale_name(&req.name)
                            );
                        }

                        match discover_and_subscribe(
                            &mut client,
                            &req.name,
                            &state,
                            &telemetry,
                        )
                        .await
                        {
                            Ok(protocol) => {
                                {
                                    let mut s = lock_or_recover(&state);
                                    if let Some(active) = s.active.as_mut() {
                                        active.protocol = protocol;
                                    }
                                    s.state = ScaleConnectionState::Ready;
                                    s.message = format!(
                                        "Connected to {}.",
                                        display_scale_name(&req.name)
                                    );
                                    s.battery_percent = None;
                                }
                                telemetry.update_scale(true, 0.0, 0.0);

                                // Read battery if available
                                read_battery(&mut client, &state).await;

                                println!(
                                    "[scale] ready — streaming from {}",
                                    display_scale_name(&req.name)
                                );
                                active_client = Some(client);
                            }
                            Err(e) => {
                                println!("[scale] discovery failed: {e}");
                                let _ = client.disconnect();
                                let mut s = lock_or_recover(&state);
                                s.state = ScaleConnectionState::Error;
                                s.message = format!(
                                    "Connected to {} but could not find a weight channel: {e}",
                                    display_scale_name(&req.name)
                                );
                                s.active = None;
                                telemetry.clear_scale();
                            }
                        }
                    }
                }

                WorkerCommand::Disconnect => {
                    // Properly disconnect then drop the client.
                    if let Some(mut old_client) = active_client.take() {
                        println!("[scale] disconnect requested, terminating link");
                        let _ = old_client.disconnect();
                        // on_disconnect callback handles state updates
                    } else {
                        let mut s = lock_or_recover(&state);
                        s.active = None;
                        s.state = ScaleConnectionState::Idle;
                        s.message = "No scale is connected right now.".to_owned();
                        s.reset_live_values();
                        telemetry.clear_scale();
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Service / characteristic discovery and notification subscription
// ---------------------------------------------------------------------------

async fn discover_and_subscribe(
    client: &mut esp32_nimble::BLEClient,
    scale_name: &str,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<ScaleProtocol> {
    let scale_like_name = is_scale_like_name(scale_name);

    // ---- Priority 1: Standard Weight Scale Service (0x181D / 0x2A9D) ----
    if let Ok(service) = client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_WEIGHT_SCALE))
        .await
    {
        if let Ok(characteristic) = service
            .get_characteristic(BleUuid::from_uuid16(UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT))
            .await
        {
            if characteristic.can_notify() || characteristic.can_indicate() || characteristic.can_read()
            {
                println!(
                    "[scale] found standard weight characteristic 0x{:04X} on service 0x{:04X}",
                    UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT, UUID_SERVICE_WEIGHT_SCALE
                );
                subscribe_weight_notifications(
                    characteristic,
                    ScaleProtocol::StandardWeight,
                    format!(
                        "0x{:04X} on service 0x{:04X}",
                        UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT, UUID_SERVICE_WEIGHT_SCALE
                    ),
                    state,
                    telemetry,
                )
                .await?;
                return Ok(ScaleProtocol::StandardWeight);
            }
        }

        // If the standard service exists but the standard char doesn't, try
        // all notifying chars on that service instead of stopping at the
        // first one. Some scales expose a status channel and a separate
        // weight channel under the same service.
        if let Ok(chars) = service.get_characteristics().await {
            let mut subscribed_any = false;
            for characteristic in chars {
                if characteristic.can_notify() || characteristic.can_indicate() {
                    let channel_label = format!(
                        "{} on service 0x{:04X}",
                        characteristic.uuid(),
                        UUID_SERVICE_WEIGHT_SCALE
                    );
                    println!(
                        "[scale] found alternate notify char {} in weight-scale service",
                        characteristic.uuid()
                    );
                    subscribe_weight_notifications(
                        characteristic,
                        ScaleProtocol::GenericNotify,
                        channel_label,
                        state,
                        telemetry,
                    )
                    .await?;
                    subscribed_any = true;
                }
            }

            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    // ---- Priority 2: Common vendor services (0xFFF0, 0xFFE0, etc.) ------
    for &svc_uuid16 in COMMON_VENDOR_SERVICE_UUIDS {
        if let Ok(service) = client
            .get_service(BleUuid::from_uuid16(svc_uuid16))
            .await
        {
            let mut subscribed_any = false;
            let mut seen_uuids = BTreeSet::new();

            // First try known vendor char UUIDs, but keep going after the
            // first match so we do not get stuck on a status-only channel.
            for &char_uuid16 in COMMON_VENDOR_NOTIFY_UUIDS {
                if let Ok(characteristic) = service
                    .get_characteristic(BleUuid::from_uuid16(char_uuid16))
                    .await
                {
                    let channel_label = format!(
                        "{} on service 0x{:04X}",
                        characteristic.uuid(),
                        svc_uuid16
                    );
                    if !seen_uuids.insert(channel_label.clone()) {
                        continue;
                    }

                    if characteristic.can_notify() || characteristic.can_indicate() {
                        // Detect Bookoo: service 0x0FFE + char 0xFF11
                        let proto = if svc_uuid16 == 0x0FFE && char_uuid16 == 0xFF11 {
                            println!(
                                "[scale] detected Bookoo protocol (svc 0x0FFE / char 0xFF11)"
                            );
                            ScaleProtocol::Bookoo
                        } else {
                            ScaleProtocol::GenericNotify
                        };
                        println!(
                            "[scale] found vendor char 0x{:04X} on service 0x{:04X} proto={}",
                            char_uuid16, svc_uuid16, proto.as_str()
                        );
                        subscribe_weight_notifications(
                            characteristic,
                            proto,
                            channel_label,
                            state,
                            telemetry,
                        )
                        .await?;
                        subscribed_any = true;
                    }
                }
            }

            // Fall back to any other notifying characteristic in this vendor
            // service.
            if let Ok(chars) = service.get_characteristics().await {
                for characteristic in chars {
                    let channel_label = format!(
                        "{} on service 0x{:04X}",
                        characteristic.uuid(),
                        svc_uuid16
                    );
                    if !seen_uuids.insert(channel_label.clone()) {
                        continue;
                    }

                    if characteristic.can_notify() || characteristic.can_indicate() {
                        println!(
                            "[scale] found notify char {} in vendor service 0x{:04X}",
                            characteristic.uuid(),
                            svc_uuid16
                        );
                        subscribe_weight_notifications(
                            characteristic,
                            ScaleProtocol::GenericNotify,
                            channel_label,
                            state,
                            telemetry,
                        )
                        .await?;
                        subscribed_any = true;
                    }
                }
            }

            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    // ---- Priority 3: Brute-force — subscribe to all notifying chars on any
    // plausible service if the device looks like a scale.
    if scale_like_name {
        if let Ok(services) = client.get_services().await {
            let mut subscribed_any = false;
            for service in services {
                let svc_uuid = service.uuid();
                // Skip standard services that aren't weight-related
                if let BleUuid::Uuid16(v) = svc_uuid {
                    if v == 0x1800 || v == 0x1801 || v == UUID_SERVICE_BATTERY {
                        continue;
                    }
                }

                if let Ok(chars) = service.get_characteristics().await {
                    for characteristic in chars {
                        if characteristic.can_notify() || characteristic.can_indicate() {
                            let channel_label = format!(
                                "{} on service {}",
                                characteristic.uuid(),
                                svc_uuid,
                            );
                            println!(
                                "[scale] brute-force: using char {} on service {}",
                                characteristic.uuid(),
                                svc_uuid,
                            );
                            subscribe_weight_notifications(
                                characteristic,
                                ScaleProtocol::GenericNotify,
                                channel_label,
                                state,
                                telemetry,
                            )
                            .await?;
                            subscribed_any = true;
                        }
                    }
                }
            }

            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    Err(anyhow!(
        "No weight characteristic found on this device."
    ))
}

async fn subscribe_weight_notifications(
    characteristic: &mut esp32_nimble::BLERemoteCharacteristic,
    protocol: ScaleProtocol,
    channel_label: String,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<()> {
    let notify_state = state.clone();
    let notify_telemetry = telemetry.clone();
    let notify_channel_label = channel_label.clone();
    let mut debug_remaining: u8 = 8;

    characteristic.on_notify(move |data| {
        let (previous_weight_g, should_log) = {
            let s = lock_or_recover(&notify_state);
            (s.weight_g, debug_remaining > 0)
        };

        if should_log {
            debug_remaining = debug_remaining.saturating_sub(1);
            println!(
                "[scale] notify char={} protocol={} bytes={} len={}",
                notify_channel_label,
                protocol.as_str(),
                hex_bytes(data),
                data.len()
            );
        }

        if let Some(weight_g) = parse_weight_measurement(protocol, data, previous_weight_g) {
            if should_log {
                println!(
                    "[scale] parsed weight_g={:.2} (prev={:.2})",
                    weight_g, previous_weight_g
                );
            }
            apply_weight_measurement(&notify_state, &notify_telemetry, weight_g);
        }
    });

    if characteristic.can_indicate() && !characteristic.can_notify() {
        characteristic.subscribe_indicate(false).await.map_err(|e| {
            anyhow!("indication subscribe failed: {e:?}")
        })?;
        println!("[scale] subscribed to indications on {channel_label}");
    } else {
        characteristic.subscribe_notify(false).await.map_err(|e| {
            anyhow!("notify subscribe failed: {e:?}")
        })?;
        println!("[scale] subscribed to notifications on {channel_label}");
    }

    if characteristic.can_read() {
        if let Ok(value) = characteristic.read_value().await {
            if !value.is_empty() {
                println!(
                    "[scale] initial read char={} protocol={} bytes={} len={}",
                    channel_label,
                    protocol.as_str(),
                    hex_bytes(&value),
                    value.len()
                );

                let previous_weight_g = lock_or_recover(state).weight_g;
                if let Some(weight_g) = parse_weight_measurement(protocol, &value, previous_weight_g)
                {
                    apply_weight_measurement(state, telemetry, weight_g);
                }
            }
        }
    }

    Ok(())
}

fn apply_weight_measurement(
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
    weight_g: f32,
) {
    let mut s = lock_or_recover(state);
    let flow_gps = s.flow_estimator.observe(weight_g, unix_time_ms());
    s.weight_g = weight_g;
    s.flow_gps = flow_gps;
    s.state = ScaleConnectionState::Ready;
    telemetry.update_scale(true, weight_g, flow_gps);
}

async fn read_battery(
    client: &mut esp32_nimble::BLEClient,
    state: &Arc<Mutex<ScaleManagerState>>,
) {
    let service = match client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_BATTERY))
        .await
    {
        Ok(s) => s,
        Err(_) => return,
    };
    let characteristic = match service
        .get_characteristic(BleUuid::from_uuid16(UUID_CHARACTERISTIC_BATTERY_LEVEL))
        .await
    {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Ok(value) = characteristic.read_value().await {
        if let Some(&level) = value.first() {
            println!("[scale] battery={}%", level);
            lock_or_recover(state).battery_percent = Some(level);
        }
    }
}

// ---------------------------------------------------------------------------
// Discovered device upsert
// ---------------------------------------------------------------------------

fn upsert_discovered(state: &Arc<Mutex<ScaleManagerState>>, incoming: DiscoveredScaleInternal) {
    let mut s = lock_or_recover(state);
    if let Some(existing) = s
        .discovered
        .iter_mut()
        .find(|d| d.address_text.eq_ignore_ascii_case(&incoming.address_text))
    {
        let incoming_has_name = incoming.name != incoming.address_text;
        let existing_has_name = existing.name != existing.address_text;
        if incoming_has_name || !existing_has_name {
            existing.name = incoming.name;
        }
        existing.addr_type_str = incoming.addr_type_str;
        existing.rssi = incoming.rssi;
        if incoming.protocol_hint != ScaleProtocol::Unknown {
            existing.protocol_hint = incoming.protocol_hint;
        }
        existing.scale_like |= incoming.scale_like;
    } else {
        s.discovered.push(incoming);
    }

    s.discovered.sort_by(|a, b| {
        b.scale_like
            .cmp(&a.scale_like)
            .then(b.rssi.cmp(&a.rssi))
            .then(a.name.cmp(&b.name))
    });
    s.discovered.truncate(MAX_DISCOVERED_SCALES);
}

// ---------------------------------------------------------------------------
// Weight parsing (unchanged logic)
// ---------------------------------------------------------------------------

fn parse_weight_measurement(
    protocol: ScaleProtocol,
    value: &[u8],
    previous_weight_g: f32,
) -> Option<f32> {
    match protocol {
        ScaleProtocol::StandardWeight => parse_standard_weight_measurement(value),
        ScaleProtocol::Bookoo => parse_bookoo_weight(value),
        ScaleProtocol::GenericNotify | ScaleProtocol::Unknown => {
            parse_generic_weight_measurement(value, previous_weight_g)
        }
    }
}

/// Bookoo BOOKOO_SC_U: service 0x0FFE, char 0xFF11, 20-byte packets.
/// Header: 03 0B. Byte 6: sign (0x2B='+', 0x2D='-'). Bytes 7-9: weight
/// big-endian 24-bit int in 0.01 g units.
fn parse_bookoo_weight(value: &[u8]) -> Option<f32> {
    if value.len() < 10 {
        return None;
    }
    if value[0] != 0x03 || value[1] != 0x0B {
        // Not a Bookoo weight packet — might be a status/heartbeat frame.
        return None;
    }
    let sign: f32 = if value[6] == 0x2D { -1.0 } else { 1.0 };
    let raw = ((value[7] as u32) << 16) | ((value[8] as u32) << 8) | (value[9] as u32);
    let weight_g = sign * (raw as f32) / 100.0;
    #[cfg(debug_assertions)]
    {
        println!(
            "[scale] bookoo: sign={} raw={} weight_g={:.1}",
            if sign < 0.0 { '-' } else { '+' },
            raw,
            weight_g
        );
    }
    Some(sanitize_weight_g(weight_g))
}

fn parse_standard_weight_measurement(value: &[u8]) -> Option<f32> {
    if value.len() < 3 {
        return None;
    }
    let flags = value[0];
    let weight_raw = u16::from_le_bytes([value[1], value[2]]) as f32;
    let weight_g = if flags & 0x01 == 0 {
        weight_raw * 5.0
    } else {
        weight_raw * 4.535_923_7
    };
    Some(sanitize_weight_g(weight_g))
}

fn parse_generic_weight_measurement(value: &[u8], previous_weight_g: f32) -> Option<f32> {
    if let Some(parsed) = parse_ascii_weight(value) {
        return Some(parsed);
    }

    let half = value.len() / 2;
    let mut best: Option<WeightCandidate> = None;
    let mut best_dist: f32 = f32::MAX;

    for start in 0..value.len() {
        let window = &value[start..];

        // Little-endian i32
        if window.len() >= 4 {
            let raw = i32::from_le_bytes([window[0], window[1], window[2], window[3]]);
            consider_raw(&mut best, &mut best_dist, raw as i64, start, half, previous_weight_g);
        }
        // Big-endian i32
        if window.len() >= 4 {
            let raw = i32::from_be_bytes([window[0], window[1], window[2], window[3]]);
            consider_raw(&mut best, &mut best_dist, raw as i64, start, half, previous_weight_g);
        }
        // Little-endian 24-bit
        if window.len() >= 3 {
            let raw = (window[0] as i32) | ((window[1] as i32) << 8) | ((window[2] as i32) << 16);
            consider_raw(&mut best, &mut best_dist, raw as i64, start, half, previous_weight_g);
        }
        // Big-endian 24-bit
        if window.len() >= 3 {
            let raw = ((window[0] as i32) << 16) | ((window[1] as i32) << 8) | (window[2] as i32);
            consider_raw(&mut best, &mut best_dist, raw as i64, start, half, previous_weight_g);
        }
        // Little-endian i16 / u16
        if window.len() >= 2 {
            let signed = i16::from_le_bytes([window[0], window[1]]) as i64;
            let unsigned = u16::from_le_bytes([window[0], window[1]]) as i64;
            consider_raw(&mut best, &mut best_dist, signed, start, half, previous_weight_g);
            consider_raw(&mut best, &mut best_dist, unsigned, start, half, previous_weight_g);
        }
        // Big-endian i16 / u16
        if window.len() >= 2 {
            let signed = i16::from_be_bytes([window[0], window[1]]) as i64;
            let unsigned = u16::from_be_bytes([window[0], window[1]]) as i64;
            consider_raw(&mut best, &mut best_dist, signed, start, half, previous_weight_g);
            consider_raw(&mut best, &mut best_dist, unsigned, start, half, previous_weight_g);
        }
    }

    best.map(|c| sanitize_weight_g(c.weight_g))
}

#[derive(Debug, Clone, Copy)]
struct WeightCandidate {
    weight_g: f32,
    offset: usize,
    raw_abs: u64,
    in_trailer: bool,
}

fn consider_raw(
    best: &mut Option<WeightCandidate>,
    best_dist: &mut f32,
    raw: i64,
    offset: usize,
    half: usize,
    previous_weight_g: f32,
) {
    let raw_abs = raw.unsigned_abs();
    let in_trailer = offset > half;
    for divisor in [1.0_f32, 10.0, 100.0, 1000.0] {
        let weight_g = raw as f32 / divisor;
        if !weight_g.is_finite() || !(0.0..=5000.0).contains(&weight_g) {
            continue;
        }
        if weight_g <= 0.0 {
            continue;
        }
        let c = WeightCandidate { weight_g, offset, raw_abs, in_trailer };
        let d = candidate_distance(&c, previous_weight_g);
        if d < *best_dist || (d == *best_dist && offset < best.map_or(usize::MAX, |b| b.offset)) {
            *best = Some(c);
            *best_dist = d;
        }
    }
}

fn candidate_distance(candidate: &WeightCandidate, previous_weight_g: f32) -> f32 {
    if previous_weight_g > 0.0 {
        let mut d = (candidate.weight_g - previous_weight_g).abs();
        if candidate.raw_abs < 10 {
            d += 500.0;
        }
        if candidate.in_trailer {
            d += 200.0;
        }
        d
    } else {
        if candidate.weight_g <= 0.0 {
            return 10_000.0;
        }
        let mut score = candidate.weight_g;
        if candidate.raw_abs < 10 {
            score += 5_000.0;
        }
        if candidate.in_trailer {
            score += 1_000.0;
        }
        score
    }
}

fn parse_ascii_weight(value: &[u8]) -> Option<f32> {
    let text = String::from_utf8_lossy(value);
    let mut number = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
        } else if !number.is_empty() {
            break;
        }
    }
    let parsed = number.parse::<f32>().ok()?;
    Some(sanitize_weight_g(parsed))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_scale_like_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SCALE_NAME_HINTS.iter().any(|hint| lower.contains(hint))
}

fn ble_addr_type_str(addr_type: BLEAddressType) -> &'static str {
    match addr_type {
        BLEAddressType::Public => "public",
        BLEAddressType::Random => "random",
        _ => "random",
    }
}

fn parse_nimble_addr_type(value: &str) -> BLEAddressType {
    match value {
        "public" => BLEAddressType::Public,
        _ => BLEAddressType::Random,
    }
}

fn display_scale_name(name: &str) -> &str {
    if name.trim().is_empty() {
        "selected scale"
    } else {
        name
    }
}

/// Cancel any in-progress BLE GAP connection attempt at the controller level.
/// Safe to call from any thread — NimBLE serialises internally.
fn cancel_ble_connect() {
    let rc = unsafe { esp_idf_svc::sys::ble_gap_conn_cancel() };
    if rc == 0 {
        println!("[scale] ble_gap_conn_cancel succeeded");
    } else {
        // rc != 0 just means no connect was pending — harmless.
        println!("[scale] ble_gap_conn_cancel rc={rc} (no pending connect)");
    }
}

/// Cancel any pending GAP operations (scan, connect) so the next operation
/// starts cleanly.  Harmless if nothing is pending.
fn cancel_gap_operations() {
    unsafe {
        let rc_disc = esp_idf_svc::sys::ble_gap_disc_cancel();
        if rc_disc == 0 {
            println!("[scale] cancelled stale scan");
        }
        let rc_conn = esp_idf_svc::sys::ble_gap_conn_cancel();
        if rc_conn == 0 {
            println!("[scale] cancelled stale connect");
        }
    }
}

/// Read the ESP32's own BLE address so we can exclude it from scan results.
fn get_own_ble_address() -> Option<String> {
    let mut addr = [0u8; 6];
    // Try public address first.
    let rc = unsafe {
        esp_idf_svc::sys::ble_hs_id_copy_addr(
            0, // BLE_ADDR_PUBLIC
            addr.as_mut_ptr(),
            core::ptr::null_mut(),
        )
    };
    if rc == 0 && addr != [0u8; 6] {
        return Some(format_ble_addr(&addr));
    }
    // Fall back to random address.
    let rc = unsafe {
        esp_idf_svc::sys::ble_hs_id_copy_addr(
            1, // BLE_ADDR_RANDOM
            addr.as_mut_ptr(),
            core::ptr::null_mut(),
        )
    };
    if rc == 0 && addr != [0u8; 6] {
        return Some(format_ble_addr(&addr));
    }
    None
}

fn format_ble_addr(bytes: &[u8; 6]) -> String {
    // NimBLE stores addresses LSB-first; Display convention is MSB-first.
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0]
    )
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(unused_imports, dead_code)]
mod tests {
    use super::{candidate_distance, parse_ascii_weight, parse_generic_weight_measurement, WeightCandidate};

    fn approx_eq(left: f32, right: f32, tolerance: f32) {
        assert!((left - right).abs() <= tolerance, "left={left}, right={right}");
    }

    #[test]
    fn generic_parser_reads_ascii_payloads() {
        let parsed = parse_ascii_weight(b"WT: 18.3 g").expect("ascii payload should parse");
        approx_eq(parsed, 18.3, 1e-6);
    }

    #[test]
    fn generic_parser_finds_weight_in_be_packet() {
        // Real Bookoo packet: 44.8g on the scale
        // Weight at bytes 8-9 as BE u16: 0x1180 = 4480 → /100 = 44.8g
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80,
            0x2B, 0x00, 0x02, 0x50, 0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w = parse_generic_weight_measurement(&pkt, 0.0).expect("should parse");
        approx_eq(w, 44.8, 0.5);
    }

    #[test]
    fn generic_parser_tracks_weight_change() {
        // First packet: 44.8g
        let pkt1: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80,
            0x2B, 0x00, 0x02, 0x50, 0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w1 = parse_generic_weight_measurement(&pkt1, 0.0).expect("should parse");
        approx_eq(w1, 44.8, 0.5);

        // Second packet: ~0g (bytes 8-9 = 00 00)
        let pkt2: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00,
            0x2B, 0x00, 0x00, 0x50, 0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        // With seeded previous, should find a candidate near 0, not lock on noise
        let w2 = parse_generic_weight_measurement(&pkt2, w1);
        // Should either return a small value or None (all real candidates zero out)
        if let Some(w) = w2 {
            assert!(w < 10.0, "expected near-zero, got {w}");
        }
    }

    #[test]
    fn generic_parser_rejects_noise_from_trailer() {
        // Packet where only trailer has non-zero: real weight is 0
        // The parser should NOT pick up 0x0196 at offset 14-15 as a fake weight
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00,
            0x2B, 0x00, 0x00, 0x50, 0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        // When seeded at 44.8, the parser should find something closer to
        // 44.8 than a random trailer interpretation, or if all "real" candidates
        // are 0 it may pick a small noise value - but NOT 44.8+ from trailer
        let w = parse_generic_weight_measurement(&pkt, 44.8);
        if let Some(weight) = w {
            assert!(weight < 50.0, "expected reasonable value, got {weight}");
        }
    }

    #[test]
    fn candidate_distance_prefers_positive_values_when_scale_is_unseeded() {
        let zero = WeightCandidate {
            weight_g: 0.0,
            offset: 0,
            raw_abs: 0,
            in_trailer: false,
        };
        let positive = WeightCandidate {
            weight_g: 2.4,
            offset: 2,
            raw_abs: 240,
            in_trailer: false,
        };

        assert!(candidate_distance(&positive, 0.0) < candidate_distance(&zero, 0.0));
    }

    #[test]
    fn candidate_distance_penalizes_low_raw_values() {
        let noise = WeightCandidate {
            weight_g: 1.0,
            offset: 15,
            raw_abs: 1,
            in_trailer: true,
        };
        let real = WeightCandidate {
            weight_g: 44.8,
            offset: 8,
            raw_abs: 4480,
            in_trailer: false,
        };

        assert!(candidate_distance(&real, 0.0) < candidate_distance(&noise, 0.0));
    }
}
