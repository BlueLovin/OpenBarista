//! BLE service / characteristic discovery and notification subscription.
//!
//! After connecting to a scale, this module walks the GATT table to find the
//! best weight channel and subscribes to notifications.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::BLERemoteCharacteristic;
use log::{debug, info};

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use super::nimble;
use super::types::{is_scale_like_name, ScaleConnectionState, ScaleManagerState, ScaleProtocol};
use super::weight;

// ---------------------------------------------------------------------------
// Well-known UUIDs
// ---------------------------------------------------------------------------

const UUID_SERVICE_WEIGHT_SCALE: u16 = 0x181D;
const UUID_CHAR_WEIGHT_MEASUREMENT: u16 = 0x2A9D;
const UUID_SERVICE_BATTERY: u16 = 0x180F;
const UUID_CHAR_BATTERY_LEVEL: u16 = 0x2A19;
const UUID_CHAR_BOOKOO_COMMAND: u16 = 0xFF12;

const COMMON_VENDOR_NOTIFY_UUIDS: &[u16] =
    &[0xFFF1, 0xFFF2, 0xFFF4, 0xFFE1, 0xFFE2, 0xFFE5, 0xFF11];
const COMMON_VENDOR_SERVICE_UUIDS: &[u16] =
    &[0xFFF0, 0xFFE0, 0xFFF1, 0xFFE1, 0xFFF5, 0xFFE5, 0x0FFE];

/// How many notification payloads to log in full before going silent.
const DEBUG_LOG_COUNT: u8 = 8;

pub(crate) struct DiscoveryResult {
    pub protocol: ScaleProtocol,
    pub bookoo_command_char: Option<BLERemoteCharacteristic>,
    pub supports_manual_brew_start: bool,
    pub supports_flow_smoothing: bool,
}

impl DiscoveryResult {
    fn new(protocol: ScaleProtocol) -> Self {
        Self {
            protocol,
            bookoo_command_char: None,
            supports_manual_brew_start: false,
            supports_flow_smoothing: false,
        }
    }

