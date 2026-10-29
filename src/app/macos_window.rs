use std::sync::atomic::{AtomicBool, Ordering};

use objc2_app_kit::{NSColor, NSView};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

static ROUNDED: AtomicBool = AtomicBool::new(false);

pub(super) fn sync_rounding(frame: &eframe::Frame, ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|input| {
        let viewport = input.viewport();
        (
            viewport.maximized.unwrap_or(false),
            viewport.fullscreen.unwrap_or(false),
        )
    });
    let radius = super::window_chrome::macos_corner_radius(maximized, fullscreen);
    let rounded = radius > 0.0;
    if ROUNDED.load(Ordering::Relaxed) == rounded {
        return;
    }
    if apply(frame, radius) {
        ROUNDED.store(rounded, Ordering::Relaxed);
    }
}

fn apply(frame: &eframe::Frame, radius: f64) -> bool {
    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return false;
    };
    let ns_view_ptr = appkit.ns_view.as_ptr().cast::<NSView>();
    if ns_view_ptr.is_null() {
        return false;
    }

    // SAFETY: the AppKit handle belongs to the live eframe window, and the
    // pointer is only used for the duration of this call.
    let ns_view = unsafe { &*ns_view_ptr };
    let Some(window) = ns_view.window() else {
        return false;
    };
    let Some(content_view) = window.contentView() else {
        return false;
    };

    // A clear, non-opaque window is required for the layer mask to expose the
    // desktop at the corners instead of the window server's background.
    window.setOpaque(false);
    let clear = NSColor::clearColor();
    window.setBackgroundColor(Some(&clear));
    content_view.setWantsLayer(true);
    let Some(layer) = content_view.layer() else {
        return false;
    };
    layer.setCornerRadius(radius);
    layer.setMasksToBounds(radius > 0.0);
    window.invalidateShadow();
    true
}
