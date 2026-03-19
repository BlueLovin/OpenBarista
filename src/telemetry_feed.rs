use std::sync::{Arc, Mutex};

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
        if let Ok(mut state) = self.inner.lock() {
            state.seq = state.seq.wrapping_add(1);
            state.temperature_c = temperature_c;
            state.pressure_bar = pressure_bar;
            state.pressure_psi = pressure_psi;
        }
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        if let Ok(state) = self.inner.lock() {
            *state
        } else {
            TelemetrySnapshot {
                seq: 0,
                temperature_c: 0.0,
                pressure_bar: 0.0,
                pressure_psi: 0.0,
            }
        }
    }
}

impl Default for SharedTelemetry {
    fn default() -> Self {
        Self::new()
    }
}
