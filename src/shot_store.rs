use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use crate::shot_recorder::{ShotRecord, ShotSummary, MAX_SHOT_POINTS};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over shot persistence. Swap `NvsShotStore` for an `ApiShotStore`
/// (or any other backend) by replacing the concrete type passed to
/// `setup_wifi_station`.
pub trait ShotStore: Send {
    fn save(&mut self, shot: ShotRecord) -> Result<u32>;
    fn list_summaries(&self) -> Result<Vec<ShotSummary>>;
    fn get_shot(&self, id: u32) -> Result<Option<ShotRecord>>;
    fn delete_shot(&mut self, id: u32) -> Result<bool>;
}

pub type SharedShotStore = Arc<Mutex<dyn ShotStore + Send>>;

// ---------------------------------------------------------------------------
// Binary encoding helpers
// ---------------------------------------------------------------------------
//
// Layout (all little-endian):
//   Header – 20 bytes
//     id          : u32   (4)
//     unix_ts     : u64   (8)
//     point_count : u16   (2)
//     _pad        : u16   (2)
//     _pad        : u32   (4)
//   Per point – 10 bytes each
//     time_ms     : u16   (2)   – seconds × 1000, capped at 65535
//     pressure    : u16   (2)   – bar × 100,      range 0–655
//     temperature : u16   (2)   – °C × 10,        range 0–6553
//     weight      : i16   (2)   – g × 10,         range ±3276
//     flow        : u16   (2)   – g/s × 100,      range 0–655
//
// Max bytes: 20 + 200×10 = 2020 bytes per shot.
// 10 shots ≈ 20 KB of payload, plus NVS page headers and existing namespaces.
// The default NVS partition (24 KB) may be tight; a custom partition table is
// recommended for production to give shots their own namespace space.

const HEADER_LEN: usize = 20;
const POINT_LEN: usize = 10;

fn encode_shot(shot: &ShotRecord) -> Vec<u8> {
    let n = shot.points.len().min(MAX_SHOT_POINTS);
    let mut buf = vec![0u8; HEADER_LEN + n * POINT_LEN];

    buf[0..4].copy_from_slice(&shot.id.to_le_bytes());
    buf[4..12].copy_from_slice(&shot.unix_timestamp.to_le_bytes());
    buf[12..14].copy_from_slice(&(n as u16).to_le_bytes());
    // bytes 14-19 are padding, remain 0.

    for (i, p) in shot.points.iter().take(n).enumerate() {
        let off = HEADER_LEN + i * POINT_LEN;

        let time_ms = p.time_ms.min(u16::MAX as u32) as u16;
        let pressure = ((p.pressure_bar * 100.0).round().clamp(0.0, 65535.0)) as u16;
        let temperature = ((p.temperature_c * 10.0).round().clamp(0.0, 65535.0)) as u16;
        let weight = ((p.weight_g * 10.0).round().clamp(i16::MIN as f32, i16::MAX as f32)) as i16;
        let flow = ((p.flow_gps * 100.0).round().clamp(0.0, 65535.0)) as u16;

        buf[off..off + 2].copy_from_slice(&time_ms.to_le_bytes());
        buf[off + 2..off + 4].copy_from_slice(&pressure.to_le_bytes());
        buf[off + 4..off + 6].copy_from_slice(&temperature.to_le_bytes());
        buf[off + 6..off + 8].copy_from_slice(&weight.to_le_bytes());
        buf[off + 8..off + 10].copy_from_slice(&flow.to_le_bytes());
    }

    buf
}

fn decode_shot(buf: &[u8]) -> Option<ShotRecord> {
    if buf.len() < HEADER_LEN {
        return None;
    }

    let id = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    if id == 0 {
        return None; // deleted / uninitialised slot
    }
    let unix_timestamp = u64::from_le_bytes(buf[4..12].try_into().ok()?);
    let point_count = u16::from_le_bytes(buf[12..14].try_into().ok()?) as usize;

    if point_count > MAX_SHOT_POINTS || buf.len() < HEADER_LEN + point_count * POINT_LEN {
        return None;
    }

    let mut points = Vec::with_capacity(point_count);
    for i in 0..point_count {
        let off = HEADER_LEN + i * POINT_LEN;
        let time_ms = u16::from_le_bytes(buf[off..off + 2].try_into().ok()?) as u32;
        let pressure_bar =
            u16::from_le_bytes(buf[off + 2..off + 4].try_into().ok()?) as f32 / 100.0;
        let temperature_c =
            u16::from_le_bytes(buf[off + 4..off + 6].try_into().ok()?) as f32 / 10.0;
        let weight_g =
            i16::from_le_bytes(buf[off + 6..off + 8].try_into().ok()?) as f32 / 10.0;
        let flow_gps =
            u16::from_le_bytes(buf[off + 8..off + 10].try_into().ok()?) as f32 / 100.0;

        points.push(crate::shot_recorder::ShotPoint {
            time_ms,
            pressure_bar,
            temperature_c,
            weight_g,
            flow_gps,
        });
    }

    Some(ShotRecord {
        id,
        unix_timestamp,
        points,
    })
}

