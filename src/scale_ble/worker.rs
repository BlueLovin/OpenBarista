use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use embassy_futures::select::{select, Either};
use embassy_sync::signal::Signal;
use esp_idf_hal::task::block_on;
use esp_idf_hal::task::embassy_sync::EspRawMutex;

use esp32_nimble::{BLEAddress, BLEDevice, BLEScan};

use openbarista::sync_utils::lock_or_recover;
use openbarista::telemetry_feed::SharedTelemetry;

use super::discovery::{discover_and_subscribe, read_battery};
use super::types::*;
use super::util::*;

pub(super) fn worker_loop(
    rx: std::sync::mpsc::Receiver<WorkerCommand>,
    state: Arc<Mutex<ScaleManagerState>>,
    telemetry: SharedTelemetry,
) {
    block_on(async {
        let ble_device = BLEDevice::take();

        {
            let mut s = lock_or_recover(&state);
            s.transport_ready = true;
            s.state = ScaleConnectionState::Idle;
            s.message = SCALE_READY_MESSAGE.to_owned();
        }

        let own_addr_text = get_own_ble_address();
        if let Some(ref a) = own_addr_text {
            println!("[scale] own BLE address: {a}");
        }

        println!("[scale] NimBLE transport ready");

        let mut active_client: Option<esp32_nimble::BLEClient> = None;

        while let Ok(command) = rx.recv() {
            match command {
                WorkerCommand::StartScan => {
                    if let Some(mut old_client) = active_client.take() {
                        let _ = old_client.disconnect();
                    }
                    cancel_gap_operations();

                    {
                        let mut s = lock_or_recover(&state);
                        s.discovered.clear();
                        s.state = ScaleConnectionState::Scanning;
                        s.message = "Scanning for nearby Bluetooth scales...".to_owned();
                    }

                    println!("[scale] starting BLE scan ({SCALE_SCAN_DURATION_S}s)");

                    let scan_state = state.clone();
                    let scan_own_addr = own_addr_text.clone();
                    let mut scan = BLEScan::new();
                    scan.active_scan(true)
                        .filter_duplicates(false)
                        .interval(100)
                        .window(99);

                    let result = scan
                        .start(
                            ble_device,
                            (SCALE_SCAN_DURATION_S * 1000) as i32,
                            |device, data| {
                                let addr = device.addr();
                                let name = data
                                    .name()
                                    .and_then(|n| core::str::from_utf8(n).ok())
                                    .unwrap_or("")
                                    .trim()
                                    .to_owned();
                                let name = if name.is_empty() {
                                    format!("{}", addr)
                                } else {
                                    name
                                };
                                let rssi = device.rssi() as i32;
                                let addr_text = format!("{}", addr);
                                let scale_like = is_scale_like_name(&name);

                                if scan_own_addr
                                    .as_deref()
                                    .map_or(false, |own| own.eq_ignore_ascii_case(&addr_text))
                                {
                                    return None::<()>;
                                }

                                if !scale_like && rssi < -88 && name == addr_text {
                                    return None::<()>;
                                }

                                let internal = DiscoveredScaleInternal {
                                    address_text: addr_text,
                                    addr_type_str: ble_addr_type_str(addr.addr_type()).to_owned(),
                                    name,
                                    rssi,
                                    protocol_hint: ScaleProtocol::Unknown,
                                    scale_like,
                                };

                                upsert_discovered(&scan_state, internal);
                                None::<()>
                            },
                        )
                        .await;

                    if let Err(e) = &result {
                        println!("[scale] scan returned error: {e:?}");
                    } else {
                        println!("[scale] scan future resolved OK");
                    }

                    let mut s = lock_or_recover(&state);
                    if s.state == ScaleConnectionState::Scanning {
                        s.state = ScaleConnectionState::Idle;
                        s.message = if s.discovered.is_empty() {
                            "No nearby scales found. Try moving the scale closer and scan again."
                                .to_owned()
                        } else {
                            format!(
                                "Found {} device(s). Tap one to connect.",
                                s.discovered.len()
                            )
                        };
                    }

                    println!("[scale] scan complete ({} devices)", s.discovered.len());
                }

                WorkerCommand::ConnectTarget(req) => {
                    if let Some(mut old_client) = active_client.take() {
                        println!("[scale] dropping previous client");
                        let _ = old_client.disconnect();
                    }
                    cancel_gap_operations();

                    let addr_type_str = req.addr_type_str.clone();
                    let addr_type = parse_nimble_addr_type(&addr_type_str);
                    let Some(addr) = BLEAddress::from_str(&req.address_text, addr_type) else {
                        let mut s = lock_or_recover(&state);
                        s.state = ScaleConnectionState::Error;
                        s.message = format!("Invalid BLE address: {}", req.address_text);
                        s.active = None;
                        continue;
                    };

                    println!(
                        "[scale] connecting to {} at {} ({})",
                        display_scale_name(&req.name),
                        req.address_text,
                        req.addr_type_str,
                    );

                    let mut client = ble_device.new_client();
                    let connected = run_connect_with_watchdog(
                        &mut client,
                        &addr,
                        &req,
                        &addr_type_str,
                        &state,
                        &telemetry,
                    )
                    .await;

                    if connected {
                        {
                            let s = lock_or_recover(&state);
                            if s.active.is_none() {
                                println!("[scale] connect succeeded but was cancelled, dropping");
                                let _ = client.disconnect();
                                continue;
                            }
                        }

                        let disc_state = state.clone();
                        let disc_telemetry = telemetry.clone();
                        let disc_name = req.name.clone();
                        client.on_disconnect(move |reason| {
                            println!(
                                "[scale] disconnected from {} (reason={reason})",
                                display_scale_name(&disc_name)
                            );
                            let mut s = lock_or_recover(&disc_state);
                            s.active = None;
                            s.state = ScaleConnectionState::Idle;
                            s.message =
                                format!("Disconnected from {}.", display_scale_name(&disc_name));
                            s.reset_live_values();
                            disc_telemetry.clear_scale();
                        });

                        println!("[scale] connected to {}", req.address_text);

                        {
                            let mut s = lock_or_recover(&state);
                            s.state = ScaleConnectionState::Discovering;
                            s.message = format!(
                                "Connected to {}. Discovering weight channel...",
                                display_scale_name(&req.name)
                            );
                        }

                        match discover_and_subscribe(&mut client, &req.name, &state, &telemetry)
                            .await
                        {
                            Ok(protocol) => {
                                {
                                    let mut s = lock_or_recover(&state);
                                    if let Some(active) = s.active.as_mut() {
                                        active.protocol = protocol;
                                    }
                                    s.state = ScaleConnectionState::Ready;
                                    s.message =
                                        format!("Connected to {}.", display_scale_name(&req.name));
                                    s.battery_percent = None;
                                }
                                telemetry.update_scale(true, 0.0, 0.0);

                                read_battery(&mut client, &state).await;

                                println!(
                                    "[scale] ready — streaming from {}",
                                    display_scale_name(&req.name)
                                );
                                active_client = Some(client);
                            }
                            Err(e) => {
                                println!("[scale] discovery failed: {e}");
                                let _ = client.disconnect();
                                let mut s = lock_or_recover(&state);
                                s.state = ScaleConnectionState::Error;
                                s.message = format!(
                                    "Connected to {} but could not find a weight channel: {e}",
                                    display_scale_name(&req.name)
                                );
                                s.active = None;
                                telemetry.clear_scale();
                            }
                        }
                    }
                }

                WorkerCommand::Disconnect => {
                    if let Some(mut old_client) = active_client.take() {
                        println!("[scale] disconnect requested, terminating link");
                        let _ = old_client.disconnect();
                    } else {
                        let mut s = lock_or_recover(&state);
                        s.active = None;
                        s.state = ScaleConnectionState::Idle;
                        s.message = "No scale is connected right now.".to_owned();
                        s.reset_live_values();
                        telemetry.clear_scale();
                    }
                }
            }
        }
    });
}

