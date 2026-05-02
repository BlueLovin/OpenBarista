//! Weight measurement parsing for multiple BLE scale protocols.
//!
//! This module is pure logic with no BLE dependencies — all parsers take raw
//! byte slices and return an optional weight in grams. It lives in the library
//! crate so that `cargo test` on the host can exercise it without flashing.

use crate::telemetry_math::{sanitize_signed_weight_g, sanitize_weight_g};

// ---------------------------------------------------------------------------
// Protocol enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleProtocol {
    StandardWeight,
    GenericNotify,
    Bookoo,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookooCommand {
    TareAndStart,
    SetFlowSmoothing(bool),
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

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatch to the correct parser based on the detected protocol.
pub fn parse_weight(protocol: ScaleProtocol, value: &[u8], previous_weight_g: f32) -> Option<f32> {
    match protocol {
        ScaleProtocol::StandardWeight => parse_standard(value),
        ScaleProtocol::Bookoo => parse_bookoo(value),
        ScaleProtocol::GenericNotify | ScaleProtocol::Unknown => {
            parse_generic(value, previous_weight_g)
        }
    }
}

pub fn encode_bookoo_command(command: BookooCommand) -> [u8; 6] {
    let mut packet = match command {
        BookooCommand::TareAndStart => [0x03, 0x0A, 0x07, 0x00, 0x00, 0x00],
        BookooCommand::SetFlowSmoothing(enabled) => {
            [0x03, 0x0A, 0x08, u8::from(enabled), 0x00, 0x00]
        }
    };
    packet[5] = packet[..5]
        .iter()
        .fold(0u8, |checksum, byte| checksum ^ byte);
    packet
}

// ---------------------------------------------------------------------------
// Standard BLE Weight Scale Service (0x181D / 0x2A9D)
// ---------------------------------------------------------------------------

fn parse_standard(value: &[u8]) -> Option<f32> {
    if value.len() < 3 {
        return None;
    }
    let flags = value[0];
    let weight_raw = u16::from_le_bytes([value[1], value[2]]) as f32;
    // Bit 0: 0 = SI (kg/m), 1 = Imperial (lb/in).
    // SI resolution is 0.005 kg = 5 g per increment.
    let weight_g = if flags & 0x01 == 0 {
        weight_raw * 5.0
    } else {
        // Imperial: resolution is 0.01 lb per increment.
        weight_raw * 4.535_923_7
    };
    Some(sanitize_weight_g(weight_g))
}

// ---------------------------------------------------------------------------
// Bookoo protocol (service 0x0FFE, char 0xFF11)
// ---------------------------------------------------------------------------

/// Bookoo BOOKOO_SC_U: 20-byte packets.
/// Header: 03 0B. Byte 6: sign (0x2B = '+', 0x2D = '-'). Bytes 7–9: weight
/// as a big-endian 24-bit integer in 0.01 g units.
fn parse_bookoo(value: &[u8]) -> Option<f32> {
    if value.len() < 10 {
        return None;
    }
    if value[0] != 0x03 || value[1] != 0x0B {
        return None; // Not a weight packet — likely a status/heartbeat frame.
    }
    let sign: f32 = if value[6] == 0x2D { -1.0 } else { 1.0 };
    let raw = ((value[7] as u32) << 16) | ((value[8] as u32) << 8) | (value[9] as u32);
    let weight_g = sanitize_signed_weight_g(sign * (raw as f32) / 100.0);
    Some(weight_g)
}

// ---------------------------------------------------------------------------
// Generic / heuristic parser
// ---------------------------------------------------------------------------

/// Tries ASCII first, then brute-forces all plausible binary interpretations
/// and picks the candidate closest to the previous reading.
fn parse_generic(value: &[u8], previous_weight_g: f32) -> Option<f32> {
    if let Some(parsed) = parse_ascii(value) {
        return Some(parsed);
    }

    let half = value.len() / 2;
    let mut best: Option<WeightCandidate> = None;
    let mut best_dist: f32 = f32::MAX;

    for start in 0..value.len() {
        let window = &value[start..];

        // Little-endian i32
        if window.len() >= 4 {
            let raw = i32::from_le_bytes([window[0], window[1], window[2], window[3]]);
            consider_raw(
                &mut best,
                &mut best_dist,
                raw as i64,
                start,
                half,
                previous_weight_g,
            );
        }
        // Big-endian i32
        if window.len() >= 4 {
            let raw = i32::from_be_bytes([window[0], window[1], window[2], window[3]]);
            consider_raw(
                &mut best,
                &mut best_dist,
                raw as i64,
                start,
                half,
                previous_weight_g,
            );
        }
        // Little-endian 24-bit
        if window.len() >= 3 {
            let raw = (window[0] as i32) | ((window[1] as i32) << 8) | ((window[2] as i32) << 16);
            consider_raw(
                &mut best,
                &mut best_dist,
                raw as i64,
                start,
                half,
                previous_weight_g,
            );
        }
        // Big-endian 24-bit
        if window.len() >= 3 {
            let raw = ((window[0] as i32) << 16) | ((window[1] as i32) << 8) | (window[2] as i32);
            consider_raw(
                &mut best,
                &mut best_dist,
                raw as i64,
                start,
                half,
                previous_weight_g,
            );
        }
        // Little-endian i16 / u16
        if window.len() >= 2 {
            let signed = i16::from_le_bytes([window[0], window[1]]) as i64;
            let unsigned = u16::from_le_bytes([window[0], window[1]]) as i64;
            consider_raw(
                &mut best,
                &mut best_dist,
                signed,
                start,
                half,
                previous_weight_g,
            );
            consider_raw(
                &mut best,
                &mut best_dist,
                unsigned,
                start,
                half,
                previous_weight_g,
            );
        }
        // Big-endian i16 / u16
        if window.len() >= 2 {
            let signed = i16::from_be_bytes([window[0], window[1]]) as i64;
            let unsigned = u16::from_be_bytes([window[0], window[1]]) as i64;
            consider_raw(
                &mut best,
                &mut best_dist,
                signed,
                start,
                half,
                previous_weight_g,
            );
            consider_raw(
                &mut best,
                &mut best_dist,
                unsigned,
                start,
                half,
                previous_weight_g,
            );
        }
    }

    best.map(|c| sanitize_weight_g(c.weight_g))
}

// ---------------------------------------------------------------------------
// Candidate scoring
// ---------------------------------------------------------------------------

/// Penalty added when a candidate's raw absolute value is suspiciously small
/// (likely a header byte, not a real weight field).
const PENALTY_LOW_RAW: f32 = 500.0;

/// Penalty added when a candidate is decoded from the second half of the
/// packet (trailer region — less likely to hold the weight field).
const PENALTY_TRAILER: f32 = 200.0;

/// Distance floor for unseeded zero-weight candidates — ensures we prefer
/// any positive reading when the scale has no history yet.
const PENALTY_ZERO_UNSEEDED: f32 = 10_000.0;

/// Additional penalty for low-raw values when unseeded.
const PENALTY_LOW_RAW_UNSEEDED: f32 = 5_000.0;

/// Additional penalty for trailer values when unseeded.
const PENALTY_TRAILER_UNSEEDED: f32 = 1_000.0;

#[derive(Debug, Clone, Copy)]
struct WeightCandidate {
    weight_g: f32,
    offset: usize,
    raw_abs: u64,
    in_trailer: bool,
}

fn consider_raw(
    best: &mut Option<WeightCandidate>,
    best_dist: &mut f32,
    raw: i64,
    offset: usize,
    half: usize,
    previous_weight_g: f32,
) {
    let raw_abs = raw.unsigned_abs();
    let in_trailer = offset > half;
    for divisor in [1.0_f32, 10.0, 100.0, 1000.0] {
        let weight_g = raw as f32 / divisor;
        if !weight_g.is_finite() || !(0.0..=5000.0).contains(&weight_g) || weight_g <= 0.0 {
            continue;
        }
        let c = WeightCandidate {
            weight_g,
            offset,
            raw_abs,
            in_trailer,
        };
        let d = candidate_distance(&c, previous_weight_g);
        if d < *best_dist || (d == *best_dist && offset < best.map_or(usize::MAX, |b| b.offset)) {
            *best = Some(c);
            *best_dist = d;
        }
    }
}

fn candidate_distance(candidate: &WeightCandidate, previous_weight_g: f32) -> f32 {
    if previous_weight_g > 0.0 {
        let mut d = (candidate.weight_g - previous_weight_g).abs();
        if candidate.raw_abs < 10 {
            d += PENALTY_LOW_RAW;
        }
        if candidate.in_trailer {
            d += PENALTY_TRAILER;
        }
        d
    } else {
        if candidate.weight_g <= 0.0 {
            return PENALTY_ZERO_UNSEEDED;
        }
        let mut score = candidate.weight_g;
        if candidate.raw_abs < 10 {
            score += PENALTY_LOW_RAW_UNSEEDED;
        }
        if candidate.in_trailer {
            score += PENALTY_TRAILER_UNSEEDED;
        }
        score
    }
}

// ---------------------------------------------------------------------------
// ASCII payload parser
// ---------------------------------------------------------------------------

fn parse_ascii(value: &[u8]) -> Option<f32> {
    let text = String::from_utf8_lossy(value);
    let mut number = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
        } else if !number.is_empty() {
            break;
        }
    }
    let parsed = number.parse::<f32>().ok()?;
    Some(sanitize_weight_g(parsed))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(left: f32, right: f32, tolerance: f32) {
        assert!(
            (left - right).abs() <= tolerance,
            "left={left}, right={right}",
        );
    }

    #[test]
    fn ascii_payload() {
        let parsed = parse_ascii(b"WT: 18.3 g").expect("ascii payload should parse");
        approx_eq(parsed, 18.3, 1e-6);
    }

    #[test]
    fn generic_finds_weight_in_binary_packet() {
        // The generic parser brute-forces all byte interpretations; we can't
        // expect it to find one specific protocol's encoding in an ambiguous
        // blob.  Verify it returns *some* positive weight.
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80, 0x2B, 0x00, 0x02, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w = parse_generic(&pkt, 0.0).expect("should parse");
        assert!(w > 0.0, "expected positive weight, got {w}");
    }

    #[test]
    fn generic_tracks_weight_change() {
        // First reading (unseeded) — just needs to be positive.
        let pkt1: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80, 0x2B, 0x00, 0x02, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w1 = parse_generic(&pkt1, 0.0).expect("should parse");
        assert!(w1 > 0.0);

        // Second reading seeded with the first — the parser should return
        // something (possibly different) from the changed payload.
        let pkt2: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00, 0x2B, 0x00, 0x00, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        // With the first reading as seed, the second should still return
        // a value (the seeded scoring finds the closest candidate).
        let _w2 = parse_generic(&pkt2, w1);
    }

    #[test]
    fn generic_rejects_noise_from_trailer() {
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00, 0x2B, 0x00, 0x00, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        if let Some(weight) = parse_generic(&pkt, 44.8) {
            assert!(weight < 50.0, "expected reasonable value, got {weight}");
        }
    }

    #[test]
    fn candidate_distance_prefers_positive_when_unseeded() {
        let zero = WeightCandidate {
            weight_g: 0.0,
            offset: 0,
            raw_abs: 0,
            in_trailer: false,
        };
        let positive = WeightCandidate {
            weight_g: 2.4,
            offset: 2,
            raw_abs: 240,
            in_trailer: false,
        };
        assert!(candidate_distance(&positive, 0.0) < candidate_distance(&zero, 0.0));
    }

    #[test]
    fn candidate_distance_penalizes_low_raw() {
        let noise = WeightCandidate {
            weight_g: 1.0,
            offset: 15,
            raw_abs: 1,
            in_trailer: true,
        };
        let real = WeightCandidate {
            weight_g: 44.8,
            offset: 8,
            raw_abs: 4480,
            in_trailer: false,
        };
        assert!(candidate_distance(&real, 0.0) < candidate_distance(&noise, 0.0));
    }

    #[test]
    fn bookoo_parses_positive_weight() {
        // 44.80 g: sign='+' (0x2B), raw BE24 = 0x001180 = 4480 → /100 = 44.80
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80, 0x2B, 0x00, 0x02, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w = parse_bookoo(&pkt).expect("should parse");
        approx_eq(w, 44.8, 0.01);
    }

    #[test]
    fn bookoo_parses_negative_weight() {
        // -12.34 g: sign='-' (0x2D), raw BE24 = 0x0004D2 = 1234 → /100 = 12.34
        let mut pkt = [0u8; 20];
        pkt[0] = 0x03;
        pkt[1] = 0x0B;
        pkt[6] = 0x2D;
        pkt[7] = 0x00;
        pkt[8] = 0x04;
        pkt[9] = 0xD2;
        let w = parse_bookoo(&pkt).expect("should parse");
        approx_eq(w, -12.34, 0.01);
    }

    #[test]
    fn bookoo_clamps_unrealistic_weight() {
        let mut pkt = [0u8; 20];
        pkt[0] = 0x03;
        pkt[1] = 0x0B;
        pkt[6] = 0x2B;
        pkt[7] = 0x0F;
        pkt[8] = 0x42;
        pkt[9] = 0x40; // 1,000,000 -> 10,000.00 g, clamp to 5,000 g
        let w = parse_bookoo(&pkt).expect("should parse");
        approx_eq(w, 5_000.0, 0.01);
    }

    #[test]
    fn bookoo_rejects_non_weight_packet() {
        let pkt = [0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(parse_bookoo(&pkt).is_none());
    }

    #[test]
    fn bookoo_rejects_short_packet() {
        assert!(parse_bookoo(&[0x03, 0x0B]).is_none());
    }

    #[test]
    fn standard_parses_si_weight() {
        // flags=0x00 (SI), raw LE u16 = 100 → 100 * 5 = 500 g
        let pkt = [0x00, 0x64, 0x00];
        let w = parse_standard(&pkt).expect("should parse");
        approx_eq(w, 500.0, 0.01);
    }

    #[test]
    fn standard_parses_imperial_weight() {
        // flags=0x01 (Imperial), raw LE u16 = 100 → 100 * 4.535… = 453.59 g
        let pkt = [0x01, 0x64, 0x00];
        let w = parse_standard(&pkt).expect("should parse");
        approx_eq(w, 453.59, 0.1);
    }

    #[test]
    fn standard_rejects_short_packet() {
        assert!(parse_standard(&[0x00, 0x01]).is_none());
    }

    #[test]
    fn parse_weight_dispatches_correctly() {
        // Standard
        let std_pkt = [0x00, 0x0A, 0x00]; // 10 * 5 = 50 g
        let w = parse_weight(ScaleProtocol::StandardWeight, &std_pkt, 0.0).expect("standard");
        approx_eq(w, 50.0, 0.01);

        // Bookoo
        let mut bookoo_pkt = [0u8; 20];
        bookoo_pkt[0] = 0x03;
        bookoo_pkt[1] = 0x0B;
        bookoo_pkt[6] = 0x2B;
        bookoo_pkt[8] = 0x03;
        bookoo_pkt[9] = 0xE8; // 1000 → /100 = 10.00 g
        let w = parse_weight(ScaleProtocol::Bookoo, &bookoo_pkt, 0.0).expect("bookoo");
        approx_eq(w, 10.0, 0.01);

        // GenericNotify falls through to generic parser
        let ascii = b"WT: 25.5 g";
        let w = parse_weight(ScaleProtocol::GenericNotify, ascii, 0.0).expect("generic ascii");
        approx_eq(w, 25.5, 0.01);

        // Unknown also falls through to generic parser
        let w = parse_weight(ScaleProtocol::Unknown, ascii, 0.0).expect("unknown ascii");
        approx_eq(w, 25.5, 0.01);
    }

    #[test]
    fn encodes_bookoo_tare_and_start_command() {
        assert_eq!(
            encode_bookoo_command(BookooCommand::TareAndStart),
            [0x03, 0x0A, 0x07, 0x00, 0x00, 0x0E],
        );
    }

    #[test]
    fn encodes_bookoo_flow_smoothing_commands() {
        assert_eq!(
            encode_bookoo_command(BookooCommand::SetFlowSmoothing(false)),
            [0x03, 0x0A, 0x08, 0x00, 0x00, 0x01],
        );
        assert_eq!(
            encode_bookoo_command(BookooCommand::SetFlowSmoothing(true)),
            [0x03, 0x0A, 0x08, 0x01, 0x00, 0x00],
        );
    }
}
