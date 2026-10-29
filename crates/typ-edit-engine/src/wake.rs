use std::sync::Arc;

/// Handle used by background workers to wake the UI.
///
/// The engine must not depend on egui; the app supplies a closure that
/// schedules a repaint. A blanket impl keeps that call site a one-liner.
pub trait Wake: Send + Sync + 'static {
    fn wake(&self);
}

impl<F: Fn() + Send + Sync + 'static> Wake for F {
    fn wake(&self) {
        self()
    }
}

/// Shared, cloneable wake handle handed to every worker thread.
pub type Waker = Arc<dyn Wake>;
