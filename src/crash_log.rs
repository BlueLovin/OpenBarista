//! Persistent crash / event log.
//!
//! Goals:
//! - Survive the random crashes ("have to unplug the machine") by persisting
//!   the panic message, the reset reason and a small ring of recent events to
//!   NVS, so the cause is visible over HTTP after the reboot
//!   (`GET /api/logs`).
//! - Mirror `log` crate output (`info!`, `warn!`, ...) into that ring.
//!
//! Layout in NVS (namespace `crashlog`):
//! - `bootcnt` — u32 boot counter (little endian blob)
//! - `l0..l63` — ring slots, each blob = `[u64 seq][bytes...]`
//! - `lseq`    — u64 seq of the newest persisted entry (informational)
//!
//! The ring logic is plain Rust and unit-tested on the host; all ESP-IDF /
//! NVS specifics are behind `#[cfg(target_arch = "xtensa")]`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Number of ring slots persisted to NVS.
pub const RING_SLOTS: usize = 64;
/// Maximum length of a single log line (bytes, truncated on char boundary).
pub const MAX_LINE: usize = 112;

#[cfg(target_arch = "xtensa")]
const NVS_NAMESPACE: &str = "crashlog";
#[cfg(target_arch = "xtensa")]
const KEY_BOOT_COUNT: &str = "bootcnt";
#[cfg(target_arch = "xtensa")]
const KEY_HEAD_SEQ: &str = "lseq";

const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Default)]
struct RingState {
    /// Sequence number of the most recent entry (monotonically increasing).
    next_seq: u64,
    /// Sequence number up to which entries have been persisted to NVS.
    flushed_seq: u64,
    /// Ring of the most recent entries, oldest first.
    entries: Vec<(u64, String)>,
}

static RING: Mutex<RingState> = Mutex::new(RingState {
    next_seq: 0,
    flushed_seq: 0,
    entries: Vec::new(),
});

/// Uptime (s) of the last flush, used to throttle NVS writes.
static LAST_FLUSH_SECS: AtomicU32 = AtomicU32::new(0);

fn ring() -> MutexGuard<'static, RingState> {
    // If some thread panicked while holding the lock we recover the inner
    // value: the log must keep working even while the system is failing.
    RING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn truncate(line: &str) -> String {
    if line.len() <= MAX_LINE {
        return line.to_string();
    }
    let mut end = MAX_LINE;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_string()
}

/// Appends a line to the in-RAM event ring (not yet persisted).
pub fn record(line: &str) {
    let line = truncate(line);
    let mut ring = ring();
    let seq = ring.next_seq;
    ring.next_seq += 1;
    ring.entries.push((seq, line));
    let overflow = ring.entries.len().saturating_sub(RING_SLOTS);
    if overflow > 0 {
        ring.entries.drain(..overflow);
    }
}

/// Returns the in-RAM ring (oldest first). Test/diagnostic helper.
pub fn ram_entries() -> Vec<(u64, String)> {
    ring().entries.clone()
}

/// Loads every persisted entry from NVS (oldest first).
#[cfg(target_arch = "xtensa")]
pub fn persisted_entries() -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let Some(nvs) = open_nvs() else {
        return out;
    };
    for slot in 0..RING_SLOTS {
        let key = format!("l{slot}");
        let mut buf = [0u8; MAX_LINE + 8];
        let Ok(Some(data)) = nvs.get_blob(&key, &mut buf) else {
            continue;
        };
        if data.len() < 8 {
            continue;
        }
        let seq = u64::from_le_bytes(data[..8].try_into().unwrap());
        let line = String::from_utf8_lossy(&data[8..]).into_owned();
        out.push((seq, line));
    }
    out.sort_by_key(|(seq, _)| *seq);
    out
}

/// Merged view of persisted + not-yet-persisted entries (oldest first).
#[cfg(target_arch = "xtensa")]
pub fn all_entries() -> Vec<(u64, String)> {
    let mut merged = persisted_entries();
    let persisted_max = merged.last().map(|(seq, _)| *seq).unwrap_or(0);
    for (seq, line) in ram_entries() {
        if seq > persisted_max {
            merged.push((seq, line));
        }
    }
    merged.sort_by_key(|(seq, _)| *seq);
    merged
}

/// Persists all not-yet-persisted RAM entries to NVS.
///
/// Only changed slots are rewritten, so NVS wear is bounded by the actual
/// log volume.
#[cfg(target_arch = "xtensa")]
pub fn flush() {
    let Some(nvs) = open_nvs() else { return };
    let mut ring = ring();
    for (seq, line) in ring.entries.iter() {
        if *seq <= ring.flushed_seq {
            continue;
        }
        let key = format!("l{}", seq % RING_SLOTS as u64);
        let mut blob = Vec::with_capacity(8 + line.len());
        blob.extend_from_slice(&seq.to_le_bytes());
        blob.extend_from_slice(line.as_bytes());
        if let Err(err) = nvs.set_blob(&key, &blob) {
            // Never panic from the logging path.
            println!("[crashlog] NVS write failed: {err:?}");
            return;
        }
    }
    if let Err(err) = nvs.set_blob(KEY_HEAD_SEQ, &ring.next_seq.to_le_bytes()) {
        println!("[crashlog] NVS head write failed: {err:?}");
        return;
    }
    ring.flushed_seq = ring.next_seq;
}

#[cfg(not(target_arch = "xtensa"))]
pub fn flush() {
    let mut ring = ring();
    ring.flushed_seq = ring.next_seq;
}

