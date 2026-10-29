//! Frame-local-to-screen mapping for one level of the frame tree.

use egui::{Pos2, Rect};
use typst_library::layout::{GroupItem, Point, Size};

use crate::affine::Affine2;
use crate::bounds::{Extents, ExtentsTransform};
use crate::paint::PaintCtx;

/// A frame-local-to-screen mapping for one level of the frame tree.
pub(crate) struct View {
    /// Maps current-level frame-local points to screen points.
    pub(crate) to_screen: Affine2,
    /// Maps the innermost hard container's local points to screen points.
    pub(crate) container: Affine2,
    /// Size of the innermost hard container in points.
    pub(crate) container_size: (f64, f64),
}

impl View {
    /// The root view for a `size` frame placed at `pos` with uniform `scale`.
    pub(crate) fn root(pos: Pos2, scale: f32, size: Size) -> Self {
        let m = Affine2::uniform_scale_translate(scale as f64, pos.x as f64, pos.y as f64);
        Self {
            to_screen: m,
            container: m,
            container_size: (size.x.to_pt(), size.y.to_pt()),
        }
    }

    /// The view for a subframe whose local points map to screen through `m`.
    ///
    /// A glyph subframe is soft, and `emit_subframe` has no access to the
    /// enclosing text item's hard container, so the subframe itself stands in
    /// as the container and `RelativeTo::Parent` paints resolve against it.
    pub(crate) fn from_transform(m: Affine2, size: Size) -> Self {
        Self {
            to_screen: m,
            container: m,
            container_size: (size.x.to_pt(), size.y.to_pt()),
        }
    }

    /// Maps an extents box to a screen rect, rounding outward so the rect can
    /// only grow. Non-finite boxes over-approximate to "visible": culling must
    /// never drop real content.
    pub(crate) fn to_screen_rect(&self, e: Extents) -> Rect {
        let t = e.transformed(&self.to_screen);
        if !(t.x0.is_finite() && t.x1.is_finite() && t.y0.is_finite() && t.y1.is_finite()) {
            return Rect::EVERYTHING;
        }
        Rect::from_min_max(
            egui::pos2(t.x0.floor() as f32, t.y0.floor() as f32),
            egui::pos2(t.x1.ceil() as f32, t.y1.ceil() as f32),
        )
    }

    /// The view for an item at `item_pos` in this frame's local coords.
    pub(crate) fn item(&self, item_pos: Point) -> View {
        let translate = Affine2::translate(item_pos.x.to_pt(), item_pos.y.to_pt());
        View {
            to_screen: translate.then(self.to_screen),
            container: self.container,
            container_size: self.container_size,
        }
    }

    /// The view for `group` at `item_pos` (in this frame's local coords).
    ///
    /// A hard group becomes the new container; a soft group inherits it.
    pub(crate) fn child(&self, item_pos: Point, group: &GroupItem) -> View {
        let translate = Affine2::translate(item_pos.x.to_pt(), item_pos.y.to_pt());
        // child-local -> parent-local (translate after the group transform) -> screen.
        let m = Affine2::from_typst(&group.transform)
            .then(translate)
            .then(self.to_screen);
        if group.frame.kind().is_hard() {
            View {
                to_screen: m,
                container: m,
                container_size: (group.frame.size().x.to_pt(), group.frame.size().y.to_pt()),
            }
        } else {
            View {
                to_screen: m,
                container: self.container,
                container_size: self.container_size,
            }
        }
    }
}

/// Builds the paint context for an item's view.
pub(crate) fn paint_ctx(view: &View, on_text: bool, pixels_per_point: f32) -> PaintCtx {
    PaintCtx {
        to_screen: view.to_screen,
        container_to_screen: view.container,
        container_size: view.container_size,
        pixels_per_point,
        on_text,
    }
}

/// Quantizes a positive scale to quarter-octave steps.
///
/// Recordings are made at the bucket center (`scale_bucket(scale)`), so the
/// ratio between any live scale in a bucket and its recording is bounded by
/// 2^(±1/8) ≈ ±9%.
pub fn scale_bucket(scale: f32) -> f32 {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    2.0_f32.powf((s.log2() * 4.0).round() / 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_bucket_is_identity_at_one_and_two() {
        assert_eq!(scale_bucket(1.0), 1.0);
        assert_eq!(scale_bucket(2.0), 2.0);
    }

    #[test]
    fn scale_bucket_falls_back_to_one_for_non_positive_or_non_finite() {
        assert_eq!(scale_bucket(0.0), 1.0);
        assert_eq!(scale_bucket(-3.0), 1.0);
        assert_eq!(scale_bucket(f32::NAN), 1.0);
        assert_eq!(scale_bucket(f32::INFINITY), 1.0);
    }

    #[test]
    fn scale_bucket_is_monotonic() {
        let mut previous = scale_bucket(0.01);
        for step in 0..2000 {
            let scale = 0.01 + step as f32 * 0.005;
            let bucket = scale_bucket(scale);
            assert!(
                bucket >= previous,
                "bucket regressed at scale {scale}: {previous} -> {bucket}"
            );
            previous = bucket;
        }
    }

    #[test]
    fn scale_bucket_replay_ratio_error_is_at_most_an_eighth_octave() {
        // 2^(1/8) = 1.0905077...
        let tolerance = 2.0_f32.powf(1.0 / 8.0);
        let mut scale = 0.05_f32;
        while scale <= 40.0 {
            let bucket = scale_bucket(scale);
            let ratio = bucket / scale;
            assert!(
                ratio <= tolerance && ratio >= 1.0 / tolerance,
                "scale {scale} -> bucket {bucket} (ratio {ratio})"
            );
            scale *= 1.01;
        }
    }
}
