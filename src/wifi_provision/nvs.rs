use anyhow::{anyhow, Result};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};

use crate::scale_ble::SavedScale;

pub(super) const NVS_NAMESPACE: &str = "wifi";
pub(super) const NVS_SSID_KEY: &str = "ssid";
pub(super) const NVS_PASS_KEY: &str = "pass";
pub(super) const SETTINGS_NAMESPACE: &str = "settings";
pub(super) const SETTINGS_LABEL_KEY: &str = "label";
pub(super) const SETTINGS_TEMP_OFFSET_KEY: &str = "temp_offset_c";
pub(super) const SCALE_NAMESPACE: &str = "scale";
pub(super) const SCALE_ADDR_KEY: &str = "addr";
pub(super) const SCALE_NAME_KEY: &str = "name";
pub(super) const SCALE_ADDR_TYPE_KEY: &str = "addr_type";

pub(super) const MAX_SSID_LEN: usize = 32;
pub(super) const MAX_PASS_LEN: usize = 64;
pub(super) const MAX_LABEL_LEN: usize = 32;
const MAX_SCALE_NAME_LEN: usize = 48;
const MAX_SCALE_ADDR_LEN: usize = 17;
const MAX_SCALE_ADDR_TYPE_LEN: usize = 16;

#[derive(Clone)]
pub(super) struct DeviceSettings {
    pub ssid: String,
    pub device_label: String,
    pub temperature_offset_c: f32,
}

pub(super) fn read_device_settings(
    nvs_partition: &EspDefaultNvsPartition,
) -> Result<DeviceSettings> {
    let nvs_wifi = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)?;
    let mut ssid_buf = [0u8; 33];
    let ssid = nvs_wifi
        .get_str(NVS_SSID_KEY, &mut ssid_buf)?
        .unwrap_or("")
        .to_owned();

    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    let mut label_buf = [0u8; MAX_LABEL_LEN + 1];
    let device_label = nvs_settings
        .get_str(SETTINGS_LABEL_KEY, &mut label_buf)?
        .unwrap_or("OpenBarista")
        .to_owned();

    let mut temp_offset_buf = [0u8; 24];
    let temperature_offset_c = nvs_settings
        .get_str(SETTINGS_TEMP_OFFSET_KEY, &mut temp_offset_buf)?
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0);

    Ok(DeviceSettings {
        ssid,
        device_label,
        temperature_offset_c,
    })
}

pub(super) fn save_device_label(
    nvs_partition: &EspDefaultNvsPartition,
    device_label: &str,
) -> Result<()> {
    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    nvs_settings.set_str(SETTINGS_LABEL_KEY, device_label)?;
    Ok(())
}

pub(super) fn save_temperature_offset(
    nvs_partition: &EspDefaultNvsPartition,
    temperature_offset_c: f32,
) -> Result<()> {
    let nvs_settings = EspNvs::new(nvs_partition.clone(), SETTINGS_NAMESPACE, true)?;
    nvs_settings.set_str(
        SETTINGS_TEMP_OFFSET_KEY,
        &format!("{temperature_offset_c:.3}"),
    )?;
    Ok(())
}

pub(super) fn read_saved_scale(
    nvs_partition: &EspDefaultNvsPartition,
) -> Result<Option<SavedScale>> {
    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;

    let mut address_buf = [0u8; MAX_SCALE_ADDR_LEN + 1];
    let mut name_buf = [0u8; MAX_SCALE_NAME_LEN + 1];
    let mut addr_type_buf = [0u8; MAX_SCALE_ADDR_TYPE_LEN + 1];

    let address = nvs_scale
        .get_str(SCALE_ADDR_KEY, &mut address_buf)?
        .unwrap_or("")
        .trim()
        .to_owned();

    if address.is_empty() {
        return Ok(None);
    }

    let name = nvs_scale
        .get_str(SCALE_NAME_KEY, &mut name_buf)?
        .unwrap_or("Saved scale")
        .trim()
        .to_owned();
    let addr_type = nvs_scale
        .get_str(SCALE_ADDR_TYPE_KEY, &mut addr_type_buf)?
        .unwrap_or("public")
        .trim()
        .to_owned();

    Ok(Some(SavedScale {
        address,
        name,
        addr_type,
    }))
}

pub(super) fn save_saved_scale(
    nvs_partition: &EspDefaultNvsPartition,
    saved_scale: &SavedScale,
) -> Result<()> {
    if saved_scale.address.is_empty()
        || saved_scale.address.len() > MAX_SCALE_ADDR_LEN
        || saved_scale.name.len() > MAX_SCALE_NAME_LEN
        || saved_scale.addr_type.len() > MAX_SCALE_ADDR_TYPE_LEN
    {
        return Err(anyhow!("Scale settings are invalid or out of range."));
    }

    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;
    nvs_scale.set_str(SCALE_ADDR_KEY, &saved_scale.address)?;
    nvs_scale.set_str(SCALE_NAME_KEY, &saved_scale.name)?;
    nvs_scale.set_str(SCALE_ADDR_TYPE_KEY, &saved_scale.addr_type)?;
    Ok(())
}

pub(super) fn clear_saved_scale(nvs_partition: &EspDefaultNvsPartition) -> Result<()> {
    let nvs_scale = EspNvs::new(nvs_partition.clone(), SCALE_NAMESPACE, true)?;
    nvs_scale.remove(SCALE_ADDR_KEY)?;
    nvs_scale.remove(SCALE_NAME_KEY)?;
    nvs_scale.remove(SCALE_ADDR_TYPE_KEY)?;
    Ok(())
}

pub(super) fn read_saved_credentials(
    nvs_partition: &EspDefaultNvsPartition,
) -> Result<(Option<String>, Option<String>)> {
    let nvs = EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true)?;
    let mut ssid_buf = [0u8; 33];
    let mut pass_buf = [0u8; 65];

    let ssid = nvs
        .get_str(NVS_SSID_KEY, &mut ssid_buf)?
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let pass = nvs.get_str(NVS_PASS_KEY, &mut pass_buf)?.map(str::to_owned);

    Ok((ssid, pass))
}
