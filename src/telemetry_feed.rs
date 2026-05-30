use std::sync::{Arc, Mutex};

use crate::sync_utils::lock_or_recover;

#[derive(Debug, Clone, Copy)]
pub struct TelemetrySnapshot {
    pub seq: u64,
    pub temperature_c: f32,
    pub pressure_bar: f32,
    pub pressure_psi: f32,
    pub scale_connected: bool,
    pub weight_g: f32,
    pub flow_gps: f32,
    pub recording_active: bool,
}

#[derive(Debug, Clone)]
pub struct SharedTelemetry {
    inner: Arc<Mutex<TelemetrySnapshot>>,
}

impl SharedTelemetry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TelemetrySnapshot {
                seq: 0,
                temperature_c: 0.0,
                pressure_bar: 0.0,
                pressure_psi: 0.0,
                scale_connected: false,
                weight_g: 0.0,
                flow_gps: 0.0,
                recording_active: false,
            })),
        }
    }

    pub fn update(&self, temperature_c: f32, pressure_bar: f32, pressure_psi: f32) {
        self.update_brew(temperature_c, pressure_bar, pressure_psi);
    }

    pub fn update_brew(&self, temperature_c: f32, pressure_bar: f32, pressure_psi: f32) {
        let mut state = lock_or_recover(&self.inner);
        state.seq = state.seq.wrapping_add(1);
        state.temperature_c = temperature_c;
        state.pressure_bar = pressure_bar;
        state.pressure_psi = pressure_psi;
    }

    pub fn update_scale(&self, connected: bool, weight_g: f32, flow_gps: f32) {
        let mut state = lock_or_recover(&self.inner);
        state.seq = state.seq.wrapping_add(1);
        state.scale_connected = connected;
        state.weight_g = weight_g;
        state.flow_gps = flow_gps;
    }

    pub fn clear_scale(&self) {
        let mut state = lock_or_recover(&self.inner);
        state.seq = state.seq.wrapping_add(1);
        state.scale_connected = false;
        state.weight_g = 0.0;
        state.flow_gps = 0.0;
    }

    pub fn update_recording_active(&self, active: bool) {
        let mut state = lock_or_recover(&self.inner);
        state.recording_active = active;
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        *lock_or_recover(&self.inner)
    }
}

impl Default for SharedTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{SharedTelemetry, TelemetrySnapshot};

    fn approx_eq(left: f32, right: f32, tolerance: f32) {
        assert!(
            (left - right).abs() <= tolerance,
            "left={left}, right={right}"
        );
    }

    #[test]
    fn update_increments_sequence_and_overwrites_values() {
        let telemetry = SharedTelemetry::new();

        telemetry.update(93.2, 8.1, 117.5);
        telemetry.update(94.4, 8.2, 118.9);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.seq, 2);
        approx_eq(snapshot.temperature_c, 94.4, 1e-6);
        approx_eq(snapshot.pressure_bar, 8.2, 1e-6);
        approx_eq(snapshot.pressure_psi, 118.9, 1e-6);
        assert!(!snapshot.scale_connected);
        approx_eq(snapshot.weight_g, 0.0, 1e-6);
        approx_eq(snapshot.flow_gps, 0.0, 1e-6);
    }

    #[test]
    fn default_matches_new() {
        let via_new = SharedTelemetry::new().snapshot();
        let via_default = SharedTelemetry::default().snapshot();

        assert_eq!(via_new.seq, via_default.seq);
        approx_eq(via_new.temperature_c, via_default.temperature_c, 1e-6);
        approx_eq(via_new.pressure_bar, via_default.pressure_bar, 1e-6);
        approx_eq(via_new.pressure_psi, via_default.pressure_psi, 1e-6);
        assert_eq!(via_new.scale_connected, via_default.scale_connected);
        approx_eq(via_new.weight_g, via_default.weight_g, 1e-6);
        approx_eq(via_new.flow_gps, via_default.flow_gps, 1e-6);
    }

    #[test]
    fn scale_updates_are_tracked_separately() {
        let telemetry = SharedTelemetry::new();

        telemetry.update_scale(true, 33.5, 2.8);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.seq, 1);
        assert!(snapshot.scale_connected);
        approx_eq(snapshot.weight_g, 33.5, 1e-6);
        approx_eq(snapshot.flow_gps, 2.8, 1e-6);
    }

    #[test]
    fn clear_scale_resets_scale_fields() {
        let telemetry = SharedTelemetry::new();

        telemetry.update_scale(true, 18.4, 1.4);
        telemetry.clear_scale();

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.seq, 2);
        assert!(!snapshot.scale_connected);
        approx_eq(snapshot.weight_g, 0.0, 1e-6);
        approx_eq(snapshot.flow_gps, 0.0, 1e-6);
    }

    #[test]
    fn poisoned_lock_recovers_and_updates() {
        let state = Arc::new(Mutex::new(TelemetrySnapshot {
            seq: 41,
            temperature_c: 95.0,
            pressure_bar: 9.0,
            pressure_psi: 130.5,
            scale_connected: true,
            weight_g: 42.0,
            flow_gps: 3.1,
            recording_active: false,
        }));

        let state_for_panic = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _guard = state_for_panic.lock().expect("lock should succeed");
            panic!("intentional poison");
        })
        .join();

        let telemetry = SharedTelemetry { inner: state };
        // With lock_or_recover, the poisoned mutex is recovered and the update
        // succeeds (accepts potentially inconsistent state rather than crashing).
        telemetry.update(96.5, 9.3, 134.9);
        let snap = telemetry.snapshot();
        approx_eq(snap.temperature_c, 96.5, 0.001);
    }
}
