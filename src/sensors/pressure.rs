use core::borrow::Borrow;

use openbarista::telemetry_math::{bar_from_psi, psi_from_voltage, voltage_from_raw};
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::adc::AdcChannel;
use esp_idf_hal::sys::EspError;

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
        let volts = voltage_from_raw(raw);
        let millivolts = (volts * 1000.0).round() as u16;
        let psi = psi_from_voltage(volts);
        let bar = bar_from_psi(psi);

        Ok(PressureReading {
            raw,
            millivolts,
            volts,
            psi,
            bar,
        })
    }
}
