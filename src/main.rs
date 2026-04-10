#[cfg(not(target_arch = "xtensa"))]
compile_error!("OpenBarista firmware must be built for an xtensa target.");

#[cfg(not(target_arch = "xtensa"))]
fn main() {
    println!("OpenBarista firmware binary is only supported on xtensa targets.");
}

mod scale_ble;
mod sensors;
mod web_assets;
mod wifi_provision;

use std::sync::Arc;

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
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use openbarista::telemetry_feed::SharedTelemetry;

use crate::sensors::pressure::PressureSensor;
use crate::sensors::temperature::Max31865;

fn main() -> Result<()> {
    // Ensure the ESP-IDF sys crate's patches are linked in, so that the correct
    // symbols are available for the ESP-IDF components we use.
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take()?;
    let (wifi_modem, bluetooth_modem) = peripherals.modem.split();
    let pins = peripherals.pins;
    let nvs_partition = EspDefaultNvsPartition::take()?;

    let telemetry = SharedTelemetry::new();
    let scale_runtime = match scale_ble::ScaleRuntime::try_new(
        bluetooth_modem,
        Some(nvs_partition.clone()),
        telemetry.clone(),
    ) {
        Ok(runtime) => Arc::new(runtime),
        Err(err) => {
            println!("[scale] BLE runtime unavailable: {err:?}");
            Arc::new(scale_ble::ScaleRuntime::disabled(format!(
                "Bluetooth scale support is unavailable right now: {err}"
            )))
        }
    };

    let wifi_runtime = wifi_provision::setup_wifi(
        wifi_modem,
        nvs_partition,
        telemetry.clone(),
        scale_runtime.clone(),
    )?;
    println!(
        "[main] Connectivity ready at http://{}",
        wifi_runtime.ip_addr()
    );

    let temp_sensor_bus = SpiDriver::new::<spi::SPI2>(
        peripherals.spi2,
        pins.gpio18,
        pins.gpio23,
        Some(pins.gpio19),
        &spi::config::DriverConfig::new(),
    )?;

    let temp_sensor_spi_config = spi::config::Config::new()
        .baudrate(1.MHz().into())
        .data_mode(MODE_1);

    let temp_sensor_device =
        SpiDeviceDriver::new(temp_sensor_bus, Some(pins.gpio5), &temp_sensor_spi_config)?;
    let mut temperature_sensor = Max31865::new(temp_sensor_device)?;

    let pressure_sensor_adc = AdcDriver::new(peripherals.adc1)?;
    let pressure_sensor_adc_config = AdcChannelConfig {
        attenuation: DB_12,
        ..Default::default()
    };
    let pressure_sensor_channel = AdcChannelDriver::new(
        &pressure_sensor_adc,
        pins.gpio34,
        &pressure_sensor_adc_config,
    )?;
    let mut pressure_sensor = PressureSensor::new(pressure_sensor_channel);

    let mut applied_temperature_offset_c = wifi_runtime.temperature_offset_c();
    temperature_sensor.set_calibration_offset_c(applied_temperature_offset_c);
    println!("[temp] Applied calibration offset: {applied_temperature_offset_c:.3} C");

    loop {
        let configured_temperature_offset_c = wifi_runtime.temperature_offset_c();
        if (configured_temperature_offset_c - applied_temperature_offset_c).abs() > 1e-6 {
            temperature_sensor.set_calibration_offset_c(configured_temperature_offset_c);
            applied_temperature_offset_c = configured_temperature_offset_c;
            println!("[temp] Applied calibration offset: {configured_temperature_offset_c:.3} C");
        }

        let temperature = temperature_sensor.read_temperature_c()?;
        let pressure = pressure_sensor.read()?;

        telemetry.update(temperature.temperature_c, pressure.bar, pressure.psi);

        // println!(
        //     "Temp: {:.2} C | Pressure: {:.2} bar | Pressure (PSI): {:.2}",
        //     temperature.temperature_c, pressure.bar, pressure.psi,
        // );

        FreeRtos::delay_ms(50);
    }
}
