const ADC_MAX: f32 = 4095.0;
const ADC_VREF: f32 = 3.3;
const ZERO_PSI_VOLTAGE: f32 = 0.35;
const FULL_SCALE_VOLTAGE: f32 = 4.5;
const FULL_SCALE_PSI: f32 = 200.0;
const PSI_TO_BAR: f32 = 0.068_947_6;

const RTD_A: f32 = 3.9083e-3;
const RTD_B: f32 = -5.775e-7;

pub fn voltage_from_raw(raw: u16) -> f32 {
    (raw as f32 / ADC_MAX) * ADC_VREF
}

pub fn psi_from_voltage(volts: f32) -> f32 {
    ((volts - ZERO_PSI_VOLTAGE) * (FULL_SCALE_PSI / (FULL_SCALE_VOLTAGE - ZERO_PSI_VOLTAGE)))
        .max(0.0)
}

pub fn bar_from_psi(psi: f32) -> f32 {
    psi * PSI_TO_BAR
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
    use super::{bar_from_psi, psi_from_voltage, temperature_c_from_raw, voltage_from_raw};

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
    fn pressure_maps_full_scale_voltage_to_full_scale_psi() {
        approx_eq(psi_from_voltage(4.5), 200.0, 1e-3);
        approx_eq(bar_from_psi(200.0), 13.78952, 1e-5);
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
}
