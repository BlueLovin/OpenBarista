//! Connect-timeout watchdog using channels instead of spin-polling atomics.
//!
//! A single persistent thread is spawned per connect cycle. It sleeps until
//! armed via a channel message, counts down, and fires a cancellation signal
//! if the connect doesn't complete in time.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use embassy_sync::signal::Signal;
use esp_idf_hal::task::embassy_sync::EspRawMutex;
use log::warn;

use super::nimble;

/// Messages sent to the watchdog thread.
pub enum WatchdogMsg {
    /// Arm the watchdog for a new connect attempt.
    Arm,
    /// The current connect attempt completed (success or fail) — disarm.
    Done,
    /// Shut down the watchdog thread entirely.
    Quit,
}

/// Handle returned by [`spawn`] that the caller uses to communicate with the
/// watchdog and to signal the async connect future on abort.
pub struct WatchdogHandle {
    pub tx: Sender<WatchdogMsg>,
    pub abort_signal: std::sync::Arc<Signal<EspRawMutex, ()>>,
    _join: Option<thread::JoinHandle<()>>,
}

impl WatchdogHandle {
    /// Arm the watchdog for a new attempt. Resets the abort signal first.
    pub fn arm(&self) {
        self.abort_signal.reset();
        let _ = self.tx.send(WatchdogMsg::Arm);
    }

    /// Tell the watchdog the current attempt finished.
    pub fn done(&self) {
        let _ = self.tx.send(WatchdogMsg::Done);
    }
}

impl Drop for WatchdogHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(WatchdogMsg::Quit);
        if let Some(handle) = self._join.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a watchdog thread for the given BLE address and timeout.
///
/// The thread blocks on the channel until armed, then counts down. If `Done`
/// isn't received before the timeout expires, it forcibly cancels the BLE
/// connection and signals the abort.
pub fn spawn(
    addr_text: String,
    addr_type_str: String,
    timeout_ms: u32,
) -> WatchdogHandle {
    let (tx, rx): (Sender<WatchdogMsg>, Receiver<WatchdogMsg>) = mpsc::channel();
    let abort_signal = std::sync::Arc::new(Signal::<EspRawMutex, ()>::new());
    let wd_signal = abort_signal.clone();

    let handle = thread::Builder::new()
        .name("ble-wd".into())
        .stack_size(4096)
        .spawn(move || {
            watchdog_loop(rx, wd_signal, &addr_text, &addr_type_str, timeout_ms);
        })
        .expect("failed to spawn watchdog thread");

    WatchdogHandle {
        tx,
        abort_signal,
        _join: Some(handle),
    }
}

fn watchdog_loop(
    rx: Receiver<WatchdogMsg>,
    abort_signal: std::sync::Arc<Signal<EspRawMutex, ()>>,
    addr_text: &str,
    addr_type_str: &str,
    timeout_ms: u32,
) {
    loop {
        // Block until we receive a message (no spin-polling).
        let msg = match rx.recv() {
            Ok(m) => m,
            Err(_) => return, // Channel closed — parent dropped.
        };

        match msg {
            WatchdogMsg::Quit => return,
            WatchdogMsg::Done => continue, // Spurious done before arm — ignore.
            WatchdogMsg::Arm => {}         // Fall through to timeout countdown.
        }

        // Count down in 200 ms ticks, checking for Done/Quit each tick.
        let mut elapsed = 0u32;
        let mut timed_out = true;
        while elapsed < timeout_ms {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(WatchdogMsg::Done) => {
                    timed_out = false;
                    break;
                }
                Ok(WatchdogMsg::Quit) => return,
                Ok(WatchdogMsg::Arm) => {
                    // Re-armed mid-countdown — reset the timer.
                    elapsed = 0;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    elapsed += 200;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        if timed_out {
            warn!("watchdog: connect timed out after {timeout_ms}ms");
            nimble::force_terminate_connection(addr_text, addr_type_str);
            abort_signal.signal(());
        }
    }
}
