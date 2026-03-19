mod sensors;

use anyhow::Result;
use embedded_hal::spi::MODE_1;
use esp_idf_hal::adc::attenuation::DB_12;
use esp_idf_hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::spi;
use esp_idf_hal::spi::{SpiDeviceDriver, SpiDriver};
use esp_idf_hal::units::FromValueType;

use crate::sensors::pressure::PressureSensor;
use crate::sensors::temperature::Max31865;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    let spi_driver = SpiDriver::new::<spi::SPI2>(
        peripherals.spi2,
        pins.gpio18,
        pins.gpio23,
        Some(pins.gpio19),
        &spi::config::DriverConfig::new(),
    )?;

    let max31865_config = spi::config::Config::new()
        .baudrate(1.MHz().into())
        .data_mode(MODE_1);

    let max31865_device = SpiDeviceDriver::new(spi_driver, Some(pins.gpio5), &max31865_config)?;
    let mut temperature_sensor = Max31865::new(max31865_device, 430.0, 100.0, true, 4.0)?;

    let adc = AdcDriver::new(peripherals.adc1)?;
    let pressure_config = AdcChannelConfig {
        attenuation: DB_12,
        ..Default::default()
    };
    let pressure_channel = AdcChannelDriver::new(&adc, pins.gpio34, &pressure_config)?;
    let mut pressure_sensor = PressureSensor::new(pressure_channel);

    loop {
        let temperature = temperature_sensor.read_temperature_c()?;
        let pressure = pressure_sensor.read()?;

        println!(
            "Temp: {:.2} C | Pressure: {:.2} bar | Pressure (PSI): {:.2}",
            temperature.temperature_c, pressure.bar, pressure.psi,
        );

        FreeRtos::delay_ms(50);
    }
}
