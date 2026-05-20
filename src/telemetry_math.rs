const ADC_MAX: f32 = 4095.0;
const ADC_VREF: f32 = 3.3;
const ZERO_PSI_VOLTAGE: f32 = 0.35;
const MIN_PSI_TO_REPORT: f32 = 2.0;
const FULL_SCALE_VOLTAGE: f32 = 4.5;
const FULL_SCALE_PSI: f32 = 250.0;
const PSI_TO_BAR: f32 = 0.068_947_6;
const FLOW_EMA_ALPHA: f32 = 0.34;
const FLOW_DECAY_ALPHA: f32 = 0.62;
const FLOW_MIN_SAMPLE_MS: u64 = 90;
const FLOW_WEIGHT_DEADBAND_G: f32 = 0.12;
const FLOW_SNAP_ZERO_GPS: f32 = 0.05;
const FLOW_MAX_GPS: f32 = 12.0;

const RTD_A: f32 = 3.9083e-3;
const RTD_B: f32 = -5.775e-7;

pub fn voltage_from_raw(raw: u16) -> f32 {
    (raw as f32 / ADC_MAX) * ADC_VREF
}

pub fn psi_from_voltage(volts: f32) -> f32 {
    let psi = (volts - ZERO_PSI_VOLTAGE)
        * (FULL_SCALE_PSI / (FULL_SCALE_VOLTAGE - ZERO_PSI_VOLTAGE)).max(0.0);

    if psi < MIN_PSI_TO_REPORT {
        return 0.0;
    }

    psi
}

pub fn bar_from_psi(psi: f32) -> f32 {
    psi * PSI_TO_BAR
}

pub fn sanitize_weight_g(weight_g: f32) -> f32 {
    if !weight_g.is_finite() {
        return 0.0;
    }

    weight_g.max(0.0)
}

pub fn sanitize_signed_weight_g(weight_g: f32) -> f32 {
    const MAX_ABS_WEIGHT_G: f32 = 5_000.0;

    if weight_g.is_finite() {
        weight_g.clamp(-MAX_ABS_WEIGHT_G, MAX_ABS_WEIGHT_G)
    } else {
        0.0
    }
}

#[derive(Debug, Clone)]
pub struct FlowEstimator {
    last_weight_g: Option<f32>,
    last_timestamp_ms: Option<u64>,
    smoothed_flow_gps: f32,
}

impl FlowEstimator {
    pub fn new() -> Self {
        Self {
            last_weight_g: None,
            last_timestamp_ms: None,
            smoothed_flow_gps: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.last_weight_g = None;
        self.last_timestamp_ms = None;
        self.smoothed_flow_gps = 0.0;
    }

    pub fn observe(&mut self, weight_g: f32, timestamp_ms: u64) -> f32 {
        let weight_g = sanitize_weight_g(weight_g);

        let Some(previous_weight_g) = self.last_weight_g else {
            self.last_weight_g = Some(weight_g);
            self.last_timestamp_ms = Some(timestamp_ms);
            return 0.0;
        };

        let Some(previous_timestamp_ms) = self.last_timestamp_ms else {
            self.last_weight_g = Some(weight_g);
            self.last_timestamp_ms = Some(timestamp_ms);
            return 0.0;
        };

        let delta_ms = timestamp_ms.saturating_sub(previous_timestamp_ms);

        if delta_ms < FLOW_MIN_SAMPLE_MS {
            return self.smoothed_flow_gps;
        }

        self.last_weight_g = Some(weight_g);
        self.last_timestamp_ms = Some(timestamp_ms);

        let delta_weight_g = weight_g - previous_weight_g;
        if delta_weight_g < -0.5 {
            self.smoothed_flow_gps = 0.0;
            return 0.0;
        }

        let raw_flow_gps = if delta_weight_g <= FLOW_WEIGHT_DEADBAND_G {
            0.0
        } else {
            (delta_weight_g / (delta_ms as f32 / 1000.0)).clamp(0.0, FLOW_MAX_GPS)
        };

        if self.smoothed_flow_gps == 0.0 {
            self.smoothed_flow_gps = raw_flow_gps;
        } else {
            let smoothing_alpha = if raw_flow_gps == 0.0 {
                FLOW_DECAY_ALPHA
            } else {
                FLOW_EMA_ALPHA
            };
            self.smoothed_flow_gps =
                self.smoothed_flow_gps * (1.0 - smoothing_alpha) + raw_flow_gps * smoothing_alpha;
        }

        if raw_flow_gps == 0.0 && self.smoothed_flow_gps < FLOW_SNAP_ZERO_GPS {
            self.smoothed_flow_gps = 0.0;
        }

        self.smoothed_flow_gps
    }
}

impl Default for FlowEstimator {
    fn default() -> Self {
        Self::new()
    }
}

pub fn resistance_from_raw(raw_code: u16, ref_resistor: f32) -> f32 {
    raw_code as f32 * ref_resistor / 32_768.0
}

pub fn temperature_c_from_raw(raw_code: u16, ref_resistor: f32, nominal_resistance: f32) -> f32 {
    let resistance = resistance_from_raw(raw_code, ref_resistor);

    let z1 = -RTD_A;
    let z2 = RTD_A * RTD_A - (4.0 * RTD_B);
    let z3 = (4.0 * RTD_B) / nominal_resistance;
    let z4 = 2.0 * RTD_B;

    let temp = (z2 + (z3 * resistance)).sqrt();
    let temp = (temp + z1) / z4;

    if temp >= 0.0 {
        return temp;
    }

    let normalized = (resistance / nominal_resistance) * 100.0;
    let mut poly = normalized;

    let mut negative_temp = -242.02;
    negative_temp += 2.2228 * poly;
    poly *= normalized;
    negative_temp += 2.5859e-3 * poly;
    poly *= normalized;
    negative_temp -= 4.8260e-6 * poly;
    poly *= normalized;
    negative_temp -= 2.8183e-8 * poly;
    poly *= normalized;
    negative_temp += 1.5243e-10 * poly;

    negative_temp
}

#[cfg(test)]
mod tests {
    use super::{
        bar_from_psi, psi_from_voltage, sanitize_signed_weight_g, sanitize_weight_g, temperature_c_from_raw,
        voltage_from_raw, FlowEstimator,
    };

