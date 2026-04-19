//! Worker thread: single async context for all NimBLE operations.
//!
//! The worker owns the `BLEClient` (which is `!Send`) and processes commands
//! from the public API and the reconnect thread via an `mpsc` channel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use embassy_futures::select::{select, Either};
use esp32_nimble::{BLEAddress, BLEDevice, BLEScan};
use log::{debug, error, info, warn};

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use super::discovery;
use super::nimble;
use super::types::*;
use super::watchdog;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SCALE_SCAN_DURATION_S: u32 = 6;
const CONNECT_TIMEOUT_MS: u32 = 2_000;
const CONNECT_MAX_ATTEMPTS: u32 = 10;

pub(crate) const SCALE_READY_MESSAGE: &str = "Bluetooth scale idle.";

// ---------------------------------------------------------------------------
// Worker commands
// ---------------------------------------------------------------------------

pub(crate) struct ConnectRequest {
    pub connection_id: u64,
    pub address_text: String,
    pub addr_type_str: String,
    pub name: String,
}

pub(crate) enum WorkerCommand {
    StartScan,
    ConnectTarget(ConnectRequest),
    Disconnect,
}

// ---------------------------------------------------------------------------
// Worker entry point
// ---------------------------------------------------------------------------

pub(crate) fn worker_loop(
    rx: std::sync::mpsc::Receiver<WorkerCommand>,
    state: Arc<Mutex<ScaleManagerState>>,
    telemetry: SharedTelemetry,
) {
    esp_idf_hal::task::block_on(async {
        let ble_device = BLEDevice::take();

        {
            let mut s = lock_or_recover(&state);
            s.transport_ready = true;
            s.state = ScaleConnectionState::Idle;
            s.message = SCALE_READY_MESSAGE.to_owned();
        }

        let own_addr = nimble::own_ble_address();
        if let Some(ref a) = own_addr {
            info!("own BLE address: {a}");
        }
        info!("NimBLE transport ready");

        let mut active_client: Option<esp32_nimble::BLEClient> = None;

        while let Ok(command) = rx.recv() {
            match command {
                WorkerCommand::StartScan => {
                    handle_scan(ble_device, &own_addr, &mut active_client, &state).await;
                }
                WorkerCommand::ConnectTarget(req) => {
                    handle_connect(
                        ble_device,
                        req,
                        &mut active_client,
                        &state,
                        &telemetry,
                    )
                    .await;
                }
                WorkerCommand::Disconnect => {
                    handle_disconnect(&mut active_client, &state, &telemetry);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

async fn handle_scan(
    ble_device: &BLEDevice,
    own_addr: &Option<String>,
    active_client: &mut Option<esp32_nimble::BLEClient>,
    state: &Arc<Mutex<ScaleManagerState>>,
) {
    if let Some(mut old) = active_client.take() {
        let _ = old.disconnect();
    }
    nimble::cancel_all_gap_operations();

    {
        let mut s = lock_or_recover(state);
        s.discovered.clear();
        s.state = ScaleConnectionState::Scanning;
        s.message = "Scanning for nearby Bluetooth scales...".to_owned();
    }

    info!("starting BLE scan ({SCALE_SCAN_DURATION_S}s)");

    let scan_state = state.clone();
    let scan_own_addr = own_addr.clone();
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
                    addr.to_string()
                } else {
                    name
                };
                let rssi = device.rssi() as i32;
                let addr_text = addr.to_string();
                let scale_like = is_scale_like_name(&name);

                // Skip our own BLE address.
                if scan_own_addr
                    .as_deref()
                    .is_some_and(|own| own.eq_ignore_ascii_case(&addr_text))
                {
                    return None::<()>;
                }

                // Ignore weak unnamed noise.
                if !scale_like && rssi < -88 && name == addr_text {
                    return None::<()>;
                }

                let internal = DiscoveredScaleInternal {
                    address_text: addr_text,
                    addr_type_str: nimble::ble_addr_type_str(addr.addr_type()).to_owned(),
                    name,
                    rssi,
                    protocol_hint: ScaleProtocol::Unknown,
                    scale_like,
                };
                upsert_discovered(&scan_state, internal);
                None::<()>
            },
        )
        .await;

    if let Err(e) = &result {
        warn!("scan returned error: {e:?}");
    }

    let mut s = lock_or_recover(state);
    if s.state == ScaleConnectionState::Scanning {
        s.state = ScaleConnectionState::Idle;
        s.message = if s.discovered.is_empty() {
            "No nearby scales found. Try moving the scale closer and scan again.".to_owned()
        } else {
            format!("Found {} device(s). Tap one to connect.", s.discovered.len())
        };
    }
    info!("scan complete ({} devices)", s.discovered.len());
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

async fn handle_connect(
    ble_device: &BLEDevice,
    req: ConnectRequest,
    active_client: &mut Option<esp32_nimble::BLEClient>,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) {
    // Check for stale request.
    {
        let s = lock_or_recover(state);
        if !s.is_active_connection(req.connection_id) {
            debug!("ignoring stale connect request for {}", display_scale_name(&req.name));
            return;
        }
    }

    // Drop any existing client.
    if let Some(mut old) = active_client.take() {
        debug!("dropping previous client");
        let _ = old.disconnect();
    }
    nimble::cancel_all_gap_operations();

    let addr_type = nimble::parse_addr_type(&req.addr_type_str);
    let Some(addr) = BLEAddress::from_str(&req.address_text, addr_type) else {
        let mut s = lock_or_recover(state);
        s.state = ScaleConnectionState::Error;
        s.message = format!("Invalid BLE address: {}", req.address_text);
        s.active = None;
        return;
    };

    info!(
        "connecting to {} at {} ({})",
        display_scale_name(&req.name),
        req.address_text,
        req.addr_type_str,
    );

    // --- Pre-connect scan: verify the device is actually advertising before
    //     committing to the retry loop.  Without this, a powered-off scale
    //     triggers ~20 connect→cancel cycles (~90 s) where each
    //     ble_gap_conn_cancel() dirties NimBLE's internal state, making
    //     subsequent attempts *less* likely to succeed.  A quick passive scan
    //     avoids all of that: if the device isn't advertising we bail
    //     instantly and let the auto-reconnect thread try again later.  When
    //     the device IS found the controller's radio is warmed up and
    //     connection succeeds much faster. ---
    {
        let prescan_found = Arc::new(AtomicBool::new(false));
        let prescan_found2 = prescan_found.clone();
        let prescan_target = req.address_text.clone();

        {
            let mut s = lock_or_recover(state);
            if s.is_active_connection(req.connection_id) {
                s.message = format!("Searching for {}...", display_scale_name(&req.name));
            }
        }

        let mut prescan = BLEScan::new();
        prescan
            .active_scan(false)
            .filter_duplicates(true)
            .interval(100)
            .window(99);

        info!("pre-connect scan for {}", req.address_text);
        let _ = prescan
            .start(ble_device, 3000, move |device, _data| {
                let ad = format!("{}", device.addr());
                if ad.eq_ignore_ascii_case(&prescan_target) {
                    prescan_found2.store(true, Ordering::Release);
                    return Some(()); // stop scan early
                }
                None::<()>
            })
            .await;

        nimble::cancel_all_gap_operations();

        if !prescan_found.load(Ordering::Acquire) {
            info!(
                "pre-connect scan: {} not advertising, skipping connect",
                req.address_text
            );
            let mut s = lock_or_recover(state);
            if s.is_active_connection(req.connection_id) {
                s.state = ScaleConnectionState::Idle;
                s.message = format!(
                    "{} not found nearby. Will retry automatically.",
                    display_scale_name(&req.name)
                );
                s.active = None;
            }
            return;
        }
        info!("pre-connect scan: found {}", req.address_text);

        {
            let mut s = lock_or_recover(state);
            if s.is_active_connection(req.connection_id) {
                s.message = format!("Connecting to {}...", display_scale_name(&req.name));
            }
        }
    }

    // Single client reused across retry attempts.
    let mut client = ble_device.new_client();
    let mut connected = false;

    // Persistent watchdog (channel-based, no spin-polling).
    let wd = watchdog::spawn(CONNECT_TIMEOUT_MS);

    for attempt in 1..=CONNECT_MAX_ATTEMPTS {
        // Check for user cancellation between retries.
        {
            let s = lock_or_recover(state);
            if !s.is_active_connection(req.connection_id) {
                info!("connect cancelled by user before attempt {attempt}");
                break;
            }
        }

        if attempt > 1 {
            info!("retrying connect (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
            nimble::cancel_all_gap_operations();
            // Real delay to let the NimBLE controller fully process the
            // previous cancellation.  yield_now() is a no-op under
            // block_on's single-task executor.
            thread::sleep(Duration::from_millis(500));
            {
                let mut s = lock_or_recover(state);
                s.message = format!("Retrying connection to {}...", display_scale_name(&req.name));
            }
        }

        // Arm watchdog for this attempt.
        wd.arm();

        info!("calling client.connect() (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
        let connect_result = match select(client.connect(&addr), wd.abort_signal.wait()).await {
            Either::First(result) => Some(result),
            Either::Second(()) => {
                info!("connect aborted by watchdog signal");
                None
            }
        };
        wd.done();

        match connect_result {
            Some(Ok(())) => {
                connected = true;
                break;
            }
            Some(Err(e)) => {
                nimble::cancel_all_gap_operations();
                let _ = client.disconnect();
                client = ble_device.new_client();

                let s = lock_or_recover(state);
                if !s.is_active_connection(req.connection_id) {
                    info!("connect cancelled (err={e:?})");
                    break;
                }
                drop(s);

                if attempt < CONNECT_MAX_ATTEMPTS {
                    warn!("connect attempt {attempt} failed: {e:?}, will retry");
                } else {
                    error!("connect failed after {CONNECT_MAX_ATTEMPTS} attempts: {e:?}");
                    set_connect_failed(state, telemetry, &req);
                }
            }
            None => {
                nimble::cancel_all_gap_operations();
                let _ = client.disconnect();
                client = ble_device.new_client();

                let s = lock_or_recover(state);
                if !s.is_active_connection(req.connection_id) {
                    info!("connect cancelled during watchdog abort");
                    break;
                }
                drop(s);

                if attempt < CONNECT_MAX_ATTEMPTS {
                    warn!("connect attempt {attempt} timed out, will retry");
                } else {
                    error!("connect timed out after {CONNECT_MAX_ATTEMPTS} attempts");
                    set_connect_failed(state, telemetry, &req);
                }
            }
        }
    }

    // Watchdog is dropped here (via `wd`), which sends Quit and joins.
    drop(wd);

    if !connected {
        // Ensure the client's NimBLE resources are released even though no
        // connection was established.  drop() alone only clears the GAP
        // callback and can leak GATTC / connection slots after repeated
        // cycles.
        let _ = client.disconnect();
        nimble::cancel_all_gap_operations();
        // Let NimBLE fully settle before the next cycle.
        thread::sleep(Duration::from_millis(200));
        return;
    }

    // --- Post-connect: verify the connection wasn't cancelled while we waited ---
    {
        let s = lock_or_recover(state);
        if !s.is_active_connection(req.connection_id) {
            info!("connect succeeded but was cancelled, dropping");
            let _ = client.disconnect();
            return;
        }
    }

    // Register disconnect callback.
    let disc_state = state.clone();
    let disc_telemetry = telemetry.clone();
    let disc_name = req.name.clone();
    let disc_id = req.connection_id;
    client.on_disconnect(move |reason| {
        let mut s = lock_or_recover(&disc_state);
        if !s.is_active_connection(disc_id) {
            debug!(
                "ignoring stale disconnect from {} (reason={reason})",
                display_scale_name(&disc_name),
            );
            return;
        }
        info!(
            "disconnected from {} (reason={reason})",
            display_scale_name(&disc_name),
        );
        s.active = None;
        s.state = ScaleConnectionState::Idle;
        s.message = format!("Disconnected from {}.", display_scale_name(&disc_name));
        s.reset_live_values();
        disc_telemetry.clear_scale();
    });

    info!("connected to {}", req.address_text);

    {
        let mut s = lock_or_recover(state);
        s.state = ScaleConnectionState::Discovering;
        s.message = format!(
            "Connected to {}. Discovering weight channel...",
            display_scale_name(&req.name),
        );
    }

    match discovery::discover_and_subscribe(
        &mut client,
        req.connection_id,
        &req.name,
        state,
        telemetry,
    )
    .await
    {
        Ok(protocol) => {
            {
                let mut s = lock_or_recover(state);
                if !s.is_active_connection(req.connection_id) {
                    info!("discovery completed for stale connection, dropping");
                    drop(s);
                    let _ = client.disconnect();
                    return;
                }
                if let Some(active) = s.active.as_mut() {
                    active.protocol = protocol;
                }
                s.state = ScaleConnectionState::Ready;
                s.message = format!("Connected to {}.", display_scale_name(&req.name));
                s.battery_percent = None;
            }
            telemetry.update_scale(true, 0.0, 0.0);

            discovery::read_battery(&mut client, req.connection_id, state).await;

            info!("ready — streaming from {}", display_scale_name(&req.name));
            *active_client = Some(client);
        }
        Err(e) => {
            error!("discovery failed: {e}");
            let _ = client.disconnect();
            let mut s = lock_or_recover(state);
            if s.is_active_connection(req.connection_id) {
                s.state = ScaleConnectionState::Error;
                s.message = format!(
                    "Connected to {} but could not find a weight channel: {e}",
                    display_scale_name(&req.name),
                );
                s.active = None;
                telemetry.clear_scale();
            }
        }
    }
}

fn set_connect_failed(
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
    req: &ConnectRequest,
) {
    let mut s = lock_or_recover(state);
    if s.is_active_connection(req.connection_id) {
        s.state = ScaleConnectionState::Idle;
        s.message = format!(
            "Could not reach {}. It may be off or out of range.",
            display_scale_name(&req.name),
        );
        s.active = None;
        telemetry.clear_scale();
    }
}

// ---------------------------------------------------------------------------
// Disconnect
// ---------------------------------------------------------------------------

fn handle_disconnect(
    active_client: &mut Option<esp32_nimble::BLEClient>,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) {
    if let Some(mut old) = active_client.take() {
        info!("disconnect requested, terminating link");
        let _ = old.disconnect();
        // on_disconnect callback handles state updates.
    } else {
        let mut s = lock_or_recover(state);
        s.active = None;
        s.state = ScaleConnectionState::Idle;
        s.message = "No scale is connected right now.".to_owned();
        s.reset_live_values();
        telemetry.clear_scale();
    }
}