    fn with_bookoo_command(mut self, characteristic: Option<BLERemoteCharacteristic>) -> Self {
        self.bookoo_command_char = characteristic;
        self.supports_manual_brew_start = self.bookoo_command_char.is_some();
        self.supports_flow_smoothing = self.bookoo_command_char.is_some();
        self
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Walk the GATT table and subscribe to the best weight notification channel.
/// Returns the detected protocol on success.
pub async fn discover_and_subscribe(
    client: &mut esp32_nimble::BLEClient,
    connection_id: u64,
    scale_name: &str,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<DiscoveryResult> {
    let scale_like = is_scale_like_name(scale_name);

    // Priority 1: Standard Weight Scale Service (0x181D / 0x2A9D)
    if let Some(proto) = try_standard_service(client, connection_id, state, telemetry).await? {
        return Ok(proto);
    }

    // Priority 2: Common vendor services (0xFFF0, 0xFFE0, etc.)
    if let Some(proto) = try_vendor_services(client, connection_id, state, telemetry).await? {
        return Ok(proto);
    }

    // Priority 3: Brute-force — subscribe to all notifying chars on any
    // plausible service if the device looks like a scale.
    if scale_like {
        if let Some(proto) = try_brute_force(client, connection_id, state, telemetry).await? {
            return Ok(proto);
        }
    }

    Err(anyhow!("No weight characteristic found on this device."))
}

/// Read battery level if the standard Battery Service is available.
pub async fn read_battery(
    client: &mut esp32_nimble::BLEClient,
    connection_id: u64,
    state: &Arc<Mutex<ScaleManagerState>>,
) {
    let Ok(service) = client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_BATTERY))
        .await
    else {
        return;
    };
    let Ok(characteristic) = service
        .get_characteristic(BleUuid::from_uuid16(UUID_CHAR_BATTERY_LEVEL))
        .await
    else {
        return;
    };
    if let Ok(value) = characteristic.read_value().await {
        if let Some(&level) = value.first() {
            info!("battery={level}%");
            let mut s = lock_or_recover(state);
            if s.is_active_connection(connection_id) {
                s.battery_percent = Some(level);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Priority 1 — Standard Weight Scale Service
// ---------------------------------------------------------------------------

async fn try_standard_service(
    client: &mut esp32_nimble::BLEClient,
    connection_id: u64,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<Option<DiscoveryResult>> {
    let Ok(service) = client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_WEIGHT_SCALE))
        .await
    else {
        return Ok(None);
    };

    // Try the standard characteristic first.
    if let Ok(characteristic) = service
        .get_characteristic(BleUuid::from_uuid16(UUID_CHAR_WEIGHT_MEASUREMENT))
        .await
    {
        if characteristic.can_notify() || characteristic.can_indicate() || characteristic.can_read()
        {
            info!(
                "found standard weight char 0x{UUID_CHAR_WEIGHT_MEASUREMENT:04X} on service 0x{UUID_SERVICE_WEIGHT_SCALE:04X}",
            );
            let label = format!(
                "0x{UUID_CHAR_WEIGHT_MEASUREMENT:04X} on service 0x{UUID_SERVICE_WEIGHT_SCALE:04X}",
            );
            subscribe(
                characteristic,
                ScaleProtocol::StandardWeight,
                label,
                connection_id,
                state,
                telemetry,
            )
            .await?;
            return Ok(Some(DiscoveryResult::new(ScaleProtocol::StandardWeight)));
        }
    }

    // Fall back: try all notifying chars on the same service.
    if let Ok(chars) = service.get_characteristics().await {
        let mut subscribed = false;
        for characteristic in chars {
            if characteristic.can_notify() || characteristic.can_indicate() {
                let label = format!(
                    "{} on service 0x{UUID_SERVICE_WEIGHT_SCALE:04X}",
                    characteristic.uuid(),
                );
                info!(
                    "found alternate notify char {} in weight-scale service",
                    characteristic.uuid()
                );
                subscribe(
                    characteristic,
                    ScaleProtocol::GenericNotify,
                    label,
                    connection_id,
                    state,
                    telemetry,
                )
                .await?;
                subscribed = true;
            }
        }
        if subscribed {
            return Ok(Some(DiscoveryResult::new(ScaleProtocol::GenericNotify)));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Priority 2 — Common vendor services
// ---------------------------------------------------------------------------

async fn try_vendor_services(
    client: &mut esp32_nimble::BLEClient,
    connection_id: u64,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<Option<DiscoveryResult>> {
    for &svc_uuid16 in COMMON_VENDOR_SERVICE_UUIDS {
        let Ok(service) = client.get_service(BleUuid::from_uuid16(svc_uuid16)).await else {
            continue;
        };

        let bookoo_command_char = if svc_uuid16 == 0x0FFE {
            service
                .get_characteristic(BleUuid::from_uuid16(UUID_CHAR_BOOKOO_COMMAND))
                .await
                .ok()
                .and_then(|characteristic| {
                    if characteristic.can_write() || characteristic.can_write_no_response() {
                        Some(characteristic.clone())
                    } else {
                        None
                    }
                })
        } else {
            None
        };

        let mut subscribed = false;
        let mut seen = BTreeSet::new();

        // Known vendor char UUIDs first.
        for &char_uuid16 in COMMON_VENDOR_NOTIFY_UUIDS {
            let Ok(characteristic) = service
                .get_characteristic(BleUuid::from_uuid16(char_uuid16))
                .await
            else {
                continue;
            };

            let label = format!("{} on service 0x{svc_uuid16:04X}", characteristic.uuid());
            if !seen.insert(label.clone()) {
                continue;
            }
            if !characteristic.can_notify() && !characteristic.can_indicate() {
                continue;
            }

            // Detect Bookoo: service 0x0FFE + char 0xFF11.
            let proto = if svc_uuid16 == 0x0FFE && char_uuid16 == 0xFF11 {
                info!("detected Bookoo protocol (svc 0x0FFE / char 0xFF11)");
                ScaleProtocol::Bookoo
            } else {
                ScaleProtocol::GenericNotify
            };
            info!(
                "found vendor char 0x{char_uuid16:04X} on service 0x{svc_uuid16:04X} proto={}",
                proto.as_str(),
            );
            subscribe(
                characteristic,
                proto,
                label,
                connection_id,
                state,
                telemetry,
            )
            .await?;
            if proto == ScaleProtocol::Bookoo {
                return Ok(Some(
                    DiscoveryResult::new(ScaleProtocol::Bookoo)
                        .with_bookoo_command(bookoo_command_char.clone()),
                ));
            }
            subscribed = true;
        }

        // Any other notifying char on this vendor service.
        if let Ok(chars) = service.get_characteristics().await {
            for characteristic in chars {
                let label = format!("{} on service 0x{svc_uuid16:04X}", characteristic.uuid());
                if !seen.insert(label.clone()) {
                    continue;
                }
                if !characteristic.can_notify() && !characteristic.can_indicate() {
                    continue;
                }
                info!(
                    "found notify char {} in vendor service 0x{svc_uuid16:04X}",
                    characteristic.uuid(),
                );
                subscribe(
                    characteristic,
                    ScaleProtocol::GenericNotify,
                    label,
                    connection_id,
                    state,
                    telemetry,
                )
                .await?;
                subscribed = true;
            }
        }

        if subscribed {
            return Ok(Some(DiscoveryResult::new(ScaleProtocol::GenericNotify)));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Priority 3 — Brute-force
// ---------------------------------------------------------------------------

async fn try_brute_force(
    client: &mut esp32_nimble::BLEClient,
    connection_id: u64,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<Option<DiscoveryResult>> {
    let Ok(services) = client.get_services().await else {
        return Ok(None);
    };

    let mut subscribed = false;
    for service in services {
        let svc_uuid = service.uuid();
        // Skip standard services that aren't weight-related.
        if let BleUuid::Uuid16(v) = svc_uuid {
            if v == 0x1800 || v == 0x1801 || v == UUID_SERVICE_BATTERY {
                continue;
            }
        }
        let Ok(chars) = service.get_characteristics().await else {
            continue;
        };
        for characteristic in chars {
            if !characteristic.can_notify() && !characteristic.can_indicate() {
                continue;
            }
            let label = format!("{} on service {svc_uuid}", characteristic.uuid());
            info!(
                "brute-force: using char {} on service {svc_uuid}",
                characteristic.uuid()
            );
            subscribe(
                characteristic,
                ScaleProtocol::GenericNotify,
                label,
                connection_id,
                state,
                telemetry,
            )
            .await?;
            subscribed = true;
        }
    }

    Ok(if subscribed {
        Some(DiscoveryResult::new(ScaleProtocol::GenericNotify))
    } else {
        None
    })
}

// ---------------------------------------------------------------------------
// Notification subscription + weight application
// ---------------------------------------------------------------------------

async fn subscribe(
    characteristic: &mut esp32_nimble::BLERemoteCharacteristic,
    protocol: ScaleProtocol,
    channel_label: String,
    connection_id: u64,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<()> {
    let notify_state = state.clone();
    let notify_telemetry = telemetry.clone();
    let notify_label = channel_label.clone();
    let mut debug_remaining: u8 = DEBUG_LOG_COUNT;

    characteristic.on_notify(move |data| {
        let Some((previous_weight_g, should_log)) = ({
            let s = lock_or_recover(&notify_state);
            if !s.is_active_connection(connection_id) {
                None
            } else {
                Some((s.weight_g, debug_remaining > 0))
            }
        }) else {
            return;
        };

        if should_log {
            debug_remaining = debug_remaining.saturating_sub(1);
            debug!(
                "notify char={} proto={} bytes={} len={}",
                notify_label,
                protocol.as_str(),
                nimble::hex_bytes(data),
                data.len(),
            );
        }

        if let Some(weight_g) = weight::parse_weight(protocol, data, previous_weight_g) {
            if should_log {
                debug!("parsed weight_g={weight_g:.2} (prev={previous_weight_g:.2})");
            }
            apply_weight(&notify_state, &notify_telemetry, connection_id, weight_g);
        }
    });

    if characteristic.can_indicate() && !characteristic.can_notify() {
        characteristic
            .subscribe_indicate(false)
            .await
            .map_err(|e| anyhow!("indication subscribe failed: {e:?}"))?;
        info!("subscribed to indications on {channel_label}");
    } else {
        characteristic
            .subscribe_notify(false)
            .await
            .map_err(|e| anyhow!("notify subscribe failed: {e:?}"))?;
        info!("subscribed to notifications on {channel_label}");
    }

    // Read the current value if readable (initial seed).
    if characteristic.can_read() {
        if let Ok(value) = characteristic.read_value().await {
            if !value.is_empty() {
                debug!(
                    "initial read char={} proto={} bytes={} len={}",
                    channel_label,
                    protocol.as_str(),
                    nimble::hex_bytes(&value),
                    value.len(),
                );
                let previous_weight_g = lock_or_recover(state).weight_g;
                if let Some(weight_g) = weight::parse_weight(protocol, &value, previous_weight_g) {
                    apply_weight(state, telemetry, connection_id, weight_g);
                }
            }
        }
    }

    Ok(())
}

fn apply_weight(
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
    connection_id: u64,
    weight_g: f32,
) {
    let mut s = lock_or_recover(state);
    if !s.is_active_connection(connection_id) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;
    let flow_gps = s.flow_estimator.observe(weight_g, now);
    s.weight_g = weight_g;
    s.flow_gps = flow_gps;
    s.state = ScaleConnectionState::Ready;
    telemetry.update_scale(true, weight_g, flow_gps);
}
