use std::collections::VecDeque;

use crate::telemetry_math::sanitize_weight_g;

pub const BREW_SAMPLE_INTERVAL_MS: u32 = 50;
const HISTORY_CAPACITY: usize = 24;

#[derive(Debug, Clone, Copy)]
pub struct ShotSample {
    pub elapsed_ms: u32,
    pub temperature_c: f32,
    pub pressure_bar: f32,
    pub flow_gps: f32,
    pub weight_g: f32,
}

#[derive(Debug, Clone)]
pub struct ShotRecord {
    pub id: u64,
    pub started_at_epoch_ms: u64,
    pub ended_at_epoch_ms: u64,
    pub duration_ms: u32,
    pub manual: bool,
    pub peak_pressure_bar: f32,
    pub avg_pressure_bar: f32,
    pub avg_temperature_c: f32,
    pub yield_g: f32,
    pub samples: Vec<ShotSample>,
}

#[derive(Debug, Clone, Copy)]
pub struct LiveTelemetryPoint {
    pub sample_index: u64,
    pub temperature_c: f32,
    pub pressure_bar: f32,
    pub scale_connected: bool,
    pub weight_g: f32,
    pub flow_gps: f32,
}

#[derive(Debug, Clone)]
struct ActiveShot {
    started_at_epoch_ms: u64,
    started_sample_index: u64,
    initial_weight_g: f32,
    manual: bool,
    peak_pressure_bar: f32,
    pressure_sum: f64,
    temperature_sum: f64,
    sample_count: u32,
    samples: Vec<ShotSample>,
}

#[derive(Debug, Clone)]
pub struct ShotHistory {
    active: Option<ActiveShot>,
    completed: VecDeque<ShotRecord>,
    next_id: u64,
}

impl Default for ShotHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ShotHistory {
    pub fn new() -> Self {
        Self {
            active: None,
            completed: VecDeque::with_capacity(HISTORY_CAPACITY),
            next_id: 1,
        }
    }

    pub fn start_manual_shot(
        &mut self,
        started_at_epoch_ms: u64,
        telemetry: LiveTelemetryPoint,
    ) -> Result<(), &'static str> {
        if self.active.is_some() {
            return Err("Shot already in progress.");
        }

        let initial_weight_g = if telemetry.scale_connected {
            telemetry.weight_g
        } else {
            0.0
        };

        let mut active = ActiveShot {
            started_at_epoch_ms,
            started_sample_index: telemetry.sample_index,
            initial_weight_g,
            manual: true,
            peak_pressure_bar: sanitize_value(telemetry.pressure_bar).max(0.0),
            pressure_sum: 0.0,
            temperature_sum: 0.0,
            sample_count: 0,
            samples: Vec::new(),
        };
        active.push_sample(telemetry);
        self.active = Some(active);
        Ok(())
    }

    pub fn on_brew_sample(&mut self, telemetry: LiveTelemetryPoint) {
        if let Some(active) = self.active.as_mut() {
            active.push_sample(telemetry);
        }
    }

    pub fn stop_manual_shot(
        &mut self,
        ended_at_epoch_ms: u64,
        telemetry: LiveTelemetryPoint,
    ) -> Result<ShotRecord, &'static str> {
        let mut active = self.active.take().ok_or("No active shot.")?;

        let elapsed_ms = active.elapsed_ms_for(telemetry.sample_index);
        let needs_terminal_sample = active
            .samples
            .last()
            .map(|sample| sample.elapsed_ms != elapsed_ms)
            .unwrap_or(true);
        if needs_terminal_sample {
            active.push_sample(telemetry);
        }

        let sample_count = active.sample_count.max(1);
        let avg_pressure_bar = (active.pressure_sum / sample_count as f64) as f32;
        let avg_temperature_c = (active.temperature_sum / sample_count as f64) as f32;
        let final_weight_g = if telemetry.scale_connected {
            telemetry.weight_g
        } else {
            active.initial_weight_g
        };
        let shot = ShotRecord {
            id: self.next_id,
            started_at_epoch_ms: active.started_at_epoch_ms,
            ended_at_epoch_ms,
            duration_ms: elapsed_ms,
            manual: active.manual,
            peak_pressure_bar: active.peak_pressure_bar,
            avg_pressure_bar: sanitize_value(avg_pressure_bar),
            avg_temperature_c: sanitize_value(avg_temperature_c),
            yield_g: sanitize_weight_g((final_weight_g - active.initial_weight_g).max(0.0)),
            samples: active.samples,
        };

        self.next_id = self.next_id.wrapping_add(1);
        self.completed.push_front(shot.clone());
        while self.completed.len() > HISTORY_CAPACITY {
            self.completed.pop_back();
        }

        Ok(shot)
    }

    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    pub fn completed_shots(&self) -> Vec<ShotRecord> {
        self.completed.iter().cloned().collect()
    }
}

