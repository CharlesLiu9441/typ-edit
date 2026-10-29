//! Small synchronization helpers shared across the engine.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, recovering the data even if a previous holder panicked.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
