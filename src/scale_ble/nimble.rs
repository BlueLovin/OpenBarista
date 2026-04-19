//! Safe wrappers around NimBLE FFI calls used by the scale transport.
//!
//! Every raw `unsafe` interaction with `esp_idf_svc::sys` lives here so the
//! rest of the crate stays safe.

use esp32_nimble::{BLEAddress, BLEAddressType};
use log::{debug, info, warn};

// ---------------------------------------------------------------------------
// Address helpers
// ---------------------------------------------------------------------------

pub fn ble_addr_type_str(addr_type: BLEAddressType) -> &'static str {
    match addr_type {
        BLEAddressType::Public => "public",
        BLEAddressType::Random => "random",
        _ => "random",
    }
}

pub fn parse_addr_type(value: &str) -> BLEAddressType {
    match value {
        "public" => BLEAddressType::Public,
        _ => BLEAddressType::Random,
    }
}

/// Format a 6-byte BLE address from NimBLE's LSB-first storage to the
/// conventional MSB-first colon-separated hex string.
fn format_ble_addr(bytes: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0],
    )
}

/// Read the ESP32's own BLE address so we can exclude it from scan results.
pub fn own_ble_address() -> Option<String> {
    let mut addr = [0u8; 6];
    // Try public address first.
    let rc = unsafe {
        esp_idf_svc::sys::ble_hs_id_copy_addr(0, addr.as_mut_ptr(), core::ptr::null_mut())
    };
    if rc == 0 && addr != [0u8; 6] {
        return Some(format_ble_addr(&addr));
    }
    // Fall back to random address.
    let rc = unsafe {
        esp_idf_svc::sys::ble_hs_id_copy_addr(1, addr.as_mut_ptr(), core::ptr::null_mut())
    };
    if rc == 0 && addr != [0u8; 6] {
        return Some(format_ble_addr(&addr));
    }
    None
}

// ---------------------------------------------------------------------------
// GAP operation management
// ---------------------------------------------------------------------------

/// Cancel an in-progress BLE GAP connection attempt.
/// Safe to call from any thread — NimBLE serialises internally.
pub fn cancel_connect() {
    let rc = unsafe { esp_idf_svc::sys::ble_gap_conn_cancel() };
    if rc == 0 {
        info!("ble_gap_conn_cancel succeeded");
    } else {
        debug!("ble_gap_conn_cancel rc={rc} (no pending connect)");
    }
}

/// Cancel any pending GAP operations (scan + connect) so the next operation
/// starts with a clean slate. Harmless if nothing is pending.
pub fn cancel_all_gap_operations() {
    unsafe {
        if esp_idf_svc::sys::ble_gap_disc_cancel() == 0 {
            info!("cancelled stale scan");
        }
        if esp_idf_svc::sys::ble_gap_conn_cancel() == 0 {
            info!("cancelled stale connect");
        }
    }
}

/// Terminate any lingering GAP connection to `addr_text` and wait for NimBLE
/// to fully remove it from the connection table. This prevents stale
/// connection-handle entries from corrupting the next connect attempt.
pub fn terminate_stale_connection(addr_text: &str, addr_type_str: &str) {
    let addr_type = parse_addr_type(addr_type_str);
    let Some(addr) = BLEAddress::from_str(addr_text, addr_type) else {
        return;
    };
    unsafe {
        let ble_addr: esp_idf_svc::sys::ble_addr_t = addr.into();
        let mut desc: esp_idf_svc::sys::ble_gap_conn_desc = core::mem::zeroed();
        if esp_idf_svc::sys::ble_gap_conn_find_by_addr(&ble_addr, &mut desc) != 0 {
            return; // No stale connection found.
        }
        info!(
            "cleanup: found stale conn_handle={} to {addr_text}, terminating",
            desc.conn_handle,
        );
        esp_idf_svc::sys::ble_gap_terminate(
            desc.conn_handle,
            esp_idf_svc::sys::ble_error_codes_BLE_ERR_REM_USER_CONN_TERM as _,
        );
        // Poll until NimBLE removes the entry (up to ~1 s).
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if esp_idf_svc::sys::ble_gap_conn_find_by_addr(&ble_addr, &mut desc) != 0 {
                debug!("cleanup: stale connection cleared");
                return;
            }
        }
        warn!("cleanup: stale connection may not have fully cleared");
    }
}

/// Forcibly terminate a connection to the given address if one exists.
/// Used by the connect watchdog when a timeout fires.
pub fn force_terminate_connection(addr_text: &str, addr_type_str: &str) {
    unsafe {
        esp_idf_svc::sys::ble_gap_conn_cancel();
    }
    let addr_type = parse_addr_type(addr_type_str);
    if let Some(addr) = BLEAddress::from_str(addr_text, addr_type) {
        unsafe {
            let ble_addr: esp_idf_svc::sys::ble_addr_t = addr.into();
            let mut desc: esp_idf_svc::sys::ble_gap_conn_desc = core::mem::zeroed();
            if esp_idf_svc::sys::ble_gap_conn_find_by_addr(&ble_addr, &mut desc) == 0 {
                info!(
                    "watchdog: terminating conn_handle={}",
                    desc.conn_handle,
                );
                esp_idf_svc::sys::ble_gap_terminate(
                    desc.conn_handle,
                    esp_idf_svc::sys::ble_error_codes_BLE_ERR_REM_USER_CONN_TERM as _,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

pub fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