    const REF_RESISTOR: f32 = 430.0;
    const NOMINAL_RESISTANCE: f32 = 100.0;
    const RTD_A: f32 = 3.9083e-3;
    const RTD_B: f32 = -5.775e-7;
    const RTD_C: f32 = -4.183e-12;

    fn approx_eq(left: f32, right: f32, tolerance: f32) {
        let delta = (left - right).abs();
        assert!(
            delta <= tolerance,
            "left={left}, right={right}, delta={delta}, tolerance={tolerance}"
        );
    }

    fn resistance_for_pt100(temp_c: f32) -> f32 {
        if temp_c >= 0.0 {
            NOMINAL_RESISTANCE * (1.0 + RTD_A * temp_c + RTD_B * temp_c * temp_c)
        } else {
            NOMINAL_RESISTANCE
                * (1.0
                    + RTD_A * temp_c
                    + RTD_B * temp_c * temp_c
                    + RTD_C * (temp_c - 100.0) * temp_c * temp_c * temp_c)
        }
    }

    fn raw_code_for_temp(temp_c: f32) -> u16 {
        let resistance = resistance_for_pt100(temp_c);
        ((resistance * 32_768.0) / REF_RESISTOR).round() as u16
    }

    #[test]
    fn pressure_clamps_below_zero_point() {
        approx_eq(psi_from_voltage(0.0), 0.0, 1e-6);
        approx_eq(psi_from_voltage(0.35), 0.0, 1e-6);
    }

    #[test]
    fn signed_weight_sanitizer_clamps_extremes() {
        approx_eq(sanitize_signed_weight_g(-12.34), -12.34, 1e-6);
        approx_eq(sanitize_signed_weight_g(9_999.0), 5_000.0, 1e-6);
        approx_eq(sanitize_signed_weight_g(f32::NAN), 0.0, 1e-6);
    }

    #[test]
    fn pressure_maps_full_scale_voltage_to_full_scale_psi() {
        approx_eq(psi_from_voltage(FULL_SCALE_VOLTAGE), FULL_SCALE_PSI, 1e-3);
        // Hard-coded expected value to catch regressions in PSI_TO_BAR: 250 PSI ≈ 17.2369 bar
        approx_eq(bar_from_psi(FULL_SCALE_PSI), 17.2369, 1e-3);
    }

    #[test]
    fn raw_adc_voltage_matches_arduino_formula() {
        approx_eq(voltage_from_raw(0), 0.0, 1e-6);
        approx_eq(voltage_from_raw(2048), 2048.0 / 4095.0 * 3.3, 1e-6);
        approx_eq(voltage_from_raw(4095), 3.3, 1e-6);
    }

    #[test]
    fn pt100_conversion_handles_zero_celsius() {
        let raw_code = raw_code_for_temp(0.0);
        let temperature = temperature_c_from_raw(raw_code, REF_RESISTOR, NOMINAL_RESISTANCE);

        approx_eq(temperature, 0.0, 0.05);
    }

    #[test]
    fn pt100_conversion_handles_boiling_range() {
        let raw_code = raw_code_for_temp(100.0);
        let temperature = temperature_c_from_raw(raw_code, REF_RESISTOR, NOMINAL_RESISTANCE);

        approx_eq(temperature, 100.0, 0.15);
    }

    #[test]
    fn pt100_conversion_handles_negative_temperature_branch() {
        let raw_code = raw_code_for_temp(-25.0);
        let temperature = temperature_c_from_raw(raw_code, REF_RESISTOR, NOMINAL_RESISTANCE);

        approx_eq(temperature, -25.0, 0.35);
    }

    #[test]
    fn weight_sanitizer_clamps_invalid_values() {
        approx_eq(sanitize_weight_g(-4.0), 0.0, 1e-6);
        approx_eq(sanitize_weight_g(18.25), 18.25, 1e-6);
        approx_eq(sanitize_weight_g(f32::NAN), 0.0, 1e-6);
    }

    #[test]
    fn flow_estimator_reports_positive_flow_for_rising_weight() {
        let mut estimator = FlowEstimator::new();

        approx_eq(estimator.observe(0.0, 0), 0.0, 1e-6);
        let flow = estimator.observe(12.0, 200);

        assert!(flow > 0.0, "expected positive flow, got {flow}");
    }

    #[test]
    fn flow_estimator_damps_to_zero_when_weight_stops() {
        let mut estimator = FlowEstimator::new();

        estimator.observe(0.0, 0);
        estimator.observe(6.0, 200);
        let flow = estimator.observe(6.02, 450);

        assert!(flow < 6.0, "expected smoothed flow to decay, got {flow}");
    }

    #[test]
    fn flow_estimator_resets_after_weight_drop() {
        let mut estimator = FlowEstimator::new();

        estimator.observe(15.0, 0);
        estimator.observe(25.0, 200);
        approx_eq(estimator.observe(4.0, 500), 0.0, 1e-6);
    }
}
