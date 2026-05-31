use std::sync::{Mutex, MutexGuard};

/// Acquires `mutex`, returning the guard on success. If the mutex is poisoned
/// (a thread panicked while holding it), the inner value is recovered and
/// returned anyway — the caller accepts that the value may be in a partially
/// inconsistent state.
///
/// **All shared runtime mutexes in this codebase use this helper** (telemetry,
/// shot recorder, shot store, connect-progress, etc.) so that a single thread
/// panic cannot permanently break an API endpoint or the sensor loop.  Do not
/// use bare `mutex.lock().unwrap()` or `if let Ok(g) = mutex.lock()` for
/// shared state — that silently stops processing after a poison event.
///
/// The `T: ?Sized` bound is intentional: it allows this helper to be called
/// with trait-object mutexes such as `Mutex<dyn ShotStore + Send>`.
pub fn lock_or_recover<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Acquires `mutex`, returning the guard on success. Panics if the mutex is
/// poisoned — use this when operating on partially inconsistent state would be
/// worse than crashing.
pub fn lock_or_panic<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("mutex poisoned: refusing to continue with potentially inconsistent state")
}
