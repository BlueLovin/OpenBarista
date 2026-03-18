use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_hal::spi::SpiDevice;

const CONFIG_REG: u8 = 0x00;
const CONFIG_BIAS: u8 = 0x80;
const CONFIG_1SHOT: u8 = 0x20;
const CONFIG_3WIRE: u8 = 0x10;
const CONFIG_FAULTSTAT: u8 = 0x02;
const RTD_MSB_REG: u8 = 0x01;

const RTD_A: f32 = 3.9083e-3;
const RTD_B: f32 = -5.775e-7;

#[derive(Debug, Clone, Copy)]
pub struct TemperatureReading {
    pub raw_code: u16,
    pub resistance_ohms: f32,
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
    pub fn new(
        spi: SPI,
        ref_resistor: f32,
        nominal_resistance: f32,
        three_wire: bool,
        calibration_offset_c: f32,
    ) -> Result<Self> {
        let mut sensor = Self {
            spi,
            ref_resistor,
            nominal_resistance,
            three_wire,
            calibration_offset_c,
        };

        sensor.initialize()?;

        Ok(sensor)
    }

    pub fn read_temperature_c(&mut self) -> Result<TemperatureReading> {
        let raw_code = self.read_rtd()?;
        let resistance_ohms = raw_code as f32 * self.ref_resistor / 32_768.0;
        let temperature_c = self.calculate_temperature(raw_code) + self.calibration_offset_c;

        Ok(TemperatureReading {
            raw_code,
            resistance_ohms,
            temperature_c,
        })
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
        let mut resistance = raw_code as f32;
        resistance /= 32_768.0;
        resistance *= self.ref_resistor;

        let z1 = -RTD_A;
        let z2 = RTD_A * RTD_A - (4.0 * RTD_B);
        let z3 = (4.0 * RTD_B) / self.nominal_resistance;
        let z4 = 2.0 * RTD_B;

        let temp = (z2 + (z3 * resistance)).sqrt();
        let temp = (temp + z1) / z4;

        if temp >= 0.0 {
            return temp;
        }

        let normalized = (resistance / self.nominal_resistance) * 100.0;
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