use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use esp32_nimble::BLEAddressType;

use openbarista::sync_utils::lock_or_recover;

use super::types::{
    DiscoveredScaleInternal, ScaleManagerState, ScaleProtocol, MAX_DISCOVERED_SCALES,
    SCALE_NAME_HINTS,
};

pub(super) fn is_scale_like_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SCALE_NAME_HINTS.iter().any(|hint| lower.contains(hint))
}

pub(super) fn ble_addr_type_str(addr_type: BLEAddressType) -> &'static str {
    match addr_type {
        BLEAddressType::Public => "public",
        BLEAddressType::Random => "random",
        _ => "random",
    }
}

pub(super) fn parse_nimble_addr_type(value: &str) -> BLEAddressType {
    match value {
        "public" => BLEAddressType::Public,
        _ => BLEAddressType::Random,
    }
}

pub(super) fn display_scale_name(name: &str) -> &str {
    if name.trim().is_empty() {
        "selected scale"
    } else {
        name
    }
}

/// Cancel any in-progress BLE GAP connection attempt at the controller level.
pub(super) fn cancel_ble_connect() {
    let rc = unsafe { esp_idf_svc::sys::ble_gap_conn_cancel() };
    if rc == 0 {
        println!("[scale] ble_gap_conn_cancel succeeded");
    } else {
        println!("[scale] ble_gap_conn_cancel rc={rc} (no pending connect)");
    }
}

/// Cancel any pending GAP operations (scan, connect) so the next operation
/// starts cleanly.
pub(super) fn cancel_gap_operations() {
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
pub(super) fn get_own_ble_address() -> Option<String> {
    let mut addr = [0u8; 6];
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
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0]
    )
}

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

pub(super) fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn upsert_discovered(
    state: &Arc<Mutex<ScaleManagerState>>,
    incoming: DiscoveredScaleInternal,
) {
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