// ---------------------------------------------------------------------------
// NvsShotStore  (ESP-IDF / xtensa target only)
// ---------------------------------------------------------------------------

/// Maximum number of shots stored in NVS. Oldest is overwritten when full.
pub const MAX_STORED_SHOTS: usize = 10;

#[cfg(target_arch = "xtensa")]
const SHOTS_NAMESPACE: &str = "shots";
#[cfg(target_arch = "xtensa")]
const KEY_HEAD: &str = "head";
#[cfg(target_arch = "xtensa")]
const KEY_COUNT: &str = "count";
#[cfg(target_arch = "xtensa")]
const KEY_NEXT_ID: &str = "next_id";

#[cfg(target_arch = "xtensa")]
fn slot_key(slot: usize) -> &'static str {
    // NVS keys must be ≤ 15 chars. "s0"–"s9" are all fine.
    const KEYS: [&str; 10] = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9"];
    KEYS[slot]
}

/// Shot store backed by ESP-IDF NVS blobs.
#[cfg(target_arch = "xtensa")]
pub struct NvsShotStore {
    nvs_partition: esp_idf_svc::nvs::EspDefaultNvsPartition,
    /// Index of the next write slot (0–9).
    head: usize,
    /// Number of occupied slots (0–10).
    count: usize,
    /// Next shot ID to assign.
    next_id: u32,
}

#[cfg(target_arch = "xtensa")]
impl NvsShotStore {
    pub fn new(nvs_partition: esp_idf_svc::nvs::EspDefaultNvsPartition) -> Result<Self> {
        use esp_idf_svc::nvs::EspNvs;

        let nvs = EspNvs::new(nvs_partition.clone(), SHOTS_NAMESPACE, true)?;

        let mut head_buf = [0u8; 4];
        let head = nvs
            .get_blob(KEY_HEAD, &mut head_buf)?
            .map(|b| u8::from_le_bytes(b.try_into().unwrap_or([0])) as usize)
            .unwrap_or(0)
            .min(MAX_STORED_SHOTS - 1);

        let mut count_buf = [0u8; 4];
        let count = nvs
            .get_blob(KEY_COUNT, &mut count_buf)?
            .map(|b| u8::from_le_bytes(b.try_into().unwrap_or([0])) as usize)
            .unwrap_or(0)
            .min(MAX_STORED_SHOTS);

        let mut next_id_buf = [0u8; 4];
        let next_id = nvs
            .get_blob(KEY_NEXT_ID, &mut next_id_buf)?
            .map(|b| u32::from_le_bytes(b.try_into().unwrap_or([0, 0, 0, 1])))
            .unwrap_or(1)
            .max(1);

        println!(
            "[shots] NvsShotStore loaded: head={head}, count={count}, next_id={next_id}"
        );

        Ok(Self {
            nvs_partition,
            head,
            count,
            next_id,
        })
    }

    fn open_nvs(&self) -> Result<esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>> {
        Ok(esp_idf_svc::nvs::EspNvs::new(
            self.nvs_partition.clone(),
            SHOTS_NAMESPACE,
            true,
        )?)
    }

    fn save_metadata(&self) -> Result<()> {
        let nvs = self.open_nvs()?;
        nvs.set_blob(KEY_HEAD, &[self.head as u8])?;
        nvs.set_blob(KEY_COUNT, &[self.count as u8])?;
        nvs.set_blob(KEY_NEXT_ID, &self.next_id.to_le_bytes())?;
        Ok(())
    }
}

#[cfg(target_arch = "xtensa")]
impl ShotStore for NvsShotStore {
    fn save(&mut self, mut shot: ShotRecord) -> Result<u32> {
        // Assign a stable ID from the store's sequence, so IDs remain unique
        // even after the recorder resets its counter on reboot.
        let assigned_id = self.next_id;
        shot.id = assigned_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);

        let blob = encode_shot(&shot);
        let nvs = self.open_nvs()?;
        nvs.set_blob(slot_key(self.head), &blob)?;

        self.head = (self.head + 1) % MAX_STORED_SHOTS;
        if self.count < MAX_STORED_SHOTS {
            self.count += 1;
        }
        self.save_metadata()?;

