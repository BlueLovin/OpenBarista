use std::sync::{Mutex, MutexGuard};

/// Acquires `mutex`, returning the guard on success. If the mutex is poisoned
/// (a thread panicked while holding it), the inner value is recovered and
/// returned anyway — the caller accepts that the value may be in a partially
/// inconsistent state.
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
