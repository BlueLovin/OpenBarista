use anyhow::anyhow;
use esp_idf_svc::io::Read;

use crate::scale_ble::ScaleStatusSnapshot;

use super::nvs::DeviceSettings;

// ---------------------------------------------------------------------------
// Security headers
// ---------------------------------------------------------------------------

pub(super) fn response_headers<'a>(
    content_type: &'a str,
    cache_control: &'a str,
) -> [(&'a str, &'a str); 7] {
    [
        ("Content-Type", content_type),
        ("Cache-Control", cache_control),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'",
        ),
        ("Permissions-Policy", "geolocation=(), microphone=(), camera=()"),
    ]
}

/// Station pages use uPlot which applies inline styles.
pub(super) fn station_response_headers<'a>(
    content_type: &'a str,
    cache_control: &'a str,
) -> [(&'a str, &'a str); 7] {
    [
        ("Content-Type", content_type),
        ("Cache-Control", cache_control),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'self'",
        ),
        ("Permissions-Policy", "geolocation=(), microphone=(), camera=()"),
    ]
}

// ---------------------------------------------------------------------------
// Request body reading
// ---------------------------------------------------------------------------

pub(super) enum RequestBodyError {
    TooLarge,
    InvalidUtf8,
    Io(anyhow::Error),
}

pub(super) fn read_request_body_utf8<R: Read>(
    reader: &mut R,
    max_body_len: usize,
) -> std::result::Result<String, RequestBodyError> {
    let mut body = Vec::with_capacity(max_body_len);

    loop {
        let mut buf = [0u8; 128];
        let n = reader
            .read(&mut buf)
            .map_err(|err| RequestBodyError::Io(anyhow!("request body read failed: {err:?}")))?;
        if n == 0 {
            break;
        }

        let remaining = max_body_len.saturating_sub(body.len());
        if n > remaining {
            return Err(RequestBodyError::TooLarge);
        }

        body.extend_from_slice(&buf[..n]);
    }

    let body_str = std::str::from_utf8(&body).map_err(|_| RequestBodyError::InvalidUtf8)?;
    Ok(body_str.to_owned())
}

// ---------------------------------------------------------------------------
// Build / board identity
// ---------------------------------------------------------------------------

pub(super) fn build_id() -> &'static str {
    option_env!("OPENBARISTA_BUILD_ID").unwrap_or("dev")
}

