use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_hal::spi::SpiDevice;
use openbarista::telemetry_math::temperature_c_from_raw;

const CONFIG_REG: u8 = 0x00;
const CONFIG_BIAS: u8 = 0x80;
const CONFIG_1SHOT: u8 = 0x20;
const CONFIG_3WIRE: u8 = 0x10;
const CONFIG_FAULTSTAT: u8 = 0x02;
const RTD_MSB_REG: u8 = 0x01;

const REF_RESISTOR_OHMS: f32 = 430.0;
const NOMINAL_RESISTANCE_OHMS: f32 = 100.0;
const IS_THREE_WIRE: bool = true;
const CALIBRATION_OFFSET_C: f32 = 0.0;

#[derive(Debug, Clone, Copy)]
pub struct TemperatureReading {
    pub temperature_c: f32,
}

pub struct Max31865<SPI> {
    spi: SPI,
    ref_resistor: f32,
    nominal_resistance: f32,
    three_wire: bool,
    calibration_offset_c: f32,
}

impl<SPI> Max31865<SPI>
where
    SPI: SpiDevice<u8>,
    SPI::Error: core::fmt::Debug,
{
    pub fn new(spi: SPI) -> Result<Self> {
        let mut sensor = Self {
            spi,
            ref_resistor: REF_RESISTOR_OHMS,
            nominal_resistance: NOMINAL_RESISTANCE_OHMS,
            three_wire: IS_THREE_WIRE,
            calibration_offset_c: CALIBRATION_OFFSET_C,
        };

        sensor.initialize()?;

        Ok(sensor)
    }

    pub fn read_temperature_c(&mut self) -> Result<TemperatureReading> {
        let raw_code = self.read_rtd()?;
        let temperature_c = self.calculate_temperature(raw_code) + self.calibration_offset_c;

        Ok(TemperatureReading { temperature_c })
    }

    fn initialize(&mut self) -> Result<()> {
        self.clear_fault()?;
        self.set_wire_mode()?;
        self.enable_bias(false)?;
        Ok(())
    }

    fn read_rtd(&mut self) -> Result<u16> {
        self.clear_fault()?;
        self.enable_bias(true)?;
        thread::sleep(Duration::from_millis(10));

        let mut config = self.read_register8(CONFIG_REG)?;
        config |= CONFIG_1SHOT;
        self.write_register8(CONFIG_REG, config)?;

        thread::sleep(Duration::from_millis(65));

        let rtd = self.read_register16(RTD_MSB_REG)? >> 1;

        self.enable_bias(false)?;

        Ok(rtd)
    }

    fn calculate_temperature(&self, raw_code: u16) -> f32 {
        temperature_c_from_raw(raw_code, self.ref_resistor, self.nominal_resistance)
    }

    fn set_wire_mode(&mut self) -> Result<()> {
        let mut config = self.read_register8(CONFIG_REG)?;

        if self.three_wire {
            config |= CONFIG_3WIRE;
        } else {
            config &= !CONFIG_3WIRE;
        }

        self.write_register8(CONFIG_REG, config)
    }

    fn clear_fault(&mut self) -> Result<()> {
        let mut config = self.read_register8(CONFIG_REG)?;
        config &= !0x2c;
        config |= CONFIG_FAULTSTAT;
        self.write_register8(CONFIG_REG, config)
    }

    fn enable_bias(&mut self, enabled: bool) -> Result<()> {
        let mut config = self.read_register8(CONFIG_REG)?;

        if enabled {
            config |= CONFIG_BIAS;
        } else {
            config &= !CONFIG_BIAS;
        }

        self.write_register8(CONFIG_REG, config)
    }

    fn read_register8(&mut self, reg: u8) -> Result<u8> {
        let mut buffer = [reg & 0x7f, 0];
        self.spi
            .transfer_in_place(&mut buffer)
            .map_err(|err| anyhow!("MAX31865 SPI read8 failed: {err:?}"))?;

        Ok(buffer[1])
    }

    fn read_register16(&mut self, reg: u8) -> Result<u16> {
        let mut buffer = [reg & 0x7f, 0, 0];
        self.spi
            .transfer_in_place(&mut buffer)
            .map_err(|err| anyhow!("MAX31865 SPI read16 failed: {err:?}"))?;

        Ok(u16::from(buffer[1]) << 8 | u16::from(buffer[2]))
    }

    fn write_register8(&mut self, reg: u8, value: u8) -> Result<()> {
        let buffer = [reg | 0x80, value];
        self.spi
            .write(&buffer)
            .map_err(|err| anyhow!("MAX31865 SPI write failed: {err:?}"))?;

        Ok(())
    }
}
