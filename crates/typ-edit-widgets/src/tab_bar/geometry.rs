//! Silhouette and outline geometry, tab-width computation and the reorder math
//! for the tab bar. Pure functions over points and rects — no egui state.
//!
//! tui-whitelist: pure geometry (points, arcs, rects); no widget layout calls.

use super::TabBarOrientation;
use super::{TAB_ARC_STEPS, TAB_CLOSE_W, TAB_GAP, TAB_PAD_X, TAB_RADIUS};

/// Width of one tab given the laid-out width of its label: the label budget plus
/// the fixed paddings, the close zone and the two corner radii.
pub(super) fn tab_width(label_width: f32) -> f32 {
    TAB_PAD_X + label_width + TAB_CLOSE_W + TAB_RADIUS as f32 * 2.0
}

/// Total content width of a strip whose tabs have the given widths: the tab sum,
/// the inter-tab gaps and one trailing-edge radius reserve.
pub(super) fn strip_width(widths: &[f32]) -> f32 {
    widths.iter().sum::<f32>() + TAB_GAP * (widths.len() - 1) as f32 + (TAB_RADIUS as f32) * 2.0
}

/// The reserved close-button zone at a tab's trailing edge.
pub(super) fn close_zone(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.max.x - TAB_CLOSE_W - TAB_RADIUS as f32, rect.min.y),
        rect.max - egui::vec2(TAB_RADIUS as f32, 0.0),
    )
}

/// Slot the dragged tab should occupy, given the pointer x and the current
/// rects: slot `i` owns the span between its neighbours' mid-points.
pub(crate) fn reorder_index(rects: &[egui::Rect], pointer_x: f32) -> usize {
    let last = rects.len().saturating_sub(1);
    for (i, rect) in rects.iter().enumerate() {
        let left = if i == 0 {
            f32::NEG_INFINITY
        } else {
            (rects[i - 1].center().x + rect.center().x) * 0.5
        };
        let right = if i == last {
            f32::INFINITY
        } else {
            (rect.center().x + rects[i + 1].center().x) * 0.5
        };
        if pointer_x >= left && pointer_x < right {
            return i;
        }
    }
    last
}

/// Moves `items[from]` into slot `to`, keeping the element order otherwise.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= items.len() || to >= items.len() {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
}

/// Where the element at `index` ends up after `move_item(from, to)`.
pub fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

/// One tab's browser-style silhouette as a single closed path, mirrored between
/// the two orientations (docs/design/ui-cards.md §0.6).
///
/// A horizontal outline starts at the bottom-centre apex; a vertical one at
/// the open-edge apex on the right (`rect.right()`, `rect.center().y`). The
/// corners opposite the open edge are convex `radius` rounds and the open-edge
/// corners are concave fillets flaring `radius` outward (active tabs only;
/// inactive tabs keep square open-edge corners). The open edge itself is
/// implied by `closed: true`. epaint fills a closed path with a triangle fan
/// from `points[0]`; both outlines are star-shaped from their apex, so this
/// exact point order tessellates without overfill.
pub(crate) fn tab_outline(
    rect: egui::Rect,
    radius: f32,
    active: bool,
    orientation: TabBarOrientation,
) -> Vec<egui::Pos2> {
    match orientation {
        TabBarOrientation::Horizontal => {
            use std::f32::consts::{FRAC_PI_2, PI};
            let side_multiplier = if active { 1.0 } else { 0.0 };
            let mut points = Vec::with_capacity(4 * TAB_ARC_STEPS + 5);
            points.push(egui::pos2(rect.center().x, rect.bottom()));
            points.push(egui::pos2(
                rect.right() + radius * side_multiplier,
                rect.bottom(),
            ));
            if active {
                push_arc(
                    &mut points,
                    egui::pos2(rect.right() + radius, rect.bottom() - radius),
                    radius,
                    FRAC_PI_2,
                    PI,
                );
            }
            points.push(egui::pos2(rect.right(), rect.top() + radius));
            push_arc(
                &mut points,
                egui::pos2(rect.right() - radius, rect.top() + radius),
                radius,
                0.0,
                -FRAC_PI_2,
            );
            points.push(egui::pos2(rect.left() + radius, rect.top()));
            push_arc(
                &mut points,
                egui::pos2(rect.left() + radius, rect.top() + radius),
                radius,
                -FRAC_PI_2,
                -PI,
            );
            points.push(egui::pos2(
                rect.left(),
                rect.bottom() - radius * side_multiplier,
            ));
            if active {
                push_arc(
                    &mut points,
                    egui::pos2(rect.left() - radius, rect.bottom() - radius),
                    radius,
                    0.0,
                    FRAC_PI_2,
                );
            }
            points
        }
        TabBarOrientation::Vertical => {
            use std::f32::consts::{FRAC_PI_2, PI};
            let side_multiplier = if active { 1.0 } else { 0.0 };
            let mut points = Vec::with_capacity(4 * TAB_ARC_STEPS + 5);
            points.push(egui::pos2(rect.right(), rect.center().y));
            points.push(egui::pos2(
                rect.right(),
                rect.top() - radius * side_multiplier,
            ));
            if active {
                push_arc(
                    &mut points,
                    egui::pos2(rect.right() - radius, rect.top() - radius),
                    radius,
                    0.0,
                    FRAC_PI_2,
                );
            }
            points.push(egui::pos2(rect.left() + radius, rect.top()));
            push_arc(
                &mut points,
                egui::pos2(rect.left() + radius, rect.top() + radius),
                radius,
                1.5 * PI,
                PI,
            );
            points.push(egui::pos2(rect.left(), rect.bottom() - radius));
            push_arc(
                &mut points,
                egui::pos2(rect.left() + radius, rect.bottom() - radius),
                radius,
                PI,
                FRAC_PI_2,
            );
            points.push(egui::pos2(
                rect.right() - radius * side_multiplier,
                rect.bottom(),
            ));
            if active {
                push_arc(
                    &mut points,
                    egui::pos2(rect.right() - radius, rect.bottom() + radius),
                    radius,
                    -FRAC_PI_2,
                    0.0,
                );
            }
            points
        }
    }
}

