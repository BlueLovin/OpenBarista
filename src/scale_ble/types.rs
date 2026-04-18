use std::fmt;

use openbarista::telemetry_math::FlowEstimator;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(super) const SCALE_SCAN_DURATION_S: u32 = 6;
pub(super) const CONNECT_TIMEOUT_MS: u32 = 2_000;
pub(super) const CONNECT_MAX_ATTEMPTS: u32 = 10;
pub(super) const RECONNECT_POLL_INTERVAL_MS: u64 = 5_000;
pub(super) const MAX_DISCOVERED_SCALES: usize = 18;
pub(super) const SCALE_READY_MESSAGE: &str = "Bluetooth scale ready. Tap Find Scales to pair.";
pub(super) const SCALE_STARTUP_MESSAGE: &str = "Starting Bluetooth scale transport...";

pub(super) const UUID_SERVICE_WEIGHT_SCALE: u16 = 0x181D;
pub(super) const UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT: u16 = 0x2A9D;
pub(super) const UUID_SERVICE_BATTERY: u16 = 0x180F;
pub(super) const UUID_CHARACTERISTIC_BATTERY_LEVEL: u16 = 0x2A19;

pub(super) const COMMON_VENDOR_NOTIFY_UUIDS: &[u16] =
    &[0xFFF1, 0xFFF2, 0xFFF4, 0xFFE1, 0xFFE2, 0xFFE5, 0xFF11];
pub(super) const COMMON_VENDOR_SERVICE_UUIDS: &[u16] =
    &[0xFFF0, 0xFFE0, 0xFFF1, 0xFFE1, 0xFFF5, 0xFFE5, 0x0FFE];
pub(super) const SCALE_NAME_HINTS: &[&str] = &[
    "scale", "acaia", "felicita", "pearl", "lunar", "decent", "timemore", "mirror", "bookoo",
    "atomax",
];

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaleConnectionState {
    Idle,
    Scanning,
    Connecting,
    Discovering,
    Ready,
    Error,
}

impl ScaleConnectionState {
    pub fn as_str(self) -> &'static str {
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

impl fmt::Display for ScaleConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaleProtocol {
    StandardWeight,
    GenericNotify,
    Bookoo,
    Unknown,
}

impl ScaleProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StandardWeight => "standard-weight",
            Self::GenericNotify => "generic-notify",
            Self::Bookoo => "bookoo",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ScaleProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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
pub(super) struct DiscoveredScaleInternal {
    pub address_text: String,
    pub addr_type_str: String,
    pub name: String,
    pub rssi: i32,
    pub protocol_hint: ScaleProtocol,
    pub scale_like: bool,
}

pub(super) struct ActiveScaleConnection {
    pub address_text: String,
    pub name: String,
    pub protocol: ScaleProtocol,
}

pub(super) struct ScaleManagerState {
    pub available: bool,
    pub state: ScaleConnectionState,
    pub message: String,
    pub transport_ready: bool,
    pub discovered: Vec<DiscoveredScaleInternal>,
    pub active: Option<ActiveScaleConnection>,
    pub saved_scale: Option<SavedScale>,
    pub auto_reconnect_suppressed: bool,
    pub weight_g: f32,
    pub flow_gps: f32,
    pub battery_percent: Option<u8>,
    pub flow_estimator: FlowEstimator,
}

impl ScaleManagerState {
    pub fn new(available: bool, message: String) -> Self {
        Self {
            available,
            state: ScaleConnectionState::Idle,
            message,
            transport_ready: false,
            discovered: Vec::new(),
            active: None,
            saved_scale: None,
            auto_reconnect_suppressed: false,
            weight_g: 0.0,
            flow_gps: 0.0,
            battery_percent: None,
            flow_estimator: FlowEstimator::new(),
        }
    }

    pub fn snapshot(&self) -> ScaleStatusSnapshot {
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

    pub fn reset_live_values(&mut self) {
        self.weight_g = 0.0;
        self.flow_gps = 0.0;
        self.battery_percent = None;
        self.flow_estimator.reset();
    }
}

pub(super) struct ConnectRequest {
    pub address_text: String,
    pub addr_type_str: String,
    pub name: String,
}

pub(super) enum WorkerCommand {
    StartScan,
    ConnectTarget(ConnectRequest),
    Disconnect,
}
