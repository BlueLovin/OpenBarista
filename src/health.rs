//! Firmware health checks.
//!
//! Two complementary mechanisms:
//!
//! 1. **Hang monitor** — the main loop feeds a heartbeat every iteration
//!    (~50 ms). A monitor thread restarts the device if the heartbeat goes
//!    stale, which turns "machine hangs forever, must unplug" into
//!    "machine reboots itself and logs the incident". Paused while an OTA
//!    flash is in progress (flash erase stalls the main loop legitimately).
//!
//! 2. **OTA confirm** — with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` a
//!    freshly OTA-flashed image boots in "pending verify" state. Once the
//!    firmware has been alive and feeding its heartbeat for
//!    [`CONFIRM_AFTER`], the running slot is marked valid. If it instead
//!    crashes/reboots before that, the bootloader rolls back to the
//!    previous slot automatically.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// How long the heartbeat may be silent before the monitor restarts the
/// device. Generous enough to ride out sensor/BLE hiccups.
pub const HANG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Monitor poll interval.
pub const MONITOR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// Uptime after which a freshly OTA-flipped slot is marked valid.
pub const CONFIRM_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

static LAST_FEED_SECS: AtomicU32 = AtomicU32::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);
static CONFIRMED: AtomicBool = AtomicBool::new(false);

fn uptime_secs() -> u32 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs() as u32
}

/// Records a heartbeat. Call from the main loop.
pub fn feed() {
    LAST_FEED_SECS.store(uptime_secs(), Ordering::Relaxed);
}

/// Time since the last heartbeat.
pub fn stale_for() -> std::time::Duration {
    let last = LAST_FEED_SECS.load(Ordering::Relaxed);
    std::time::Duration::from_secs(uptime_secs().saturating_sub(last) as u64)
}

/// Suspends hang detection (e.g. during OTA flash, which stalls the main
/// loop while erasing flash sectors).
pub fn pause_hang_monitor() {
    PAUSED.store(true, Ordering::Relaxed);
}

/// Resumes hang detection.
pub fn resume_hang_monitor() {
    PAUSED.store(false, Ordering::Relaxed);
}

fn hang_monitor_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// Starts the hang-monitor thread. Call once at boot.
pub fn start_monitor() {
    // Start the stall window from *now* (see main.rs for why this is started
    // only after Wi-Fi provisioning completes).
    feed();
    std::thread::Builder::new()
        .name("health-monitor".into())
        // Generous stack: the monitor thread does format!, mutex locking,
        // println! and NVS writes; 4 KB proved too tight to trust.
        .stack_size(8192)
        .spawn(|| loop {
            std::thread::sleep(MONITOR_INTERVAL);
            if hang_monitor_paused() {
                continue;
            }
            if stale_for() > HANG_TIMEOUT {
                crate::crash_log::record(&format!(
                    "health: main loop stalled for {}s, restarting",
                    stale_for().as_secs()
                ));
                crate::crash_log::flush();
                println!("[health] main loop stalled, restarting");
                restart_device();
            }
        })
        .expect("failed to spawn health monitor thread");
}

/// Marks the running OTA slot as valid once the firmware has proven itself
/// healthy (alive for [`CONFIRM_AFTER`] with a live heartbeat). No-op on
/// subsequent calls and on host builds.
pub fn confirm_running_slot_valid() {
    if CONFIRMED.load(Ordering::Relaxed) {
        return;
    }
    if u64::from(uptime_secs()) < CONFIRM_AFTER.as_secs() {
        return;
    }
    if stale_for() > HANG_TIMEOUT {
        // The heartbeat isn't trustworthy yet; don't confirm a sick firmware.
        return;
    }
    CONFIRMED.store(true, Ordering::Relaxed);
    confirm_impl();
}

#[cfg(target_arch = "xtensa")]
fn confirm_impl() {
    match crate::ota_flash::mark_running_slot_valid() {
        Ok(()) => {
            crate::crash_log::record("health: OTA slot confirmed valid");
            println!("[health] OTA slot confirmed valid");
        }
        Err(err) => {
            // Non-fatal: without rollback enabled this is a no-op anyway.
            println!("[health] failed to confirm OTA slot: {err:?}");
        }
    }
}

#[cfg(not(target_arch = "xtensa"))]
fn confirm_impl() {}

#[cfg(target_arch = "xtensa")]
fn restart_device() -> ! {
    unsafe { esp_idf_svc::sys::esp_restart() }
}

#[cfg(not(target_arch = "xtensa"))]
fn restart_device() -> ! {
    panic!("health monitor would restart the device now")
}
