use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use esp32_nimble::utilities::BleUuid;

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use super::protocol::parse_weight_measurement;
use super::types::*;
use super::util::{hex_bytes, is_scale_like_name, unix_time_ms};

pub(super) async fn discover_and_subscribe(
    client: &mut esp32_nimble::BLEClient,
    scale_name: &str,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<ScaleProtocol> {
    let scale_like_name = is_scale_like_name(scale_name);

    // Priority 1: Standard Weight Scale Service (0x181D / 0x2A9D)
    if let Ok(service) = client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_WEIGHT_SCALE))
        .await
    {
        if let Ok(characteristic) = service
            .get_characteristic(BleUuid::from_uuid16(UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT))
            .await
        {
            if characteristic.can_notify()
                || characteristic.can_indicate()
                || characteristic.can_read()
            {
                println!(
                    "[scale] found standard weight characteristic 0x{:04X} on service 0x{:04X}",
                    UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT, UUID_SERVICE_WEIGHT_SCALE
                );
                subscribe_weight_notifications(
                    characteristic,
                    ScaleProtocol::StandardWeight,
                    format!(
                        "0x{:04X} on service 0x{:04X}",
                        UUID_CHARACTERISTIC_WEIGHT_MEASUREMENT, UUID_SERVICE_WEIGHT_SCALE
                    ),
                    state,
                    telemetry,
                )
                .await?;
                return Ok(ScaleProtocol::StandardWeight);
            }
        }

        if let Ok(chars) = service.get_characteristics().await {
            let mut subscribed_any = false;
            for characteristic in chars {
                if characteristic.can_notify() || characteristic.can_indicate() {
                    let channel_label = format!(
                        "{} on service 0x{:04X}",
                        characteristic.uuid(),
                        UUID_SERVICE_WEIGHT_SCALE
                    );
                    println!(
                        "[scale] found alternate notify char {} in weight-scale service",
                        characteristic.uuid()
                    );
                    subscribe_weight_notifications(
                        characteristic,
                        ScaleProtocol::GenericNotify,
                        channel_label,
                        state,
                        telemetry,
                    )
                    .await?;
                    subscribed_any = true;
                }
            }
            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    // Priority 2: Common vendor services (0xFFF0, 0xFFE0, etc.)
    for &svc_uuid16 in COMMON_VENDOR_SERVICE_UUIDS {
        if let Ok(service) = client.get_service(BleUuid::from_uuid16(svc_uuid16)).await {
            let mut subscribed_any = false;
            let mut seen_uuids = BTreeSet::new();

            for &char_uuid16 in COMMON_VENDOR_NOTIFY_UUIDS {
                if let Ok(characteristic) = service
                    .get_characteristic(BleUuid::from_uuid16(char_uuid16))
                    .await
                {
                    let channel_label =
                        format!("{} on service 0x{:04X}", characteristic.uuid(), svc_uuid16);
                    if !seen_uuids.insert(channel_label.clone()) {
                        continue;
                    }

                    if characteristic.can_notify() || characteristic.can_indicate() {
                        let proto = if svc_uuid16 == 0x0FFE && char_uuid16 == 0xFF11 {
                            println!("[scale] detected Bookoo protocol (svc 0x0FFE / char 0xFF11)");
                            ScaleProtocol::Bookoo
                        } else {
                            ScaleProtocol::GenericNotify
                        };
                        println!(
                            "[scale] found vendor char 0x{:04X} on service 0x{:04X} proto={proto}",
                            char_uuid16, svc_uuid16,
                        );
                        subscribe_weight_notifications(
                            characteristic,
                            proto,
                            channel_label,
                            state,
                            telemetry,
                        )
                        .await?;
                        subscribed_any = true;
                    }
                }
            }

            if let Ok(chars) = service.get_characteristics().await {
                for characteristic in chars {
                    let channel_label =
                        format!("{} on service 0x{:04X}", characteristic.uuid(), svc_uuid16);
                    if !seen_uuids.insert(channel_label.clone()) {
                        continue;
                    }

                    if characteristic.can_notify() || characteristic.can_indicate() {
                        println!(
                            "[scale] found notify char {} in vendor service 0x{:04X}",
                            characteristic.uuid(),
                            svc_uuid16
                        );
                        subscribe_weight_notifications(
                            characteristic,
                            ScaleProtocol::GenericNotify,
                            channel_label,
                            state,
                            telemetry,
                        )
                        .await?;
                        subscribed_any = true;
                    }
                }
            }

            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    // Priority 3: Brute-force — subscribe to all notifying chars on any
    // plausible service if the device looks like a scale.
    if scale_like_name {
        if let Ok(services) = client.get_services().await {
            let mut subscribed_any = false;
            for service in services {
                let svc_uuid = service.uuid();
                if let BleUuid::Uuid16(v) = svc_uuid {
                    if v == 0x1800 || v == 0x1801 || v == UUID_SERVICE_BATTERY {
                        continue;
                    }
                }

                if let Ok(chars) = service.get_characteristics().await {
                    for characteristic in chars {
                        if characteristic.can_notify() || characteristic.can_indicate() {
                            let channel_label =
                                format!("{} on service {}", characteristic.uuid(), svc_uuid);
                            println!(
                                "[scale] brute-force: using char {} on service {}",
                                characteristic.uuid(),
                                svc_uuid,
                            );
                            subscribe_weight_notifications(
                                characteristic,
                                ScaleProtocol::GenericNotify,
                                channel_label,
                                state,
                                telemetry,
                            )
                            .await?;
                            subscribed_any = true;
                        }
                    }
                }
            }
            if subscribed_any {
                return Ok(ScaleProtocol::GenericNotify);
            }
        }
    }

    Err(anyhow!("No weight characteristic found on this device."))
}

async fn subscribe_weight_notifications(
    characteristic: &mut esp32_nimble::BLERemoteCharacteristic,
    protocol: ScaleProtocol,
    channel_label: String,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> Result<()> {
    let notify_state = state.clone();
    let notify_telemetry = telemetry.clone();
    let notify_channel_label = channel_label.clone();
    let mut debug_remaining: u8 = 8;

    characteristic.on_notify(move |data| {
        let (previous_weight_g, should_log) = {
            let s = lock_or_recover(&notify_state);
            (s.weight_g, debug_remaining > 0)
        };

        if should_log {
            debug_remaining = debug_remaining.saturating_sub(1);
            println!(
                "[scale] notify char={} protocol={protocol} bytes={} len={}",
                notify_channel_label,
                hex_bytes(data),
                data.len()
            );
        }

        if let Some(weight_g) = parse_weight_measurement(protocol, data, previous_weight_g) {
            if should_log {
                println!(
                    "[scale] parsed weight_g={:.2} (prev={:.2})",
                    weight_g, previous_weight_g
                );
            }
            apply_weight_measurement(&notify_state, &notify_telemetry, weight_g);
        }
    });

    if characteristic.can_indicate() && !characteristic.can_notify() {
        characteristic
            .subscribe_indicate(false)
            .await
            .map_err(|e| anyhow!("indication subscribe failed: {e:?}"))?;
        println!("[scale] subscribed to indications on {channel_label}");
    } else {
        characteristic
            .subscribe_notify(false)
            .await
            .map_err(|e| anyhow!("notify subscribe failed: {e:?}"))?;
        println!("[scale] subscribed to notifications on {channel_label}");
    }

    if characteristic.can_read() {
        if let Ok(value) = characteristic.read_value().await {
            if !value.is_empty() {
                println!(
                    "[scale] initial read char={} protocol={protocol} bytes={} len={}",
                    channel_label,
                    hex_bytes(&value),
                    value.len()
                );

                let previous_weight_g = lock_or_recover(state).weight_g;
                if let Some(weight_g) =
                    parse_weight_measurement(protocol, &value, previous_weight_g)
                {
                    apply_weight_measurement(state, telemetry, weight_g);
                }
            }
        }
    }

    Ok(())
}

pub(super) fn apply_weight_measurement(
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
    weight_g: f32,
) {
    let mut s = lock_or_recover(state);
    let flow_gps = s.flow_estimator.observe(weight_g, unix_time_ms());
    s.weight_g = weight_g;
    s.flow_gps = flow_gps;
    s.state = ScaleConnectionState::Ready;
    telemetry.update_scale(true, weight_g, flow_gps);
}

pub(super) async fn read_battery(
    client: &mut esp32_nimble::BLEClient,
    state: &Arc<Mutex<ScaleManagerState>>,
) {
    let service = match client
        .get_service(BleUuid::from_uuid16(UUID_SERVICE_BATTERY))
        .await
    {
        Ok(s) => s,
        Err(_) => return,
    };
    let characteristic = match service
        .get_characteristic(BleUuid::from_uuid16(UUID_CHARACTERISTIC_BATTERY_LEVEL))
        .await
    {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Ok(value) = characteristic.read_value().await {
        if let Some(&level) = value.first() {
            println!("[scale] battery={level}%");
            lock_or_recover(state).battery_percent = Some(level);
        }
    }
}
