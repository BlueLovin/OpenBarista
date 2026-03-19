mod sensors;
mod web_assets;
mod wifi_provision;

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
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use crate::sensors::pressure::PressureSensor;
use crate::sensors::temperature::Max31865;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    // --- WiFi provisioning & mDNS -------------------------------------------
    // On first boot this will start a SoftAP named "OpenBarista" and serve a
    // captive portal at 192.168.4.1 so the user can enter their home WiFi
    // credentials.  On subsequent boots the device connects to the saved
    // network and advertises itself as http://openbarista.local via mDNS.
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_partition = EspDefaultNvsPartition::take()?;
    // Keep _wifi_stack alive for the lifetime of the program; dropping it would
    // disconnect WiFi and stop mDNS.
    let _wifi_stack = wifi_provision::setup_wifi(peripherals.modem, sysloop, nvs_partition)?;
    // Keep station-mode HTTP server alive so openbarista.local serves a page.
    let _http_server = wifi_provision::start_station_http_server(&_wifi_stack.ip_addr)?;
    // -------------------------------------------------------------------------

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
