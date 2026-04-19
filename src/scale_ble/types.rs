//! Shared types for the scale BLE subsystem.
//!
//! All public types that cross the module boundary live here so consumers
//! (`wifi_provision`, `main`) import a single coherent set of definitions.

use openbarista::telemetry_math::FlowEstimator;

pub use openbarista::scale_weight::ScaleProtocol;

// ---------------------------------------------------------------------------
// Connection state enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleConnectionState {
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

// ---------------------------------------------------------------------------
// Public data-transfer types
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
    pub protocol_hint: &'static str,
    pub saved: bool,
}

#[derive(Debug, Clone)]
pub struct ScaleStatusSnapshot {
    pub available: bool,
    pub state: &'static str,
    pub message: String,
    pub connected_name: String,
    pub connected_address: String,
    pub protocol: &'static str,
    pub weight_g: f32,
    pub flow_gps: f32,
    pub battery_percent: Option<u8>,
    pub saved_scale: Option<SavedScale>,
    pub devices: Vec<DiscoveredScale>,
}

// ---------------------------------------------------------------------------
// Internal discovered device
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredScaleInternal {
    pub address_text: String,
    pub addr_type_str: String,
    pub name: String,
    pub rssi: i32,
    pub protocol_hint: ScaleProtocol,
    pub scale_like: bool,
}

// ---------------------------------------------------------------------------
// Active connection token
// ---------------------------------------------------------------------------

pub(crate) struct ActiveScaleConnection {
    pub id: u64,
    pub address_text: String,
    pub name: String,
    pub protocol: ScaleProtocol,
}

// ---------------------------------------------------------------------------
// Internal manager state
// ---------------------------------------------------------------------------

pub(crate) struct ScaleManagerState {
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
    next_connection_id: u64,
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
            next_connection_id: 1,
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
            .map(|a| a.protocol.as_str())
            .unwrap_or(ScaleProtocol::Unknown.as_str());

        let devices = self
            .discovered
            .iter()
            .map(|d| DiscoveredScale {
                address: d.address_text.clone(),
                name: d.name.clone(),
                address_type: d.addr_type_str.clone(),
                rssi: d.rssi,
                protocol_hint: d.protocol_hint.as_str(),
                saved: self
                    .saved_scale
                    .as_ref()
                    .map(|s| s.address.eq_ignore_ascii_case(&d.address_text))
                    .unwrap_or(false),
            })
            .collect();

        ScaleStatusSnapshot {
            available: self.available,
            state: self.state.as_str(),
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

    pub fn allocate_connection_id(&mut self) -> u64 {
        let id = self.next_connection_id;
        self.next_connection_id = self.next_connection_id.wrapping_add(1);
        if self.next_connection_id == 0 {
            self.next_connection_id = 1;
        }
        id
    }

    pub fn is_active_connection(&self, connection_id: u64) -> bool {
        self.active
            .as_ref()
            .map(|a| a.id == connection_id)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) const MAX_DISCOVERED_SCALES: usize = 18;

/// Human-readable display name with a fallback for blank names.
pub(crate) fn display_scale_name(name: &str) -> &str {
    if name.trim().is_empty() {
        "selected scale"
    } else {
        name
    }
}

pub(crate) const SCALE_NAME_HINTS: &[&str] = &[
    "scale", "acaia", "felicita", "pearl", "lunar", "decent", "timemore", "mirror", "bookoo",
    "atomax",
];

pub(crate) fn is_scale_like_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SCALE_NAME_HINTS.iter().any(|hint| lower.contains(hint))
}

/// Insert or update a discovered device in the list, keeping it sorted
/// (scale-like first, then by RSSI descending, then by name).
pub(crate) fn upsert_discovered(
    state: &std::sync::Arc<std::sync::Mutex<ScaleManagerState>>,
    incoming: DiscoveredScaleInternal,
) {
    let mut s = openbarista::sync_utils::lock_or_recover(state);
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
