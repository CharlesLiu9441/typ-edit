//! Background disposal for large UI-owned values.
//!
//! Replacing a compiled document drops its `DocResources`, `WorldState` and page
//! frames. Freeing those large graphs on the UI thread produces a visible hitch,
//! so replacements hand the old value to one long-lived drop thread instead.
//! Ordering is irrelevant: the values are no longer observed once moved here.

use crossbeam_channel::{Sender, TrySendError, unbounded};
use std::sync::OnceLock;

type Job = Box<dyn Send + 'static>;

fn sender() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Job>();
        let spawned = std::thread::Builder::new()
            .name("typ-edit-drop".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    drop(job);
                }
            });
        if let Err(err) = spawned {
            log::error!("cannot start background drop worker: {err}");
        }
        tx
    })
}

/// Drops `value` on the background disposal thread.
///
/// A disconnected worker (spawn failure) falls back to dropping on the caller:
/// this is a best-effort optimization, never a correctness boundary.
pub(super) fn later<T: Send + 'static>(value: T) {
    match sender().try_send(Box::new(value)) {
        Ok(()) | Err(TrySendError::Disconnected(_)) | Err(TrySendError::Full(_)) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Marker(std::sync::mpsc::Sender<()>);

    impl Drop for Marker {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn later_moves_the_drop_to_the_worker() {
        let (tx, rx) = std::sync::mpsc::channel();
        later(Marker(tx));
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("drop worker runs");
    }
}