        println!(
            "[shots] Saved shot id={} ({} points) to slot {}",
            assigned_id,
            shot.points.len(),
            (self.head + MAX_STORED_SHOTS - 1) % MAX_STORED_SHOTS
        );
        Ok(assigned_id)
    }

    fn list_summaries(&self) -> Result<Vec<ShotSummary>> {
        if self.count == 0 {
            return Ok(Vec::new());
        }

        let nvs = self.open_nvs()?;
        // Max blob size.
        const MAX_BLOB: usize = HEADER_LEN + MAX_SHOT_POINTS * POINT_LEN;
        let mut buf = vec![0u8; MAX_BLOB];
        let mut summaries = Vec::with_capacity(self.count);

        // Iterate slots oldest-first so summaries come out in ascending id order.
        // Oldest slot = (head - count + MAX) % MAX.
        for i in 0..self.count {
            let slot = (self.head + MAX_STORED_SHOTS - self.count + i) % MAX_STORED_SHOTS;
            if let Ok(Some(data)) = nvs.get_blob(slot_key(slot), &mut buf) {
                if let Some(shot) = decode_shot(data) {
                    summaries.push(shot.to_summary());
                }
            }
        }

        // Sort newest first.
        summaries.sort_unstable_by(|a, b| b.id.cmp(&a.id));
        Ok(summaries)
    }

    fn get_shot(&self, id: u32) -> Result<Option<ShotRecord>> {
        let nvs = self.open_nvs()?;
        const MAX_BLOB: usize = HEADER_LEN + MAX_SHOT_POINTS * POINT_LEN;
        let mut buf = vec![0u8; MAX_BLOB];

        for i in 0..self.count {
            let slot = (self.head + MAX_STORED_SHOTS - self.count + i) % MAX_STORED_SHOTS;
            if let Ok(Some(data)) = nvs.get_blob(slot_key(slot), &mut buf) {
                if let Some(shot) = decode_shot(data) {
                    if shot.id == id {
                        return Ok(Some(shot));
                    }
                }
            }
        }
        Ok(None)
    }

    fn delete_shot(&mut self, id: u32) -> Result<bool> {
        let nvs = self.open_nvs()?;
        const MAX_BLOB: usize = HEADER_LEN + MAX_SHOT_POINTS * POINT_LEN;
        let mut buf = vec![0u8; MAX_BLOB];

        for i in 0..self.count {
            let slot = (self.head + MAX_STORED_SHOTS - self.count + i) % MAX_STORED_SHOTS;
            if let Ok(Some(data)) = nvs.get_blob(slot_key(slot), &mut buf) {
                if let Some(shot) = decode_shot(data) {
                    if shot.id == id {
                        // Zero out the slot (id=0 signals deleted in decode_shot).
                        // Do NOT decrement count: the zeroed slot is simply skipped
                        // during iteration via the id==0 sentinel.  Decrementing
                        // would shift the ring's logical start and permanently hide
                        // the oldest occupied slot.
                        nvs.set_blob(slot_key(slot), &[0u8; HEADER_LEN])?;
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}


// ---------------------------------------------------------------------------
// Tests (host-side, no ESP-IDF required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shot_recorder::ShotPoint;

    fn make_shot(id: u32, n_points: usize) -> ShotRecord {
        ShotRecord {
            id,
            unix_timestamp: 1_700_000_000 + id as u64,
            points: (0..n_points)
                .map(|i| ShotPoint {
                    time_ms: i as u32 * 250,
                    pressure_bar: 8.5 + (i as f32 * 0.01),
                    temperature_c: 93.0,
                    weight_g: i as f32 * 0.5,
                    flow_gps: 1.2,
                })
                .collect(),
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let shot = make_shot(42, 80);
        let buf = encode_shot(&shot);
        let decoded = decode_shot(&buf).expect("decode failed");

        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.unix_timestamp, shot.unix_timestamp);
        assert_eq!(decoded.points.len(), 80);

        let p0 = &decoded.points[0];
        assert!((p0.pressure_bar - 8.5).abs() < 0.02);
        assert!((p0.temperature_c - 93.0).abs() < 0.2);
    }

    #[test]
    fn decode_returns_none_for_short_buf() {
        assert!(decode_shot(&[0u8; 5]).is_none());
    }

    #[test]
    fn decode_returns_none_for_zero_id() {
        assert!(decode_shot(&[0u8; HEADER_LEN]).is_none());
    }

    #[test]
    fn encode_caps_at_max_shot_points() {
        let shot = make_shot(1, MAX_SHOT_POINTS + 50);
        let buf = encode_shot(&shot);
        let decoded = decode_shot(&buf).unwrap();
        assert_eq!(decoded.points.len(), MAX_SHOT_POINTS);
    }

    #[test]
    fn summary_analytics() {
        let shot = make_shot(5, 40);
        let summary = shot.to_summary();
        assert_eq!(summary.id, 5);
        assert!(summary.max_pressure_bar > 8.0);
        assert!((summary.avg_temperature_c - 93.0).abs() < 0.1);
        // yield = last - first weight = 39*0.5 - 0 = 19.5
        assert!((summary.yield_g - 19.5).abs() < 0.2);
    }
}
