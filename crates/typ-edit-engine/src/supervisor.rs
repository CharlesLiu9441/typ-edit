//! Panic-isolation supervisor for background workers.
//!
//! A worker runs its session loop until it returns normally (channels closed =
//! clean exit). A panic is caught, reported as [`EngineHealth::WorkerRestarted`],
//! and the session is re-run in place. Panics that cluster within
//! `RESTART_RESET_WINDOW` are counted; more than `MAX_CONSECUTIVE_RESTARTS`
//! consecutive panics stop the worker with [`EngineHealth::WorkerStopped`].

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

/// Which background pipeline a health event refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipeline {
    Parse,
    Compile,
    Raster,
    Spell,
    Search,
    Ide,
}

impl Pipeline {
    /// Stable lowercase name used in health messages.
    pub fn name(self) -> &'static str {
        match self {
            Pipeline::Parse => "parse",
            Pipeline::Compile => "compile",
            Pipeline::Raster => "raster",
            Pipeline::Spell => "spell",
            Pipeline::Search => "search",
            Pipeline::Ide => "ide",
        }
    }
}

/// Worker health events (engine -> UI).
#[derive(Clone, Debug)]
pub enum EngineHealth {
    /// A worker panicked and its session loop was restarted in place.
    WorkerRestarted { pipeline: Pipeline, message: String },
    /// A worker panicked repeatedly in a short window and was stopped (degraded).
    WorkerStopped { pipeline: Pipeline, message: String },
    /// A one-shot task (currently only compile runs) panicked; the worker lives on.
    TaskPanicked { pipeline: Pipeline, message: String },
}

const RESTART_BACKOFF: Duration = Duration::from_millis(100);
const MAX_CONSECUTIVE_RESTARTS: u32 = 5;
const RESTART_RESET_WINDOW: Duration = Duration::from_secs(30);

/// Runs `session` until it returns normally (channels closed = clean exit).
///
/// A panic is caught, reported as [`EngineHealth::WorkerRestarted`], and the
/// session re-run in place; panics that cluster within `RESTART_RESET_WINDOW`
/// are counted, and more than `MAX_CONSECUTIVE_RESTARTS` transitions to
/// [`EngineHealth::WorkerStopped`] and returns. Health sends use `try_send` on a
/// bounded channel and never block a worker.
pub(crate) fn supervise(pipeline: Pipeline, health: &Sender<EngineHealth>, session: impl FnMut()) {
    supervise_with(
        pipeline,
        health,
        session,
        RESTART_BACKOFF,
        MAX_CONSECUTIVE_RESTARTS,
        RESTART_RESET_WINDOW,
    );
}

/// [`supervise`] with injectable timing/count limits, so tests can use tiny windows.
pub(crate) fn supervise_with(
    pipeline: Pipeline,
    health: &Sender<EngineHealth>,
    mut session: impl FnMut(),
    backoff: Duration,
    max_consecutive: u32,
    reset_window: Duration,
) {
    let mut consecutive: u32 = 0;
    let mut last_panic: Option<Instant> = None;

    loop {
        match catch_unwind(AssertUnwindSafe(&mut session)) {
            // Clean exit: the worker's channels are closed.
            Ok(()) => return,
            Err(payload) => {
                let message = panic_message(payload);
                let now = Instant::now();
                consecutive = match last_panic {
                    Some(previous) if now.duration_since(previous) <= reset_window => {
                        consecutive + 1
                    }
                    _ => 1,
                };
                last_panic = Some(now);

                if consecutive > max_consecutive {
                    try_send(health, EngineHealth::WorkerStopped { pipeline, message });
                    return;
                }

                try_send(
                    health,
                    EngineHealth::WorkerRestarted {
                        pipeline,
                        message: message.clone(),
                    },
                );
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
            }
        }
    }
}

/// Sends a health event without ever blocking a worker or panicking on a full
/// or disconnected channel.
fn try_send(health: &Sender<EngineHealth>, event: EngineHealth) {
    let _ = health.try_send(event);
}

/// Extracts the panic payload as text, falling back to `"panicked"`.
fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crossbeam_channel::bounded;

    use super::*;

    fn restarted_pipelines(events: &[EngineHealth]) -> Vec<Pipeline> {
        events
            .iter()
            .filter_map(|event| match event {
                EngineHealth::WorkerRestarted { pipeline, .. } => Some(*pipeline),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn panicking_session_is_restarted_in_place() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let session = move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first run fails");
            }
        };

        let (tx, rx) = bounded(16);
        supervise_with(
            Pipeline::Parse,
            &tx,
            session,
            Duration::ZERO,
            5,
            Duration::from_secs(30),
        );
        drop(tx);

        assert_eq!(calls.load(Ordering::SeqCst), 2, "session ran twice");
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(restarted_pipelines(&events), vec![Pipeline::Parse]);
    }

    #[test]
    fn repeated_panics_stop_the_worker() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let session = move || {
            counter.fetch_add(1, Ordering::SeqCst);
            panic!("always");
        };

        let (tx, rx) = bounded(16);
        supervise_with(
            Pipeline::Spell,
            &tx,
            session,
            Duration::ZERO,
            2,
            Duration::from_secs(30),
        );
        drop(tx);

        assert!(
            calls.load(Ordering::SeqCst) <= 3,
            "calls are limited by the restart budget"
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            matches!(events.last(), Some(EngineHealth::WorkerStopped { pipeline, .. }) if *pipeline == Pipeline::Spell)
        );
        assert_eq!(restarted_pipelines(&events).len(), 2);
    }

    #[test]
    fn clean_return_emits_no_health_events() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let session = move || {
            counter.fetch_add(1, Ordering::SeqCst);
        };

        let (tx, rx) = bounded(16);
        supervise_with(
            Pipeline::Compile,
            &tx,
            session,
            Duration::ZERO,
            5,
            Duration::from_secs(30),
        );
        drop(tx);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rx.try_iter().count(), 0);
    }

    #[test]
    fn panic_message_surfaces_in_health_event() {
        let session = || panic!("boom msg");

        let (tx, rx) = bounded(16);
        supervise_with(
            Pipeline::Ide,
            &tx,
            session,
            Duration::ZERO,
            0,
            Duration::from_secs(30),
        );
        drop(tx);

        let events: Vec<_> = rx.try_iter().collect();
        let message = events.iter().find_map(|event| match event {
            EngineHealth::WorkerStopped { message, .. }
            | EngineHealth::WorkerRestarted { message, .. } => Some(message.clone()),
            _ => None,
        });
        assert_eq!(message.as_deref(), Some("boom msg"));
    }

    #[test]
    fn spaced_panics_reset_the_counter() {
        // A zero-width reset window makes every panic look like a fresh cluster,
        // so the worker never reaches the stop threshold.
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let session = move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            if n < 3 {
                panic!("spaced");
            }
        };

        let (tx, rx) = bounded(16);
        supervise_with(
            Pipeline::Search,
            &tx,
            session,
            Duration::ZERO,
            1,
            Duration::ZERO,
        );
        drop(tx);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "session ran until clean exit"
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EngineHealth::WorkerStopped { .. })),
            "a reset window prevents the stop transition"
        );
    }
}