impl ActiveShot {
    fn elapsed_ms_for(&self, sample_index: u64) -> u32 {
        sample_index
            .saturating_sub(self.started_sample_index)
            .saturating_mul(BREW_SAMPLE_INTERVAL_MS as u64)
            .min(u32::MAX as u64) as u32
    }

    fn push_sample(&mut self, telemetry: LiveTelemetryPoint) {
        let temperature_c = sanitize_value(telemetry.temperature_c);
        let pressure_bar = sanitize_value(telemetry.pressure_bar).max(0.0);
        let flow_gps = if telemetry.scale_connected {
            sanitize_value(telemetry.flow_gps)
        } else {
            0.0
        };
        let weight_g = if telemetry.scale_connected {
            sanitize_weight_g((telemetry.weight_g - self.initial_weight_g).max(0.0))
        } else {
            0.0
        };

        self.samples.push(ShotSample {
            elapsed_ms: self.elapsed_ms_for(telemetry.sample_index),
            temperature_c,
            pressure_bar,
            flow_gps,
            weight_g,
        });
        self.sample_count = self.sample_count.saturating_add(1);
        self.peak_pressure_bar = self.peak_pressure_bar.max(pressure_bar);
        self.pressure_sum += pressure_bar as f64;
        self.temperature_sum += temperature_c as f64;
    }
}

fn sanitize_value(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveTelemetryPoint, ShotHistory, BREW_SAMPLE_INTERVAL_MS};

    fn point(sample_index: u64, pressure_bar: f32, weight_g: f32) -> LiveTelemetryPoint {
        LiveTelemetryPoint {
            sample_index,
            temperature_c: 93.0 + sample_index as f32,
            pressure_bar,
            scale_connected: true,
            weight_g,
            flow_gps: 2.0,
        }
    }

    #[test]
    fn manual_stop_always_saves_below_pressure_threshold() {
        let mut history = ShotHistory::new();
        history
            .start_manual_shot(1_000, point(10, 0.3, 20.0))
            .unwrap();
        history.on_brew_sample(point(11, 0.4, 20.5));
        let shot = history
            .stop_manual_shot(2_000, point(12, 0.2, 21.1))
            .unwrap();

        assert_eq!(shot.id, 1);
        assert!(shot.manual);
        assert!(shot.peak_pressure_bar < 1.0);
        assert_eq!(shot.duration_ms, 2 * BREW_SAMPLE_INTERVAL_MS);
        assert!(shot.yield_g > 1.0);
        assert_eq!(history.completed_shots().len(), 1);
    }

    #[test]
    fn duration_tracks_brew_sample_index_not_wall_clock() {
        let mut history = ShotHistory::new();
        history
            .start_manual_shot(1_000, point(100, 1.2, 10.0))
            .unwrap();
        history.on_brew_sample(point(130, 8.4, 24.0));

        let shot = history
            .stop_manual_shot(31_000, point(160, 8.8, 38.0))
            .unwrap();

        assert_eq!(shot.duration_ms, 60 * BREW_SAMPLE_INTERVAL_MS);
        assert!(shot.avg_pressure_bar > 1.0);
        assert_eq!(shot.samples.first().unwrap().elapsed_ms, 0);
        assert_eq!(shot.samples.last().unwrap().elapsed_ms, shot.duration_ms);
    }
}
