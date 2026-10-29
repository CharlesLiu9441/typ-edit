//! Group-clip helpers shared with the page traversal.

use egui::Rect;
use typst_library::visualize::{Curve, CurveItem};

use crate::bounds::Extents;
use crate::view::View;

/// Screen-space bounding rectangle of a group's clip curve.
pub(crate) fn clip_screen_rect(clip: &Curve, view: &View) -> Rect {
    let rect = clip.bbox(None);
    view.to_screen_rect(Extents::from_rect(
        rect.min.x.to_pt(),
        rect.min.y.to_pt(),
        rect.size().x.to_pt(),
        rect.size().y.to_pt(),
    ))
}

/// Whether `curve` is a closed, axis-aligned rectangle (4 line edges).
pub(crate) fn clip_is_axis_aligned_rect(curve: &Curve) -> bool {
    if curve.0.len() != 5 {
        return false;
    }
    let (CurveItem::Move(p0), CurveItem::Line(p1), CurveItem::Line(p2), CurveItem::Line(p3)) =
        (&curve.0[0], &curve.0[1], &curve.0[2], &curve.0[3])
    else {
        return false;
    };
    if !matches!(curve.0[4], CurveItem::Close) {
        return false;
    }
    let points = [p0, p1, p2, p3];
    let (mut horizontal, mut vertical) = (0, 0);
    for i in 0..4 {
        let a = points[i];
        let b = points[(i + 1) % 4];
        let dx = (a.x - b.x).to_pt();
        let dy = (a.y - b.y).to_pt();
        if dx.abs() < 1e-9 && dy.abs() > 1e-9 {
            vertical += 1;
        } else if dy.abs() < 1e-9 && dx.abs() > 1e-9 {
            horizontal += 1;
        } else {
            return false;
        }
    }
    horizontal == 2 && vertical == 2
}