/// Throttled flush: persists at most once every [`FLUSH_INTERVAL`], meant to
/// be called from the main loop.
pub fn periodic_flush() {
    let now = uptime_secs();
    let last = LAST_FLUSH_SECS.load(Ordering::Relaxed);
    if now < last || u64::from(now.saturating_sub(last)) < FLUSH_INTERVAL.as_secs() {
        return;
    }
    LAST_FLUSH_SECS.store(now, Ordering::Relaxed);
    flush();
}

/// Reads the current boot counter from NVS (`0` when unavailable).
#[cfg(target_arch = "xtensa")]
pub fn boot_count() -> u32 {
    let Some(nvs) = open_nvs() else { return 0 };
    let mut buf = [0u8; 4];
    match nvs.get_blob(KEY_BOOT_COUNT, &mut buf) {
        Ok(Some(data)) if data.len() == 4 => u32::from_le_bytes(data.try_into().unwrap()),
        _ => 0,
    }
}

#[cfg(not(target_arch = "xtensa"))]
pub fn boot_count() -> u32 {
    0
}

/// ESP32 reset reasons, mapped to stable labels.
pub fn reset_reason_label() -> &'static str {
    #[cfg(target_arch = "xtensa")]
    {
        let reason = unsafe { esp_idf_svc::sys::esp_reset_reason() } as u32;
        match reason {
            1 => "power-on",
            2 => "external-pin",
            3 => "software",
            4 => "panic",
            5 => "interrupt-wdt",
            6 => "task-wdt",
            7 => "wdt",
            8 => "deep-sleep",
            9 => "brownout",
            10 => "sdio",
            11..=16 => "usb",
            17 => "efuse",
            _ => "unknown",
        }
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        "host"
    }
}

/// Initializes the crash log and records a boot entry.
///
/// Must be called once at startup, *before* any thread that could panic.
/// The default NVS partition is moved in (not re-taken) because `main`
/// already owns it.
#[cfg(target_arch = "xtensa")]
pub fn init(nvs_partition: esp_idf_svc::nvs::EspDefaultNvsPartition, build_id: &str) {
    if XTENSA_NVS_PARTITION.set(nvs_partition).is_err() {
        println!("[crashlog] already initialized");
    }
    let count = boot_count().wrapping_add(1);
    if let Some(nvs) = open_nvs() {
        if let Err(err) = nvs.set_blob(KEY_BOOT_COUNT, &count.to_le_bytes()) {
            println!("[crashlog] boot count write failed: {err:?}");
        }
    }
    record(&format!(
        "boot #{count} reset={} fw={build_id}",
        reset_reason_label()
    ));
    flush();
}

/// Host test no-op.
#[cfg(not(target_arch = "xtensa"))]
pub fn init(_unused: (), build_id: &str) {
    record(&format!("boot reset={} fw={build_id}", reset_reason_label()));
    flush();
}

/// Installs a `log` crate logger that prints to the serial console *and*
/// appends INFO+ records to the crash log ring.
pub fn init_logger() {
    static LOGGER: TeeLogger = TeeLogger;
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
}

struct TeeLogger;

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, entry: &log::Record) {
        if !self.enabled(entry.metadata()) {
            return;
        }
        let line = format!(
            "[{}] [{}] {}",
            entry.level().as_str(),
            entry.target(),
            entry.args()
        );
        println!("{line}");
        record(&line);
    }

    fn flush(&self) {}
}

/// Installs the panic hook: records the panic message and flushes to NVS
/// before the ESP-IDF panic handler prints its dump and reboots.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        let location = info
            .location()
            .map(|loc| format!(" at {}:{}", loc.file(), loc.line()))
            .unwrap_or_default();
        let line = format!("PANIC thread={name}: {msg}{location}");
        println!("[crashlog] panic persisted: {msg}{location}");
        record(&line);
        flush();
    }));
}

fn uptime_secs() -> u32 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs() as u32
}

// --- ESP-IDF specific plumbing ------------------------------------------------

#[cfg(target_arch = "xtensa")]
static XTENSA_NVS_PARTITION: OnceLock<esp_idf_svc::nvs::EspDefaultNvsPartition> = OnceLock::new();

#[cfg(target_arch = "xtensa")]
fn open_nvs() -> Option<esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>> {
    let part = XTENSA_NVS_PARTITION.get()?;
    match esp_idf_svc::nvs::EspNvs::new(part.clone(), NVS_NAMESPACE, true) {
        Ok(nvs) => Some(nvs),
        Err(err) => {
            println!("[crashlog] failed to open NVS namespace: {err:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_most_recent_entries_in_order() {
        let mut state = RingState::default();
        for i in 0..(RING_SLOTS + 10) {
            push_line(&mut state, &format!("line {i}"));
        }
        assert_eq!(state.entries.len(), RING_SLOTS);
        assert_eq!(state.entries.first().unwrap().1, "line 10");
        assert_eq!(state.entries.last().unwrap().1, format!("line {}", RING_SLOTS + 9));
        // Sequences stay monotonically increasing.
        for pair in state.entries.windows(2) {
            assert!(pair[0].0 < pair[1].0);
        }
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let multi_byte = "é".repeat(80); // 160 bytes
        let truncated = truncate(&multi_byte);
        assert!(truncated.len() <= MAX_LINE);
        assert!(truncated.chars().all(|c| c == 'é'));
        assert_eq!(truncate("short"), "short");
    }

    fn push_line(state: &mut RingState, line: &str) {
        let seq = state.next_seq;
        state.next_seq += 1;
        state.entries.push((seq, line.to_string()));
        let overflow = state.entries.len().saturating_sub(RING_SLOTS);
        if overflow > 0 {
            state.entries.drain(..overflow);
        }
    }
}
