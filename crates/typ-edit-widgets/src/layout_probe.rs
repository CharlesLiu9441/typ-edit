//! Test-only geometry probe for container layout assertions.
//!
//! egui keeps its widget/UI stack private, so the app cannot enumerate every
//! `Ui` child after a frame. This module records a container's `max_rect` and
//! the union of everything allocated inside it (`min_rect`) at the points where
//! the app owns the container. The probe is disabled by default, so production
//! calls only pay a thread-local enabled check.

use std::cell::RefCell;

use egui::{Id, Rect, Ui};

/// One observed container and the extent of its children.
#[derive(Clone, Copy, Debug)]
pub struct ContainerBounds {
    pub id: Id,
    /// The container's allocated rect.
    pub container: Rect,
    /// Union of the container's child allocations.
    pub used: Rect,
    /// Scroll containers may legitimately contain off-screen children; only
    /// their horizontal bounds are checked.
    pub scrollable: bool,
}

thread_local! {
    static RECORDS: RefCell<Option<Vec<ContainerBounds>>> = const { RefCell::new(None) };
}

/// Starts recording container bounds, replacing any previous records.
pub fn enable() {
    RECORDS.with(|records| *records.borrow_mut() = Some(Vec::new()));
}

/// Stops recording. Existing records are discarded.
pub fn disable() {
    RECORDS.with(|records| *records.borrow_mut() = None);
}

/// Drops records but keeps recording enabled.
pub fn clear() {
    RECORDS.with(|records| {
        if let Some(records) = records.borrow_mut().as_mut() {
            records.clear();
        }
    });
}

/// Returns the current frame's records, if recording is enabled.
pub fn records() -> Vec<ContainerBounds> {
    RECORDS.with(|records| records.borrow().clone().unwrap_or_default())
}

/// Records `ui`'s container and used extent. `scrollable` marks a vertical
/// scroll viewport, whose used height may exceed its max height by design.
pub fn record(ui: &Ui, id: Id, scrollable: bool) {
    let Some(mut records) = RECORDS.with(|records| records.borrow_mut().take()) else {
        return;
    };
    let container = ui.max_rect();
    let used = ui.min_rect();
    if container.is_finite() && used.is_finite() {
        records.push(ContainerBounds {
            id,
            container,
            used,
            scrollable,
        });
    }
    RECORDS.with(|slot| *slot.borrow_mut() = Some(records));
}
