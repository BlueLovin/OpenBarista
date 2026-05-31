use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::telemetry_feed::TelemetrySnapshot;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum pressure (bar) to trigger shot start.
pub const SHOT_START_PRESSURE_BAR: f32 = 0.5;

/// Minimum temperature (°C) to trigger shot start.
/// Below this the machine is too cold to be pulling espresso.
pub const SHOT_START_TEMPERATURE_C: f32 = 70.0;

/// Number of 50 ms ticks below threshold before a shot is finalised.
/// 40 ticks × 50 ms = 2 s debounce.
const SHOT_END_DEBOUNCE_TICKS: u32 = 40;

/// Length of the pre-shot ring buffer (50 ms ticks).
/// 60 ticks × 50 ms = 3 s of pre-shot history.
const PRE_SHOT_BUFFER_LEN: usize = 60;

/// Record one data point every N ticks. 5 × 50 ms = 250 ms per point.
const RECORD_INTERVAL_TICKS: u32 = 5;

/// Maximum number of points stored per shot. 200 × 250 ms = 50 s.
pub const MAX_SHOT_POINTS: usize = 200;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotPoint {
    /// Milliseconds since shot start (including pre-shot).
    pub time_ms: u32,
    pub pressure_bar: f32,
    pub temperature_c: f32,
    pub weight_g: f32,
    pub flow_gps: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotRecord {
    pub id: u32,
    /// Unix timestamp (seconds). 0 when NTP has not synced yet.
    pub unix_timestamp: u64,
    pub points: Vec<ShotPoint>,
}

impl ShotRecord {
    pub fn to_summary(&self) -> ShotSummary {
        let duration_ms = self.points.last().map(|p| p.time_ms).unwrap_or(0);

        let pressures: Vec<f32> = self
            .points
            .iter()
            .map(|p| p.pressure_bar)
            .filter(|v| v.is_finite())
            .collect();
        let max_pressure_bar = pressures
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let max_pressure_bar = if max_pressure_bar.is_finite() {
            max_pressure_bar
        } else {
            0.0
        };

        let temps: Vec<f32> = self
            .points
            .iter()
            .map(|p| p.temperature_c)
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        let avg_temperature_c = if temps.is_empty() {
            0.0
        } else {
            temps.iter().sum::<f32>() / temps.len() as f32
        };

        // Yield = last weight − first weight (delta from when shot started).
        let first_weight = self.points.first().map(|p| p.weight_g).unwrap_or(0.0);
        let last_weight = self.points.last().map(|p| p.weight_g).unwrap_or(0.0);
        let yield_g = (last_weight - first_weight).max(0.0);

        let flows: Vec<f32> = self
            .points
            .iter()
            .map(|p| p.flow_gps)
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        let avg_flow_gps = if flows.is_empty() {
            0.0
        } else {
            flows.iter().sum::<f32>() / flows.len() as f32
        };

        ShotSummary {
            id: self.id,
            unix_timestamp: self.unix_timestamp,
            duration_ms,
            max_pressure_bar,
            avg_temperature_c,
            yield_g,
            avg_flow_gps,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotSummary {
    pub id: u32,
    pub unix_timestamp: u64,
    pub duration_ms: u32,
    pub max_pressure_bar: f32,
    pub avg_temperature_c: f32,
    pub yield_g: f32,
    pub avg_flow_gps: f32,
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

enum RecorderState {
    Idle,
    /// Entered after a manual `finalize()` while the signal is still above the
    /// auto-start threshold.  Auto-detection is suppressed until pressure/
    /// temperature drop below the threshold, preventing an immediate re-start
    /// of a new shot when the user presses Stop mid-extraction.
    Cooldown,
    Recording {
        points: Vec<ShotPoint>,
        tick_count: u32,
        record_ticker: u32,
        start_unix_ts: u64,
        shot_id: u32,
    },
    Debouncing {
        points: Vec<ShotPoint>,
        tick_count: u32,
        record_ticker: u32,
        debounce_ticks: u32,
        start_unix_ts: u64,
        shot_id: u32,
    },
}

// ---------------------------------------------------------------------------
// ShotRecorder
// ---------------------------------------------------------------------------

pub struct ShotRecorder {
    pre_buf: Vec<TelemetrySnapshot>,
    pre_buf_pos: usize,
    pre_buf_filled: usize,
    state: RecorderState,
    next_id: u32,
}

pub type SharedShotRecorder = Arc<Mutex<ShotRecorder>>;

impl ShotRecorder {
    pub fn new() -> Self {
        // Initialise with a zeroed-out snapshot so the ring buffer is always
        // fully addressable without needing Option.
        let empty = TelemetrySnapshot {
            seq: 0,
            temperature_c: 0.0,
            pressure_bar: 0.0,
            pressure_psi: 0.0,
            scale_connected: false,
            weight_g: 0.0,
            flow_gps: 0.0,
            recording_active: false,
        };
        Self {
            pre_buf: vec![empty; PRE_SHOT_BUFFER_LEN],
            pre_buf_pos: 0,
            pre_buf_filled: 0,
            state: RecorderState::Idle,
            next_id: 1,
        }
    }

    /// Called from the main loop every 50 ms.
    ///
    /// Returns `Some(ShotRecord)` when a shot has been fully captured and is
    /// ready to be saved. Returns `None` in all other cases.
    pub fn update(&mut self, snap: &TelemetrySnapshot, unix_timestamp: u64) -> Option<ShotRecord> {
        // Always push into the pre-shot ring buffer.
        self.pre_buf[self.pre_buf_pos] = *snap;
        self.pre_buf_pos = (self.pre_buf_pos + 1) % PRE_SHOT_BUFFER_LEN;
        if self.pre_buf_filled < PRE_SHOT_BUFFER_LEN {
            self.pre_buf_filled += 1;
        }

        let above_threshold = snap.pressure_bar >= SHOT_START_PRESSURE_BAR
            && snap.temperature_c >= SHOT_START_TEMPERATURE_C;

        // Use a temporary replacement to satisfy the borrow checker when
        // transitioning states.
        let current_state = std::mem::replace(&mut self.state, RecorderState::Idle);

        match current_state {
            RecorderState::Idle => {
                if above_threshold {
                    let id = self.next_id;
                    self.next_id = self.next_id.wrapping_add(1);
                    let mut points = self.drain_pre_buf();
                    // Compute the live-point timestamp before pushing so we can
                    // seed tick_count from it — ensuring subsequent recording
                    // points continue monotonically after the pre-shot buffer.
                    let live_t_ms = points
                        .last()
                        .map(|p: &ShotPoint| p.time_ms + (RECORD_INTERVAL_TICKS * 50))
                        .unwrap_or(0);
                    if points.len() < MAX_SHOT_POINTS {
                        points.push(snap_to_point(snap, live_t_ms));
                    }
                    self.state = RecorderState::Recording {
                        points,
                        tick_count: live_t_ms / 50,
                        record_ticker: 0,
                        start_unix_ts: unix_timestamp,
                        shot_id: id,
                    };
                } else {
                    self.state = RecorderState::Idle;
                }
                None
            }

            // Suppress auto-detection until signal drops below threshold.
            RecorderState::Cooldown => {
                if !above_threshold {
                    self.state = RecorderState::Idle;
                } else {
                    self.state = RecorderState::Cooldown;
                }
                None
            }

            RecorderState::Recording {
                mut points,
                tick_count,
                mut record_ticker,
                start_unix_ts,
                shot_id,
            } => {
                let new_tick = tick_count + 1;
                record_ticker += 1;

                if record_ticker >= RECORD_INTERVAL_TICKS {
                    record_ticker = 0;
                    if points.len() < MAX_SHOT_POINTS {
                        let t_ms = new_tick * 50;
                        points.push(snap_to_point(snap, t_ms));
                    }
                }

                if !above_threshold {
                    self.state = RecorderState::Debouncing {
                        points,
                        tick_count: new_tick,
                        record_ticker,
                        debounce_ticks: 1,
                        start_unix_ts,
                        shot_id,
                    };
                } else {
                    self.state = RecorderState::Recording {
                        points,
                        tick_count: new_tick,
                        record_ticker,
                        start_unix_ts,
                        shot_id,
                    };
                }
                None
            }

            RecorderState::Debouncing {
                mut points,
                tick_count,
                mut record_ticker,
                debounce_ticks,
                start_unix_ts,
                shot_id,
            } => {
                let new_tick = tick_count + 1;
                record_ticker += 1;

                if record_ticker >= RECORD_INTERVAL_TICKS {
                    record_ticker = 0;
                    if points.len() < MAX_SHOT_POINTS {
                        let t_ms = new_tick * 50;
                        points.push(snap_to_point(snap, t_ms));
                    }
                }

                if above_threshold {
                    // Pressure came back — continue recording.
                    self.state = RecorderState::Recording {
                        points,
                        tick_count: new_tick,
                        record_ticker,
                        start_unix_ts,
                        shot_id,
                    };
                    None
                } else if debounce_ticks + 1 >= SHOT_END_DEBOUNCE_TICKS {
                    // Debounce expired — finalise the shot.
                    self.state = RecorderState::Idle;
                    Some(ShotRecord {
                        id: shot_id,
                        unix_timestamp: start_unix_ts,
                        points,
                    })
                } else {
                    self.state = RecorderState::Debouncing {
                        points,
                        tick_count: new_tick,
                        record_ticker,
                        debounce_ticks: debounce_ticks + 1,
                        start_unix_ts,
                        shot_id,
                    };
                    None
                }
            }
        }
    }

    /// Immediately finalises whatever is currently being recorded and returns
    /// it. Used when the user manually stops a shot from the UI.
    ///
    /// Returns `None` if there is nothing in progress.
    pub fn finalize(&mut self) -> Option<ShotRecord> {
        // Replace with Cooldown to suppress auto-restart if signal is still
        // above threshold when the user manually stops.  Corrected to Idle
        // below if nothing was actually recording.
        let current = std::mem::replace(&mut self.state, RecorderState::Cooldown);
        match current {
            RecorderState::Recording {
                points,
                start_unix_ts,
                shot_id,
                ..
            }
            | RecorderState::Debouncing {
                points,
                start_unix_ts,
                shot_id,
                ..
            } => {
                if points.is_empty() {
                    self.state = RecorderState::Idle;
                    None
                } else {
                    Some(ShotRecord {
                        id: shot_id,
                        unix_timestamp: start_unix_ts,
                        points,
                    })
                }
            }
            // Nothing was recording — no need for cooldown.
            RecorderState::Idle | RecorderState::Cooldown => {
                self.state = RecorderState::Idle;
                None
            }
        }
    }

    /// Returns `true` if a shot is currently being recorded or debouncing.
    pub fn is_active(&self) -> bool {
        // Cooldown is intentionally excluded: the shot has been finalised and
        // the UI should show idle even while auto-detection is suppressed.
        matches!(
            self.state,
            RecorderState::Recording { .. } | RecorderState::Debouncing { .. }
        )
    }

    /// Manually starts recording regardless of current pressure.
    ///
    /// If recording is already in progress this is a no-op (the existing shot
    /// continues). Use [`finalize`] to stop it.
    pub fn force_start(&mut self, unix_timestamp: u64) {
        if self.is_active() {
            return;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let points = self.drain_pre_buf();
        // Seed tick_count from the last pre-shot point so subsequent samples
        // continue monotonically — same logic as auto-start in the Idle branch.
        let initial_tick = points.last().map(|p| p.time_ms / 50).unwrap_or(0);
        self.state = RecorderState::Recording {
            points,
            tick_count: initial_tick,
            record_ticker: 0,
            start_unix_ts: unix_timestamp,
            shot_id: id,
        };
    }

    // Drain the pre-shot ring buffer into a Vec of ShotPoints sampled at
    // RECORD_INTERVAL_TICKS (every 5 × 50ms = 250ms).
    fn drain_pre_buf(&self) -> Vec<ShotPoint> {
        let filled = self.pre_buf_filled;
        if filled == 0 {
            return Vec::new();
        }

        // Walk the ring oldest-first.
        let mut out = Vec::new();
        let start = if filled < PRE_SHOT_BUFFER_LEN {
            0
        } else {
            self.pre_buf_pos // oldest slot
        };

        for i in 0..filled {
            // Only keep every RECORD_INTERVAL_TICKS-th entry.
            if i % RECORD_INTERVAL_TICKS as usize != 0 {
                continue;
            }
            let idx = (start + i) % PRE_SHOT_BUFFER_LEN;
            let snap = &self.pre_buf[idx];
            // Time is negative offset from shot start (in ms).
            // i=0 is the oldest, filled-1 is the newest (= ~0ms before shot).
            let offset_from_end = filled - 1 - i;
            let t_ms_neg = offset_from_end as u32 * 50;
            // We'll correct times to be 0-based after collecting.
            out.push(ShotPoint {
                time_ms: t_ms_neg, // placeholder; will subtract from max below
                pressure_bar: snap.pressure_bar,
                temperature_c: snap.temperature_c,
                weight_g: snap.weight_g,
                flow_gps: snap.flow_gps,
            });
        }

        // Convert placeholder times: max value → 0, others are subtracted.
        if let Some(max_t) = out.iter().map(|p| p.time_ms).max() {
            for p in &mut out {
                p.time_ms = max_t - p.time_ms;
            }
        }

        out
    }
}

impl Default for ShotRecorder {
    fn default() -> Self {
        Self::new()
    }
}

fn snap_to_point(snap: &TelemetrySnapshot, time_ms: u32) -> ShotPoint {
    ShotPoint {
        time_ms,
        pressure_bar: snap.pressure_bar,
        temperature_c: snap.temperature_c,
        weight_g: snap.weight_g,
        flow_gps: snap.flow_gps,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snap(pressure_bar: f32) -> TelemetrySnapshot {
        make_snap_at_temp(pressure_bar, 93.0)
    }

    fn make_snap_at_temp(pressure_bar: f32, temperature_c: f32) -> TelemetrySnapshot {
        TelemetrySnapshot {
            seq: 0,
            temperature_c,
            pressure_bar,
            pressure_psi: pressure_bar * 14.5,
            scale_connected: false,
            weight_g: 0.0,
            flow_gps: 0.0,
            recording_active: false,
        }
    }

    #[test]
    fn cold_machine_does_not_start_shot() {
        let mut rec = ShotRecorder::new();
        // Pressure is above threshold but temperature is below 70 °C.
        for _ in 0..100 {
            let result = rec.update(&make_snap_at_temp(8.0, 45.0), 0);
            assert!(result.is_none());
        }
        assert!(!rec.is_active());
    }

    #[test]
    fn warm_machine_starts_shot() {
        let mut rec = ShotRecorder::new();
        rec.update(&make_snap_at_temp(8.0, 69.9), 0);
        assert!(!rec.is_active(), "should not start below 70 °C");
        rec.update(&make_snap_at_temp(8.0, 70.0), 0);
        assert!(rec.is_active(), "should start at exactly 70 °C");
    }


    #[test]
    fn idle_below_threshold_stays_idle() {
        let mut rec = ShotRecorder::new();
        for _ in 0..100 {
            let result = rec.update(&make_snap(0.2), 0);
            assert!(result.is_none());
        }
        assert!(!rec.is_active());
    }

    #[test]
    fn crossing_threshold_starts_recording() {
        let mut rec = ShotRecorder::new();
        rec.update(&make_snap(0.2), 0);
        rec.update(&make_snap(0.6), 0);
        assert!(rec.is_active());
    }

    #[test]
    fn debounce_finalises_shot() {
        let mut rec = ShotRecorder::new();
        // Start shot.
        for _ in 0..20 {
            rec.update(&make_snap(8.0), 1000);
        }
        // Drop below threshold — shot should not finalise immediately.
        for i in 0..39 {
            let result = rec.update(&make_snap(0.1), 1000);
            assert!(result.is_none(), "premature finalise at debounce tick {i}");
        }
        // 40th tick below threshold → finalised.
        let result = rec.update(&make_snap(0.1), 1000);
        assert!(result.is_some());
        let shot = result.unwrap();
        assert!(shot.id > 0);
        assert!(!shot.points.is_empty());
        assert_eq!(shot.unix_timestamp, 1000);
    }

    #[test]
    fn debounce_resets_on_pressure_return() {
        let mut rec = ShotRecorder::new();
        for _ in 0..10 {
            rec.update(&make_snap(8.0), 0);
        }
        // 30 ticks below (not enough to debounce).
        for _ in 0..30 {
            rec.update(&make_snap(0.0), 0);
        }
        // Pressure comes back.
        rec.update(&make_snap(7.0), 0);
        assert!(rec.is_active());
        // Now fully debounce.
        for _ in 0..SHOT_END_DEBOUNCE_TICKS {
            rec.update(&make_snap(0.0), 0);
        }
        // Should have finalised now (one extra tick to tip over).
        let mut got_shot = false;
        for _ in 0..5 {
            if rec.update(&make_snap(0.0), 0).is_some() {
                got_shot = true;
                break;
            }
        }
        // After full debounce it should already have fired, check state.
        assert!(!rec.is_active() || got_shot);
    }

    #[test]
    fn pre_shot_buffer_prepended() {
        let mut rec = ShotRecorder::new();
        // Fill pre-buf with low pressure.
        for _ in 0..PRE_SHOT_BUFFER_LEN {
            rec.update(&make_snap(0.1), 0);
        }
        // Trigger shot.
        rec.update(&make_snap(1.0), 1234);
        assert!(rec.is_active());
        // Finalize immediately.
        let shot = rec.finalize().unwrap();
        // Should have pre-shot points.
        assert!(shot.points.len() > 1, "expected pre-shot points prepended");
    }

    #[test]
    fn finalize_returns_none_when_idle() {
        let mut rec = ShotRecorder::new();
        assert!(rec.finalize().is_none());
    }

    #[test]
    fn finalize_returns_shot_mid_recording() {
        let mut rec = ShotRecorder::new();
        for _ in 0..10 {
            rec.update(&make_snap(8.0), 999);
        }
        let shot = rec.finalize();
        assert!(shot.is_some());
        assert!(!rec.is_active());
    }
}