pub(super) fn board_id() -> String {
    let mut mac = [0u8; 6];
    let err = unsafe { esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if err != 0 {
        return "unknown".to_owned();
    }
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

// ---------------------------------------------------------------------------
// JSON serialization
// ---------------------------------------------------------------------------

pub(super) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn sanitize_telemetry_value(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

pub(super) fn networks_json(items: &[String]) -> String {
    let mut out = String::from("[");
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(item));
        out.push('"');
    }
    out.push(']');
    out
}

pub(super) fn connect_progress_json(progress: &super::ConnectProgress) -> String {
    format!(
        "{{\"stage\":\"{}\",\"ssid\":\"{}\",\"attempt\":{},\"total\":{},\"message\":\"{}\"}}",
        json_escape(&progress.stage),
        json_escape(&progress.ssid),
        progress.attempt,
        progress.total,
        json_escape(&progress.message),
    )
}

pub(super) fn settings_json(
    settings: &DeviceSettings,
    ip_addr: &str,
    build_id: &str,
    board_id: &str,
    ok: bool,
    message: &str,
    rebooting: bool,
) -> String {
    format!(
        "{{\"ok\":{},\"message\":\"{}\",\"rebooting\":{},\"ssid\":\"{}\",\"device_label\":\"{}\",\"temperature_offset_c\":{:.3},\"ip_addr\":\"{}\",\"build_id\":\"{}\",\"board_id\":\"{}\"}}",
        if ok { "true" } else { "false" },
        json_escape(message),
        if rebooting { "true" } else { "false" },
        json_escape(&settings.ssid),
        json_escape(&settings.device_label),
        settings.temperature_offset_c,
        json_escape(ip_addr),
        json_escape(build_id),
        json_escape(board_id),
    )
}

pub(super) fn telemetry_json(
    seq: u64,
    temperature_c: f32,
    pressure_bar: f32,
    pressure_psi: f32,
    scale_connected: bool,
    weight_g: f32,
    flow_gps: f32,
) -> String {
    format!(
        "{{\"seq\":{},\"temperature_c\":{:.3},\"pressure_bar\":{:.3},\"pressure_psi\":{:.3},\"scale_connected\":{},\"weight_g\":{:.3},\"flow_gps\":{:.3}}}",
        seq,
        sanitize_telemetry_value(temperature_c),
        sanitize_telemetry_value(pressure_bar),
        sanitize_telemetry_value(pressure_psi),
        if scale_connected { "true" } else { "false" },
        sanitize_telemetry_value(weight_g),
        sanitize_telemetry_value(flow_gps),
    )
}

pub(super) fn action_result_json(ok: bool, message: &str) -> String {
    format!(
        "{{\"ok\":{},\"message\":\"{}\"}}",
        if ok { "true" } else { "false" },
        json_escape(message),
    )
}

pub(super) fn scale_status_json(snapshot: &ScaleStatusSnapshot) -> String {
    let saved_scale = snapshot.saved_scale.as_ref().map_or_else(
        || "null".to_owned(),
        |saved| {
            format!(
                "{{\"address\":\"{}\",\"name\":\"{}\",\"addr_type\":\"{}\"}}",
                json_escape(&saved.address),
                json_escape(&saved.name),
                json_escape(&saved.addr_type),
            )
        },
    );

    let mut devices_json = String::from("[");
    for (idx, device) in snapshot.devices.iter().enumerate() {
        if idx > 0 {
            devices_json.push(',');
        }
        devices_json.push_str(&format!(
            "{{\"address\":\"{}\",\"name\":\"{}\",\"address_type\":\"{}\",\"rssi\":{},\"protocol_hint\":\"{}\",\"saved\":{}}}",
            json_escape(&device.address),
            json_escape(&device.name),
            json_escape(&device.address_type),
            device.rssi,
            json_escape(&device.protocol_hint),
            if device.saved { "true" } else { "false" },
        ));
    }
    devices_json.push(']');

    let battery_json = snapshot
        .battery_percent
        .map(|battery| battery.to_string())
        .unwrap_or_else(|| "null".to_owned());

    format!(
        "{{\"available\":{},\"state\":\"{}\",\"message\":\"{}\",\"connected_name\":\"{}\",\"connected_address\":\"{}\",\"protocol\":\"{}\",\"weight_g\":{:.3},\"flow_gps\":{:.3},\"battery_percent\":{},\"saved_scale\":{},\"devices\":{}}}",
        if snapshot.available { "true" } else { "false" },
        json_escape(&snapshot.state),
        json_escape(&snapshot.message),
        json_escape(&snapshot.connected_name),
        json_escape(&snapshot.connected_address),
        json_escape(&snapshot.protocol),
        sanitize_telemetry_value(snapshot.weight_g),
        sanitize_telemetry_value(snapshot.flow_gps),
        battery_json,
        saved_scale,
        devices_json,
    )
}

// ---------------------------------------------------------------------------
// Form parsing
// ---------------------------------------------------------------------------

pub(super) fn parse_form_field(body: &str, key: &str) -> Option<String> {
    form_urlencoded::parse(body.as_bytes()).find_map(|(k, v)| {
        if k == key {
            Some(v.into_owned())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_form_field_basic() {
        let body = "ssid=my%20network&password=secret+pass";
        assert_eq!(
            parse_form_field(body, "ssid"),
            Some("my network".to_string())
        );
        assert_eq!(
            parse_form_field(body, "password"),
            Some("secret pass".to_string())
        );
    }

    #[test]
    fn test_parse_form_field_missing_key() {
        let body = "ssid=myssid&password=secret";
        assert_eq!(parse_form_field(body, "unknown"), None);
    }

    #[test]
    fn test_parse_form_field_empty_password_for_open_network() {
        let body = "ssid=open_network&password=&mode=open";
        let password = parse_form_field(body, "password");
        assert_eq!(password, Some(String::new()));
    }
}