/// Runs the connect-with-retry loop using a persistent watchdog thread.
/// Returns `true` if the connection succeeded.
async fn run_connect_with_watchdog(
    client: &mut esp32_nimble::BLEClient,
    addr: &BLEAddress,
    req: &ConnectRequest,
    addr_type_str: &str,
    state: &Arc<Mutex<ScaleManagerState>>,
    telemetry: &SharedTelemetry,
) -> bool {
    let abort_signal = Arc::new(Signal::<EspRawMutex, ()>::new());
    let wd_abort = abort_signal.clone();
    let wd_arm = Arc::new(AtomicBool::new(false));
    let wd_arm2 = wd_arm.clone();
    let wd_done = Arc::new(AtomicBool::new(false));
    let wd_done2 = wd_done.clone();
    let wd_quit = Arc::new(AtomicBool::new(false));
    let wd_quit2 = wd_quit.clone();
    let wd_exited = Arc::new(AtomicBool::new(false));
    let wd_exited2 = wd_exited.clone();
    let wd_addr_text = req.address_text.clone();
    let wd_addr_type_str = addr_type_str.to_owned();

    let _wd_handle = thread::Builder::new()
        .name("ble-wd".into())
        .stack_size(4096)
        .spawn(move || {
            watchdog_thread(
                wd_arm2,
                wd_done2,
                wd_quit2,
                wd_exited2,
                wd_abort,
                &wd_addr_text,
                &wd_addr_type_str,
            );
        });

    let mut connected = false;

    for attempt in 1..=CONNECT_MAX_ATTEMPTS {
        {
            let s = lock_or_recover(state);
            if s.active.is_none() {
                println!("[scale] connect cancelled by user before attempt {attempt}");
                break;
            }
        }

        if attempt > 1 {
            println!("[scale] retrying connect (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
            cancel_gap_operations();
            thread::sleep(Duration::from_millis(500));
            {
                let mut s = lock_or_recover(state);
                s.message = format!("Attempting to pair to {}...", display_scale_name(&req.name));
            }
        }

        abort_signal.reset();
        wd_done.store(false, Ordering::Relaxed);
        wd_arm.store(true, Ordering::Release);

        println!("[scale] calling client.connect() (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})");
        let connect_result = match select(client.connect(addr), abort_signal.wait()).await {
            Either::First(result) => Some(result),
            Either::Second(()) => {
                println!("[scale] connect aborted by signal");
                None
            }
        };
        wd_done.store(true, Ordering::Release);

        match connect_result {
            Some(Ok(())) => {
                connected = true;
                break;
            }
            Some(Err(e)) => {
                cancel_gap_operations();
                let s = lock_or_recover(state);
                if s.active.is_none() {
                    println!("[scale] connect cancelled (err={e:?})");
                    break;
                }
                drop(s);
                if attempt < CONNECT_MAX_ATTEMPTS {
                    println!("[scale] connect attempt {attempt} failed: {e:?}, will retry");
                } else {
                    println!("[scale] connect failed after {CONNECT_MAX_ATTEMPTS} attempts: {e:?}");
                    let mut s = lock_or_recover(state);
                    s.state = ScaleConnectionState::Error;
                    s.message = format!(
                        "Could not connect to {} after {CONNECT_MAX_ATTEMPTS} attempts. Make sure the scale is on and nearby.",
                        display_scale_name(&req.name)
                    );
                    s.active = None;
                    telemetry.clear_scale();
                }
            }
            None => {
                cancel_gap_operations();
                let s = lock_or_recover(state);
                if s.active.is_none() {
                    println!("[scale] connect cancelled during watchdog abort");
                    break;
                }
                drop(s);
                if attempt < CONNECT_MAX_ATTEMPTS {
                    println!("[scale] connect attempt {attempt} timed out, will retry");
                } else {
                    println!("[scale] connect timed out after {CONNECT_MAX_ATTEMPTS} attempts");
                    let mut s = lock_or_recover(state);
                    s.state = ScaleConnectionState::Error;
                    s.message = format!(
                        "Connection to {} timed out after {CONNECT_MAX_ATTEMPTS} attempts. Make sure the scale is on and nearby.",
                        display_scale_name(&req.name)
                    );
                    s.active = None;
                    telemetry.clear_scale();
                }
            }
        }
    }

    // Shut down the persistent watchdog thread.
    wd_quit.store(true, Ordering::Release);
    while !wd_exited.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(10));
    }

    connected
}

