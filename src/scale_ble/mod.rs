mod discovery;
mod protocol;
mod types;
mod util;
mod worker;

use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

pub use types::{SavedScale, ScaleStatusSnapshot};

use types::*;
use util::{cancel_ble_connect, display_scale_name};

// ---------------------------------------------------------------------------
// ScaleRuntime — public entry point
// ---------------------------------------------------------------------------

pub struct ScaleRuntime {
    state: Arc<Mutex<ScaleManagerState>>,
    worker_tx: Option<mpsc::Sender<WorkerCommand>>,
    telemetry: SharedTelemetry,
    _worker_thread: Option<thread::JoinHandle<()>>,
    _reconnect_thread: Option<thread::JoinHandle<()>>,
}

impl ScaleRuntime {
    pub fn disabled(message: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScaleManagerState::new(false, message.into()))),
            worker_tx: None,
            telemetry: SharedTelemetry::new(),
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

        let reconn_state = state.clone();
        let reconn_tx = worker_tx.clone();
        let reconnect_thread = thread::Builder::new()
            .name("ble-reconn".into())
            .stack_size(4096)
            .spawn(move || {
                reconnect_loop(reconn_state, reconn_tx);
            })?;

        Ok(Self {
            state,
            worker_tx: Some(worker_tx),
            telemetry,
            _worker_thread: Some(worker_thread),
            _reconnect_thread: Some(reconnect_thread),
        })
    }

    pub fn snapshot(&self) -> ScaleStatusSnapshot {
        lock_or_recover(&self.state).snapshot()
    }

    pub fn apply_saved_scale(&self, saved_scale: Option<SavedScale>) {
        lock_or_recover(&self.state).saved_scale = saved_scale;
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
            state.auto_reconnect_suppressed = false;
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

        {
            let mut state = lock_or_recover(&self.state);
            state.auto_reconnect_suppressed = false;
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
            let was_connecting = state.state == ScaleConnectionState::Connecting
                || state.state == ScaleConnectionState::Discovering;
            state.active = None;
            state.state = ScaleConnectionState::Idle;
            state.message = "Disconnected.".to_owned();
            state.auto_reconnect_suppressed = true;
            state.reset_live_values();

            if was_connecting {
                cancel_ble_connect();
            }
        }
        self.telemetry.clear_scale();

        let _ = self.send_command(WorkerCommand::Disconnect);
        Ok("Disconnected.")
    }

    pub fn forget_saved_scale(&self) {
        lock_or_recover(&self.state).saved_scale = None;
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
// Auto-reconnect background loop
// ---------------------------------------------------------------------------

fn reconnect_loop(state: Arc<Mutex<ScaleManagerState>>, tx: mpsc::Sender<WorkerCommand>) {
    thread::sleep(Duration::from_secs(2));
    loop {
        thread::sleep(Duration::from_millis(RECONNECT_POLL_INTERVAL_MS));
        let request = {
            let s = lock_or_recover(&state);
            let idle_or_error = matches!(
                s.state,
                ScaleConnectionState::Idle | ScaleConnectionState::Error
            );
            if !idle_or_error || s.auto_reconnect_suppressed || s.active.is_some() {
                continue;
            }
            let Some(saved) = s.saved_scale.as_ref() else {
                continue;
            };
            ConnectRequest {
                address_text: saved.address.clone(),
                addr_type_str: saved.addr_type.clone(),
                name: saved.name.clone(),
            }
        };

        println!(
            "[scale] auto-reconnect: trying {}",
            display_scale_name(&request.name)
        );

        {
            let mut s = lock_or_recover(&state);
            s.state = ScaleConnectionState::Connecting;
            s.message = format!(
                "Auto-connecting to {}...",
                display_scale_name(&request.name)
            );
            s.active = Some(ActiveScaleConnection {
                address_text: request.address_text.clone(),
                name: request.name.clone(),
                protocol: ScaleProtocol::Unknown,
            });
            s.reset_live_values();
        }

        if tx.send(WorkerCommand::ConnectTarget(request)).is_err() {
            let mut s = lock_or_recover(&state);
            s.state = ScaleConnectionState::Error;
            s.message = "Auto-reconnect failed: BLE worker is unavailable.".to_owned();
            s.active = None;
            println!("[scale] auto-reconnect: worker channel closed, stopping");
            break;
        }
    }
}
