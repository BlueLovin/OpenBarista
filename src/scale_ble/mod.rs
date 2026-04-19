//! Bluetooth Low Energy scale integration.
//!
//! This module manages the full lifecycle of a BLE scale connection: scanning,
//! connecting, service discovery, weight streaming, auto-reconnect, and
//! disconnect. It exposes a thread-safe [`ScaleRuntime`] that the rest of the
//! firmware interacts with.
//!
//! # Module layout
//!
//! | Module       | Responsibility |
//! |--------------|----------------|
//! | `types`      | Shared enums, DTOs, internal state |
//! | `weight`     | Pure weight-parsing logic (no BLE deps) |
//! | `nimble`     | Safe wrappers around NimBLE FFI |
//! | `watchdog`   | Channel-based connect-timeout watchdog |
//! | `discovery`  | GATT service/characteristic discovery |
//! | `worker`     | Async worker thread + scan/connect/disconnect handlers |

mod discovery;
mod nimble;
pub(crate) mod types;
mod watchdog;
mod weight;
mod worker;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::info;

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

pub use types::{SavedScale, ScaleStatusSnapshot};

use types::*;
use worker::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SCALE_STARTUP_MESSAGE: &str = "Starting Bluetooth scale transport...";
const RECONNECT_POLL_INTERVAL: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// ScaleRuntime — public entry point
// ---------------------------------------------------------------------------

pub struct ScaleRuntime {
    state: Arc<Mutex<ScaleManagerState>>,
    worker_tx: Option<Sender<WorkerCommand>>,
    telemetry: SharedTelemetry,
    /// Condvar poked when a saved scale is set or a manual connect/scan
    /// happens, so the reconnect thread can wake immediately.
    reconnect_notify: Arc<Condvar>,
    _worker_thread: Option<thread::JoinHandle<()>>,
    _reconnect_thread: Option<thread::JoinHandle<()>>,
}

impl ScaleRuntime {
    pub fn disabled(message: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScaleManagerState::new(false, message.into()))),
            worker_tx: None,
            telemetry: SharedTelemetry::new(),
            reconnect_notify: Arc::new(Condvar::new()),
            _worker_thread: None,
            _reconnect_thread: None,
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
                worker::worker_loop(worker_rx, worker_state, worker_telemetry);
            })?;

        let reconnect_notify = Arc::new(Condvar::new());
        let reconn_state = state.clone();
        let reconn_tx = worker_tx.clone();
        let reconn_cv = reconnect_notify.clone();
        let reconnect_thread = thread::Builder::new()
            .name("ble-reconn".into())
            .stack_size(4096)
            .spawn(move || {
                reconnect_loop(reconn_state, reconn_tx, reconn_cv);
            })?;

        Ok(Self {
            state,
            worker_tx: Some(worker_tx),
            telemetry,
            reconnect_notify,
            _worker_thread: Some(worker_thread),
            _reconnect_thread: Some(reconnect_thread),
        })
    }

    // ---- Read-only queries ------------------------------------------------

    pub fn snapshot(&self) -> ScaleStatusSnapshot {
        lock_or_recover(&self.state).snapshot()
    }

    // ---- Mutations --------------------------------------------------------

    pub fn apply_saved_scale(&self, saved_scale: Option<SavedScale>) {
        {
            let mut state = lock_or_recover(&self.state);
            state.saved_scale = saved_scale;
        }
        // Wake the reconnect thread so it sees the new saved scale immediately
        // instead of sleeping for up to 15 seconds.
        self.reconnect_notify.notify_all();
    }

    pub fn forget_saved_scale(&self) {
        lock_or_recover(&self.state).saved_scale = None;
    }

    pub fn start_scan(&self) -> Result<&'static str> {
        let tx = self
            .worker_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Bluetooth is unavailable on this build."))?;

        {
            let mut s = lock_or_recover(&self.state);
            if !s.transport_ready {
                s.message = "Bluetooth is still starting. Try again in a moment.".to_owned();
                return Ok("Bluetooth is still starting. Try again in a moment.");
            }
            if s.state == ScaleConnectionState::Ready {
                s.message = "Disconnect the current scale before scanning.".to_owned();
                return Ok("Disconnect the current scale before scanning.");
            }
            if s.state == ScaleConnectionState::Connecting
                || s.state == ScaleConnectionState::Discovering
            {
                info!("cancelling in-progress connect for new scan");
                s.active = None;
                s.reset_live_values();
                nimble::cancel_connect();
            }
            s.discovered.clear();
            s.state = ScaleConnectionState::Scanning;
            s.auto_reconnect_suppressed = false;
            s.message = "Scanning for nearby Bluetooth scales...".to_owned();
        }
        self.telemetry.clear_scale();
        self.reconnect_notify.notify_all();

        tx.send(WorkerCommand::StartScan)
            .map_err(|_| anyhow!("Bluetooth worker stopped unexpectedly."))?;
        Ok("Scanning for nearby Bluetooth scales...")
    }

    pub fn connect_address(&self, address: &str) -> Result<String> {
        let tx = self
            .worker_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Bluetooth is unavailable on this build."))?;

        // Single lock for both reading state and mutating — no lock-drop-lock race.
        let message = {
            let mut s = lock_or_recover(&self.state);
            s.auto_reconnect_suppressed = false;

            // Already connected to this address?
            if let Some(active) = s.active.as_ref() {
                if active.address_text.eq_ignore_ascii_case(address) {
                    return Ok(format!(
                        "Already connected to {}.",
                        display_scale_name(&active.name),
                    ));
                }
                return Err(anyhow!(
                    "Disconnect {} before connecting a different scale.",
                    display_scale_name(&active.name),
                ));
            }

            // Find the device in discovered list or saved scale.
            let (addr_text, addr_type_str, name) = if let Some(d) = s
                .discovered
                .iter()
                .find(|d| d.address_text.eq_ignore_ascii_case(address))
            {
                (
                    d.address_text.clone(),
                    d.addr_type_str.clone(),
                    d.name.clone(),
                )
            } else if let Some(saved) = s
                .saved_scale
                .as_ref()
                .filter(|sv| sv.address.eq_ignore_ascii_case(address))
            {
                (
                    saved.address.clone(),
                    saved.addr_type.clone(),
                    saved.name.clone(),
                )
            } else {
                return Err(anyhow!(
                    "Scale not found. Scan again and tap a device from the list.",
                ));
            };

            let connection_id = s.allocate_connection_id();
            s.state = ScaleConnectionState::Connecting;
            let msg = format!("Connecting to {}...", display_scale_name(&name));
            s.message = msg.clone();
            s.active = Some(ActiveScaleConnection {
                id: connection_id,
                address_text: addr_text.clone(),
                name: name.clone(),
                protocol: ScaleProtocol::Unknown,
            });
            s.reset_live_values();

            let request = ConnectRequest {
                connection_id,
                address_text: addr_text,
                addr_type_str,
                name,
            };
            self.telemetry.clear_scale();
            // Send while holding the lock so no other thread can sneak in
            // between state update and command dispatch.
            if let Err(_) = tx.send(WorkerCommand::ConnectTarget(request)) {
                s.state = ScaleConnectionState::Idle;
                s.message = "Failed to send connect command.".to_owned();
                s.active = None;
                return Err(anyhow!("Bluetooth worker stopped unexpectedly."));
            }
            msg
        };

        self.reconnect_notify.notify_all();
        Ok(message)
    }

    pub fn connect_saved_scale(&self) -> Result<String> {
        let address = {
            let s = lock_or_recover(&self.state);
            s.saved_scale
                .as_ref()
                .ok_or_else(|| anyhow!("No saved scale configured."))?
                .address
                .clone()
        };
        self.connect_address(&address)
    }

    pub fn disconnect(&self) -> Result<&'static str> {
        let tx = self
            .worker_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Bluetooth is unavailable on this build."))?;

        {
            let mut s = lock_or_recover(&self.state);
            if s.active.is_none()
                && s.state != ScaleConnectionState::Connecting
                && s.state != ScaleConnectionState::Discovering
            {
                s.message = "No scale is connected right now.".to_owned();
                return Ok("No scale is connected right now.");
            }
            let was_connecting = s.state == ScaleConnectionState::Connecting
                || s.state == ScaleConnectionState::Discovering;
            s.active = None;
            s.state = ScaleConnectionState::Idle;
            s.message = "Disconnected.".to_owned();
            s.auto_reconnect_suppressed = true;
            s.reset_live_values();

            if was_connecting {
                nimble::cancel_connect();
            }
        }
        self.telemetry.clear_scale();

        let _ = tx.send(WorkerCommand::Disconnect);
        Ok("Disconnected.")
    }
}