fn watchdog_thread(
    arm: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    abort: Arc<Signal<EspRawMutex, ()>>,
    addr_text: &str,
    addr_type_str: &str,
) {
    loop {
        // Park until armed or told to quit.
        loop {
            if quit.load(Ordering::Acquire) {
                exited.store(true, Ordering::Release);
                return;
            }
            if arm.load(Ordering::Acquire) {
                arm.store(false, Ordering::Relaxed);
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let mut elapsed = 0u32;
        let mut timed_out = true;
        while elapsed < CONNECT_TIMEOUT_MS {
            thread::sleep(Duration::from_millis(200));
            if done.load(Ordering::Acquire) {
                timed_out = false;
                break;
            }
            elapsed += 200;
        }

        if timed_out {
            println!("[scale] WATCHDOG: connect timed out after {CONNECT_TIMEOUT_MS}ms");
            unsafe {
                esp_idf_svc::sys::ble_gap_conn_cancel();
                if let Some(a) =
                    BLEAddress::from_str(addr_text, parse_nimble_addr_type(addr_type_str))
                {
                    let ble_addr: esp_idf_svc::sys::ble_addr_t = a.into();
                    let mut desc: esp_idf_svc::sys::ble_gap_conn_desc = core::mem::zeroed();
                    if esp_idf_svc::sys::ble_gap_conn_find_by_addr(&ble_addr, &mut desc) == 0 {
                        println!(
                            "[scale] WATCHDOG: terminating conn_handle={}",
                            desc.conn_handle
                        );
                        esp_idf_svc::sys::ble_gap_terminate(
                            desc.conn_handle,
                            esp_idf_svc::sys::ble_error_codes_BLE_ERR_REM_USER_CONN_TERM as _,
                        );
                    }
                }
            }
            abort.signal(());
        }
    }
}
