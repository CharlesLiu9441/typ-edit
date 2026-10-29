//! Shared painting paths for the tab bar: the tab silhouettes and the active
//! tab's accent line. Both are orientation-agnostic (`tab_outline` mirrors the
//! geometry), so the horizontal and vertical strips paint through the same two
//! functions.
//!
//! tui-whitelist: pure painting (filled rects and stroked paths); no widget
//! layout calls.

use super::TAB_RADIUS;
use super::TabBarOrientation;
use super::geometry::{tab_accent_edge, tab_outline};

/// Fills one tab's silhouette with a single anti-aliased closed path.
pub(super) fn paint_tab(
    painter: &egui::Painter,
    rect: egui::Rect,
    fill: egui::Color32,
    active: bool,
    orientation: TabBarOrientation,
) {
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: tab_outline(rect, f32::from(TAB_RADIUS), active, orientation),
        closed: true,
        fill,
        stroke: egui::epaint::PathStroke::NONE,
    }));
}

/// Strokes the active tab's accent line (the silhouette minus its open edge).
pub(super) fn paint_active_accent(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    orientation: TabBarOrientation,
    thickness: f32,
) {
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: tab_accent_edge(rect, f32::from(TAB_RADIUS), true, orientation),
        closed: false,
        fill: egui::Color32::TRANSPARENT,
        stroke: egui::epaint::PathStroke::new(thickness, color).inside(),
    }));
}