// ---------------------------------------------------------------------------
// Reconnect thread — Condvar-based instead of sleep-polling
// ---------------------------------------------------------------------------

fn reconnect_loop(
    state: Arc<Mutex<ScaleManagerState>>,
    tx: Sender<WorkerCommand>,
    cv: Arc<Condvar>,
) {
    // Wait for the BLE worker to finish initialising.
    thread::sleep(Duration::from_secs(2));

    // The Condvar needs a mutex to wait on. We reuse the state mutex so a
    // notify wakes us immediately when conditions change.
    loop {
        // Sleep until the interval elapses OR someone calls notify_all().
        {
            let guard = lock_or_recover(&state);
            let _ = cv.wait_timeout(guard, RECONNECT_POLL_INTERVAL);
        }

        let request = {
            let mut s = lock_or_recover(&state);

            let should_reconnect = matches!(
                s.state,
                ScaleConnectionState::Idle | ScaleConnectionState::Error,
            ) && !s.auto_reconnect_suppressed
                && s.active.is_none();

            if !should_reconnect {
                continue;
            }

            let Some(saved) = s.saved_scale.clone() else {
                continue;
            };

            let connection_id = s.allocate_connection_id();
            let request = ConnectRequest {
                connection_id,
                address_text: saved.address,
                addr_type_str: saved.addr_type,
                name: saved.name,
            };
            s.state = ScaleConnectionState::Connecting;
            s.message = format!(
                "Reconnecting to {} in the background...",
                display_scale_name(&request.name),
            );
            s.active = Some(ActiveScaleConnection {
                id: request.connection_id,
                address_text: request.address_text.clone(),
                name: request.name.clone(),
                protocol: ScaleProtocol::Unknown,
            });
            s.reset_live_values();
            request
        };

        info!(
            "auto-reconnect: trying {}",
            display_scale_name(&request.name),
        );
        let connection_id = request.connection_id;
        if tx.send(WorkerCommand::ConnectTarget(request)).is_err() {
            let mut s = lock_or_recover(&state);
            if s.is_active_connection(connection_id) {
                s.state = ScaleConnectionState::Idle;
                s.message = "Bluetooth scale idle.".to_owned();
                s.active = None;
            }
            info!("auto-reconnect: worker channel closed, stopping");
            break;
        }
    }
}
