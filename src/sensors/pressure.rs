use core::borrow::Borrow;

use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::adc::AdcChannel;
use esp_idf_hal::sys::EspError;

const ADC_MAX: f32 = 4095.0;
const ADC_VREF: f32 = 3.3;
const ZERO_PSI_VOLTAGE: f32 = 0.35;
const FULL_SCALE_VOLTAGE: f32 = 4.5;
const FULL_SCALE_PSI: f32 = 200.0;
const PSI_TO_BAR: f32 = 0.068_947_6;

#[derive(Debug, Clone, Copy)]
pub struct PressureReading {
    pub raw: u16,
    pub millivolts: u16,
    pub volts: f32,
    pub psi: f32,
    pub bar: f32,
}

pub struct PressureSensor<'d, C, M>
where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    channel: AdcChannelDriver<'d, C, M>,
}

impl<'d, C, M> PressureSensor<'d, C, M>
where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    pub fn new(channel: AdcChannelDriver<'d, C, M>) -> Self {
        Self { channel }
    }

    pub fn read(&mut self) -> Result<PressureReading, EspError> {
        let raw = self.channel.read_raw()?;
        let volts = (raw as f32 / ADC_MAX) * ADC_VREF;
        let millivolts = (volts * 1000.0).round() as u16;
        let psi = psi_from_voltage(volts);
        let bar = psi * PSI_TO_BAR;

        Ok(PressureReading {
            raw,
            millivolts,
            volts,
            psi,
            bar,
        })
    }
}

fn psi_from_voltage(volts: f32) -> f32 {
    ((volts - ZERO_PSI_VOLTAGE) * (FULL_SCALE_PSI / (FULL_SCALE_VOLTAGE - ZERO_PSI_VOLTAGE)))
        .max(0.0)
}