/// The active tab's accent line: the whole silhouette except its open edge —
/// for horizontal tabs an open "∩" from the bottom-left flare end, up the left
/// side, over both convex top corners and along the top edge, down the right
/// side to the bottom-right flare end; for vertical tabs the same bracket
/// rotated (from the bottom flare end, along the bottom edge, up the left side
/// and along the top edge to the top flare end).
///
/// Derived from `tab_outline` to guarantee identical geometry: dropping the
/// starting apex leaves the silhouette's open path, and reversing it travels
/// from the first flare end toward the second along the tab's inner side, so
/// `PathStroke::inside()` still strokes the inner side in both orientations.
pub(super) fn tab_accent_edge(
    rect: egui::Rect,
    radius: f32,
    active: bool,
    orientation: TabBarOrientation,
) -> Vec<egui::Pos2> {
    let mut points = tab_outline(rect, radius, active, orientation);
    points.remove(0);
    points.reverse();
    points
}

/// Appends `TAB_ARC_STEPS` samples of the arc from `start` to `end` (radians in
/// screen space, `y` growing downwards), ending on `end`; the arc's start point
/// is expected to be the last already-pushed point.
fn push_arc(points: &mut Vec<egui::Pos2>, center: egui::Pos2, radius: f32, start: f32, end: f32) {
    for step in 1..=TAB_ARC_STEPS {
        let angle = start + (end - start) * step as f32 / TAB_ARC_STEPS as f32;
        points.push(egui::pos2(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tab_bar::TAB_HEIGHT;

    fn three_tab_rects() -> Vec<egui::Rect> {
        (0..3)
            .map(|i| {
                egui::Rect::from_min_size(
                    egui::pos2(i as f32 * 102.0, 0.0),
                    egui::vec2(100.0, 28.0),
                )
            })
            .collect()
    }

    #[test]
    fn reorder_index_picks_the_slot_under_the_pointer() {
        let rects = three_tab_rects();
        // Centres at 50, 152 and 254, so the slot boundaries sit at 101 and 203.
        assert_eq!(reorder_index(&rects, -40.0), 0, "left of the strip");
        assert_eq!(reorder_index(&rects, 50.0), 0);
        assert_eq!(reorder_index(&rects, 100.9), 0, "just before a boundary");
        assert_eq!(
            reorder_index(&rects, 101.0),
            1,
            "a boundary owns the right slot"
        );
        assert_eq!(reorder_index(&rects, 202.9), 1);
        assert_eq!(reorder_index(&rects, 203.0), 2);
        assert_eq!(reorder_index(&rects, 10_000.0), 2, "right of the strip");
        assert_eq!(
            reorder_index(&[], 5.0),
            0,
            "an empty strip falls back to slot 0"
        );
    }

    #[test]
    fn moved_index_follows_the_moved_tab() {
        assert_eq!(
            moved_index(1, 1, 3),
            3,
            "the dragged tab lands in the target slot"
        );
        assert_eq!(moved_index(2, 1, 3), 1, "tabs in between shift left");
        assert_eq!(moved_index(0, 1, 3), 0, "tabs before the move stay put");
        assert_eq!(moved_index(2, 3, 1), 3, "tabs in between shift right");
        assert_eq!(moved_index(3, 1, 1), 3, "a no-op move changes nothing");
    }

    #[test]
    fn move_item_reorders_and_ignores_invalid_moves() {
        let mut items = vec!["a", "b", "c", "d"];
        move_item(&mut items, 0, 2);
        assert_eq!(items, ["b", "c", "a", "d"]);
        move_item(&mut items, 3, 1);
        assert_eq!(items, ["b", "d", "c", "a"]);
        let unchanged = items.clone();
        move_item(&mut items, 1, 1);
        move_item(&mut items, 9, 0);
        move_item(&mut items, 0, 9);
        assert_eq!(items, unchanged, "no-op and out-of-range moves are ignored");
    }

    /// The closed tab silhouettes are filled by epaint as triangle fans from
    /// `points[0]`, so every outline point must be visible from that apex
    /// (docs/design/ui-cards.md §0.6); a hidden point makes the fan overfill.
    fn assert_star_shaped_from_apex(
        rect: egui::Rect,
        orientation: TabBarOrientation,
        expected_apex: egui::Pos2,
    ) {
        fn cross(p: egui::Pos2, q: egui::Pos2, r: egui::Pos2) -> f32 {
            (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
        }

        /// Proper crossing only: collinear or end-touching segments do not count.
        fn crosses(a: (egui::Pos2, egui::Pos2), b: (egui::Pos2, egui::Pos2)) -> bool {
            let (o1, o2) = (cross(a.0, a.1, b.0), cross(a.0, a.1, b.1));
            let (o3, o4) = (cross(b.0, b.1, a.0), cross(b.0, b.1, a.1));
            (o1 > 0.0) != (o2 > 0.0) && (o3 > 0.0) != (o4 > 0.0)
        }

        let outline = tab_outline(rect, f32::from(TAB_RADIUS), true, orientation);
        let apex = outline[0];
        assert!(
            (apex.x - expected_apex.x).abs() < 1e-4 && (apex.y - expected_apex.y).abs() < 1e-4,
            "the {orientation:?} path must start at its apex, got {apex:?}"
        );

        let edge = |i: usize| (outline[i], outline[(i + 1) % outline.len()]);
        for (i, &point) in outline.iter().enumerate().skip(1) {
            for e in 0..outline.len() {
                let (start, end) = (e, (e + 1) % outline.len());
                // Edges sharing the ray's start or end touch it by construction.
                if start == i || end == i || start == 0 || end == 0 {
                    continue;
                }
                assert!(
                    !crosses((apex, point), edge(e)),
                    "ray from the {orientation:?} apex to point {i} crosses edge {e}: the fan would overfill"
                );
            }
        }
    }

    #[test]
    fn tab_outline_is_star_shaped_from_its_bottom_apex() {
        let rect =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(160.0, TAB_HEIGHT));
        assert_star_shaped_from_apex(
            rect,
            TabBarOrientation::Horizontal,
            egui::pos2(rect.center().x, rect.bottom()),
        );
    }

    #[test]
    fn vertical_tab_outline_is_star_shaped_from_its_open_edge_apex() {
        let rect =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(160.0, TAB_HEIGHT));
        assert_star_shaped_from_apex(
            rect,
            TabBarOrientation::Vertical,
            egui::pos2(rect.right(), rect.center().y),
        );
    }

    /// epaint strokes `inside()` by offsetting each point with
    /// `pos -= 0.5 * width * normal`, where `normal = (p1 - p0).normalized().rot90()`
    /// (`rot90 = vec2(y, -x)`): equal winding keeps the accent stroke on the
    /// tab's inner side in both orientations.
    #[test]
    fn accent_paths_of_both_orientations_share_the_inside_stroke_winding() {
        let rect =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(160.0, TAB_HEIGHT));
        let signed_area = |orientation: TabBarOrientation| {
            let mut points = tab_accent_edge(rect, f32::from(TAB_RADIUS), true, orientation);
            points.push(points[0]);
            let mut area = 0.0;
            for pair in points.windows(2) {
                area += pair[0].x * pair[1].y - pair[1].x * pair[0].y;
            }
            area * 0.5
        };
        let horizontal = signed_area(TabBarOrientation::Horizontal);
        let vertical = signed_area(TabBarOrientation::Vertical);
        assert!(
            horizontal.abs() > 1.0,
            "the horizontal accent path must enclose a real area, got {horizontal}"
        );
        assert!(
            vertical.abs() > 1.0,
            "the vertical accent path must enclose a real area, got {vertical}"
        );
        assert!(
            (horizontal > 0.0) == (vertical > 0.0),
            "the accent paths must wind the same way: horizontal {horizontal}, vertical {vertical}"
        );
    }
}
