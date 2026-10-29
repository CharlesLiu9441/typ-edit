//! Shared geometry helpers for the virtualized text path.

use crate::{DRAG_SCROLL_GAIN, DRAG_SCROLL_MAX_SPEED};

/// The vertical scroll offset added for one frame while a selection drag
/// reaches beyond the viewport: negative above its top, positive below its
/// bottom, proportional to the overshoot and capped, `0.0` when inside.
pub(crate) fn drag_autoscroll_delta(pointer_y: f32, viewport: egui::Rect, dt: f32) -> f32 {
    let overshoot = if pointer_y > viewport.bottom() {
        pointer_y - viewport.bottom()
    } else if pointer_y < viewport.top() {
        pointer_y - viewport.top()
    } else {
        return 0.0;
    };
    let speed = (overshoot.abs() * DRAG_SCROLL_GAIN).min(DRAG_SCROLL_MAX_SPEED);
    overshoot.signum() * speed * dt.min(0.1)
}
