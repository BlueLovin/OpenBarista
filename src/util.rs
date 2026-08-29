//! Small shared helpers.

/// Seconds since the start of the process (monotonic).
///
/// Used by both the health monitor (heartbeat) and the crash log (flush
/// throttle) so they share one clock instead of duplicating the boilerplate.
pub fn uptime_secs() -> u32 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs() as u32
}

/// Escape a string for safe inclusion in a JSON string value.
///
/// Shared by every JSON endpoint (telemetry, logs, scale, provisioning…).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_handles_quotes_and_control_chars() {
        assert_eq!(json_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(json_escape("a\nb\tc\\d"), "a\\nb\\tc\\\\d");
        assert_eq!(json_escape("nul\u{0}x"), "nul\\u0000x");
    }

    #[test]
    fn json_escape_passes_printable_unicode_through() {
        assert_eq!(json_escape("café ☕"), "café ☕");
    }
}
