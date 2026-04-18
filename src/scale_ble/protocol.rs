use openbarista::telemetry_math::sanitize_weight_g;

use super::types::ScaleProtocol;

pub(super) fn parse_weight_measurement(
    protocol: ScaleProtocol,
    value: &[u8],
    previous_weight_g: f32,
) -> Option<f32> {
    match protocol {
        ScaleProtocol::StandardWeight => parse_standard_weight_measurement(value),
        ScaleProtocol::Bookoo => parse_bookoo_weight(value),
        ScaleProtocol::GenericNotify | ScaleProtocol::Unknown => {
            parse_generic_weight_measurement(value, previous_weight_g)
        }
    }
}

/// Bookoo BOOKOO_SC_U: service 0x0FFE, char 0xFF11, 20-byte packets.
/// Header: 03 0B. Byte 6: sign (0x2B='+', 0x2D='-'). Bytes 7-9: weight
/// big-endian 24-bit int in 0.01 g units.
fn parse_bookoo_weight(value: &[u8]) -> Option<f32> {
    if value.len() < 10 {
        return None;
    }
    if value[0] != 0x03 || value[1] != 0x0B {
        return None;
    }
    let sign: f32 = if value[6] == 0x2D { -1.0 } else { 1.0 };
    let raw = ((value[7] as u32) << 16) | ((value[8] as u32) << 8) | (value[9] as u32);
    let weight_g = sign * (raw as f32) / 100.0;
    #[cfg(debug_assertions)]
    {
        println!(
            "[scale] bookoo: sign={} raw={} weight_g={:.1}",
            if sign < 0.0 { '-' } else { '+' },
            raw,
            weight_g
        );
    }
    Some(sanitize_weight_g(weight_g))
}

fn parse_standard_weight_measurement(value: &[u8]) -> Option<f32> {
    if value.len() < 3 {
        return None;
    }
    let flags = value[0];
    let weight_raw = u16::from_le_bytes([value[1], value[2]]) as f32;
    let weight_g = if flags & 0x01 == 0 {
        weight_raw * 5.0
    } else {
        weight_raw * 4.535_923_7
    };
    Some(sanitize_weight_g(weight_g))
}

fn parse_generic_weight_measurement(value: &[u8], previous_weight_g: f32) -> Option<f32> {
    if let Some(parsed) = parse_ascii_weight(value) {
        return Some(parsed);
    }

    let half = value.len() / 2;
    let mut best: Option<WeightCandidate> = None;
    let mut best_dist: f32 = f32::MAX;

    for start in 0..value.len() {
        let window = &value[start..];

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
        if !weight_g.is_finite() || !(0.0..=5000.0).contains(&weight_g) {
            continue;
        }
        if weight_g <= 0.0 {
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
            d += 500.0;
        }
        if candidate.in_trailer {
            d += 200.0;
        }
        d
    } else {
        if candidate.weight_g <= 0.0 {
            return 10_000.0;
        }
        let mut score = candidate.weight_g;
        if candidate.raw_abs < 10 {
            score += 5_000.0;
        }
        if candidate.in_trailer {
            score += 1_000.0;
        }
        score
    }
}

fn parse_ascii_weight(value: &[u8]) -> Option<f32> {
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

#[cfg(test)]
#[allow(unused_imports, dead_code)]
mod tests {
    use super::{
        candidate_distance, parse_ascii_weight, parse_generic_weight_measurement, WeightCandidate,
    };

    fn approx_eq(left: f32, right: f32, tolerance: f32) {
        assert!(
            (left - right).abs() <= tolerance,
            "left={left}, right={right}"
        );
    }

    #[test]
    fn generic_parser_reads_ascii_payloads() {
        let parsed = parse_ascii_weight(b"WT: 18.3 g").expect("ascii payload should parse");
        approx_eq(parsed, 18.3, 1e-6);
    }

    #[test]
    fn generic_parser_finds_weight_in_be_packet() {
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80, 0x2B, 0x00, 0x02, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w = parse_generic_weight_measurement(&pkt, 0.0).expect("should parse");
        approx_eq(w, 44.8, 0.5);
    }

    #[test]
    fn generic_parser_tracks_weight_change() {
        let pkt1: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x11, 0x80, 0x2B, 0x00, 0x02, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0x5D,
        ];
        let w1 = parse_generic_weight_measurement(&pkt1, 0.0).expect("should parse");
        approx_eq(w1, 44.8, 0.5);

        let pkt2: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00, 0x2B, 0x00, 0x00, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        let w2 = parse_generic_weight_measurement(&pkt2, w1);
        if let Some(w) = w2 {
            assert!(w < 10.0, "expected near-zero, got {w}");
        }
    }

    #[test]
    fn generic_parser_rejects_noise_from_trailer() {
        let pkt: [u8; 20] = [
            0x03, 0x0B, 0x00, 0x00, 0x00, 0x01, 0x2B, 0x00, 0x00, 0x00, 0x2B, 0x00, 0x00, 0x50,
            0x00, 0x96, 0x01, 0x00, 0x00, 0xCE,
        ];
        let w = parse_generic_weight_measurement(&pkt, 44.8);
        if let Some(weight) = w {
            assert!(weight < 50.0, "expected reasonable value, got {weight}");
        }
    }

    #[test]
    fn candidate_distance_prefers_positive_values_when_scale_is_unseeded() {
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
    fn candidate_distance_penalizes_low_raw_values() {
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
}
