use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy)]
pub struct TelemetrySnapshot {
    pub seq: u64,
    pub temperature_c: f32,
    pub pressure_bar: f32,
    pub pressure_psi: f32,
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
            })),
        }
    }

    pub fn update(&self, temperature_c: f32, pressure_bar: f32, pressure_psi: f32) {
        let mut state = lock_or_recover(&self.inner);
        state.seq = state.seq.wrapping_add(1);
        state.temperature_c = temperature_c;
        state.pressure_bar = pressure_bar;
        state.pressure_psi = pressure_psi;
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

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
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
    }

    #[test]
    fn default_matches_new() {
        let via_new = SharedTelemetry::new().snapshot();
        let via_default = SharedTelemetry::default().snapshot();

        assert_eq!(via_new.seq, via_default.seq);
        approx_eq(via_new.temperature_c, via_default.temperature_c, 1e-6);
        approx_eq(via_new.pressure_bar, via_default.pressure_bar, 1e-6);
        approx_eq(via_new.pressure_psi, via_default.pressure_psi, 1e-6);
    }

    #[test]
    fn poisoned_lock_is_recovered_without_zeroing_data() {
        let state = Arc::new(Mutex::new(TelemetrySnapshot {
            seq: 41,
            temperature_c: 95.0,
            pressure_bar: 9.0,
            pressure_psi: 130.5,
        }));

        let state_for_panic = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _guard = state_for_panic.lock().expect("lock should succeed");
            panic!("intentional poison");
        })
        .join();

        let telemetry = SharedTelemetry { inner: state };
        telemetry.update(96.5, 9.3, 134.9);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.seq, 42);
        approx_eq(snapshot.temperature_c, 96.5, 1e-6);
        approx_eq(snapshot.pressure_bar, 9.3, 1e-6);
        approx_eq(snapshot.pressure_psi, 134.9, 1e-6);
    }
}
