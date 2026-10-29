//! Conservative per-item bounds in frame-local points.
//!
//! Every item is turned into an axis-aligned [`Extents`] box that is guaranteed
//! to contain all ink the item can paint. The boxes deliberately over-approximate
//! (they may be larger than the real ink) so that culling by intersection can
//! never drop a visible item.

use typst_library::layout::{FrameItem, Point};

use crate::affine::Affine2;

pub use typ_edit_geom::Extents;

/// Extra padding added to every item extent, in points.
///
/// This absorbs rounding error when a group transform or the final
/// frame-to-screen mapping is applied, so the box can only ever grow.
pub(crate) const EXTENT_PAD_PT: f64 = 1.0;

/// Maps an [`Extents`] box through a 2D affine transform.
///
/// This lives here, not on [`Extents`], because [`Affine2`] is a GPU-canvas type:
/// the shared box in `typ-edit-geom` stays standard-library-only.
pub(crate) trait ExtentsTransform {
    /// The axis-aligned box covering the four transformed corners.
    fn transformed(self, m: &Affine2) -> Extents;
}

impl ExtentsTransform for Extents {
    fn transformed(self, m: &Affine2) -> Extents {
        let corners = [
            (self.x0, self.y0),
            (self.x1, self.y0),
            (self.x0, self.y1),
            (self.x1, self.y1),
        ];
        let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
        for (x, y) in corners {
            let (tx, ty) = m.map(x, y);
            x0 = x0.min(tx);
            x1 = x1.max(tx);
            y0 = y0.min(ty);
            y1 = y1.max(ty);
        }
        Extents { x0, x1, y0, y1 }
    }
}

/// Conservative extents of one item at `pos` in the parent frame's local coords.
///
/// The result is **not** padded; callers add [`EXTENT_PAD_PT`]. `child_bounds`
/// must be `Some` for [`FrameItem::Group`] (the already-built child tree's
/// bounds). Passing the bounds rather than the child tree keeps this module
/// independent of [`crate::bvh`].
pub(crate) fn item_extents(pos: Point, item: &FrameItem, child_bounds: Option<Extents>) -> Extents {
    let x = pos.x.to_pt();
    let y = pos.y.to_pt();
    match item {
        FrameItem::Text(text) => {
            let rect = text.bbox();
            let (mut x0, mut x1, mut y0, mut y1) = normalized_text_rect(
                rect.min.x.to_pt(),
                rect.max.x.to_pt(),
                rect.min.y.to_pt(),
                rect.max.y.to_pt(),
                text.width().to_pt(),
                text.height().to_pt(),
            );
            if let Some(stroke) = &text.stroke {
                let extra = stroke.thickness.to_pt();
                x0 -= extra;
                x1 += extra;
                y0 -= extra;
                y1 += extra;
            }
            Extents {
                x0: x + x0,
                x1: x + x1,
                y0: y + y0,
                y1: y + y1,
            }
        }
        FrameItem::Shape(shape, _) => {
            let rect = shape.bbox(true);
            Extents {
                x0: x + rect.min.x.to_pt(),
                x1: x + rect.max.x.to_pt(),
                y0: y + rect.min.y.to_pt(),
                y1: y + rect.max.y.to_pt(),
            }
        }
        FrameItem::Image(_, size, _) | FrameItem::Link(_, size) => {
            Extents::from_rect(x, y, size.x.to_pt(), size.y.to_pt())
        }
        FrameItem::Tag(_) => Extents::from_rect(x, y, 0.0, 0.0),
        FrameItem::Group(group) => {
            // Maps child-local coords into the parent frame's local coords:
            // translate by `pos` after the group transform.
            let m = Affine2::from_typst(&group.transform).then(Affine2::translate(x, y));
            let Some(bounds) = child_bounds else {
                return Extents::from_rect(x, y, 0.0, 0.0);
            };
            let content = bounds.transformed(&m);
            match &group.clip {
                Some(clip) => {
                    let rect = clip.bbox(None);
                    let clip_box = Extents::from_rect(
                        rect.min.x.to_pt(),
                        rect.min.y.to_pt(),
                        rect.size().x.to_pt(),
                        rect.size().y.to_pt(),
                    )
                    .transformed(&m);
                    content.intersection(clip_box)
                }
                None => content,
            }
        }
    }
}

/// Normalizes a text run's `bbox` into ordered, finite edges.
///
/// `TextItem::bbox` mirrors y without reordering, so runs with outlined glyphs
/// come back inverted; runs whose glyphs all lack outlines (e.g. spaces) come
/// back fully degenerate (`min = +inf`, `max = -inf`). Inverted edges are
/// reordered, non-finite ones fall back to the run's advance box.
fn normalized_text_rect(
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    advance_w: f64,
    advance_h: f64,
) -> (f64, f64, f64, f64) {
    if !x0.is_finite() || !x1.is_finite() || !y0.is_finite() || !y1.is_finite() {
        return (0.0, advance_w.max(0.0), 0.0, advance_h.max(0.0));
    }
    (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1))
}

/// Convenience: builds the conservative, padded extents of one item.
#[cfg(test)]
fn padded_extents(pos: Point, item: &FrameItem, child_bounds: Option<Extents>) -> Extents {
    item_extents(pos, item, child_bounds).pad(EXTENT_PAD_PT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bvh::FrameBvh;
    use typst_library::layout::{Abs, Frame, FrameItem, GroupItem, Size, Transform};
    use typst_library::visualize::{Color, Curve, Geometry};
    use typst_syntax::Span;

    fn rect_item(w: f64, h: f64) -> FrameItem {
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(w), Abs::pt(h))).filled(Color::RED),
            Span::detached(),
        )
    }

    fn shape_frame(w: f64, h: f64, x: f64, y: f64) -> Frame {
        let mut frame = Frame::soft(Size::new(Abs::pt(w), Abs::pt(h)));
        frame.push(Point::new(Abs::pt(x), Abs::pt(y)), rect_item(w, h));
        frame
    }

    #[test]
    fn shape_rect_extents_include_stroke_and_pad() {
        let item = rect_item(10.0, 20.0);
        let e = padded_extents(Point::new(Abs::pt(5.0), Abs::pt(7.0)), &item, None);
        assert_eq!(
            e,
            Extents {
                x0: 4.0,
                x1: 16.0,
                y0: 6.0,
                y1: 28.0
            }
        );
    }

    #[test]
    fn negative_size_rect_extents_cover_the_full_rect() {
        let item = FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(-10.0), Abs::pt(-20.0))).filled(Color::RED),
            Span::detached(),
        );
        // No pad here so the raw geometry box is visible.
        let e = item_extents(Point::new(Abs::pt(100.0), Abs::pt(50.0)), &item, None);
        assert_eq!(
            e,
            Extents {
                x0: 90.0,
                x1: 100.0,
                y0: 30.0,
                y1: 50.0
            }
        );
    }

    #[test]
    fn group_translate_and_scale_transform_content() {
        let child = FrameBvh::new(shape_frame(10.0, 10.0, 0.0, 0.0));
        let child_bounds = child.bounds();
        assert_eq!(
            child_bounds,
            Extents {
                x0: -1.0,
                x1: 11.0,
                y0: -1.0,
                y1: 11.0
            }
        );

        let mut group = GroupItem::new(Frame::soft(Size::new(Abs::pt(10.0), Abs::pt(10.0))));
        group.transform = Transform::translate(Abs::pt(20.0), Abs::pt(30.0));
        let item = FrameItem::Group(group);

        let e = item_extents(
            Point::new(Abs::pt(2.0), Abs::pt(3.0)),
            &item,
            Some(child_bounds),
        );
        // translate(20,30) then translate(2,3): content moves by (22, 33).
        assert_eq!(e, child_bounds.translate(22.0, 33.0));
    }

    #[test]
    fn group_scale_transform_scales_content_box() {
        let child = FrameBvh::new(shape_frame(10.0, 10.0, 0.0, 0.0));
        let child_bounds = child.bounds();
        let mut group = GroupItem::new(Frame::soft(Size::new(Abs::pt(10.0), Abs::pt(10.0))));
        group.transform = typst_library::layout::Transform::scale(
            typst_library::layout::Ratio::new(2.0),
            typst_library::layout::Ratio::new(3.0),
        );
        let item = FrameItem::Group(group);

        let e = item_extents(
            Point::new(Abs::pt(0.0), Abs::pt(0.0)),
            &item,
            Some(child_bounds),
        );
        assert_eq!(
            e,
            Extents {
                x0: -2.0,
                x1: 22.0,
                y0: -3.0,
                y1: 33.0
            }
        );
    }

    #[test]
    fn group_clip_intersects_content() {
        let child = FrameBvh::new(shape_frame(10.0, 10.0, 0.0, 0.0));
        let child_bounds = child.bounds();
        let mut group = GroupItem::new(Frame::soft(Size::new(Abs::pt(10.0), Abs::pt(10.0))));
        group.clip = Some(Curve::rect(Size::new(Abs::pt(5.0), Abs::pt(5.0))));
        let item = FrameItem::Group(group);

        let e = item_extents(
            Point::new(Abs::pt(0.0), Abs::pt(0.0)),
            &item,
            Some(child_bounds),
        );
        assert_eq!(
            e,
            Extents {
                x0: 0.0,
                x1: 5.0,
                y0: 0.0,
                y1: 5.0
            }
        );
    }

    #[test]
    fn normalized_text_rect_orders_inverted_edges_and_rejects_degenerate_boxes() {
        assert_eq!(
            normalized_text_rect(0.0, 10.0, 9.0, 2.0, 12.0, 3.0),
            (0.0, 10.0, 2.0, 9.0)
        );
        assert_eq!(
            normalized_text_rect(
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                12.0,
                0.0
            ),
            (0.0, 12.0, 0.0, 0.0)
        );
    }

    #[test]
    fn intersection_of_disjoint_boxes_collapses_to_point() {
        let a = Extents::from_rect(0.0, 0.0, 1.0, 1.0);
        let b = Extents::from_rect(10.0, 10.0, 1.0, 1.0);
        let i = a.intersection(b);
        assert_eq!(i.width(), 0.0);
        assert_eq!(i.height(), 0.0);
    }

    #[test]
    fn transformed_box_covers_all_corner_images() {
        let e = Extents::from_rect(0.0, 0.0, 10.0, 20.0);
        let m = Affine2 {
            sx: 1.0,
            ky: 0.0,
            kx: 0.5,
            sy: 1.0,
            tx: 0.0,
            ty: 0.0,
        };
        // Skew: x' = x + 0.5y, so right edge reaches 10 + 10 = 20.
        let t = e.transformed(&m);
        assert_eq!(
            t,
            Extents {
                x0: 0.0,
                x1: 20.0,
                y0: 0.0,
                y1: 20.0
            }
        );
    }
}
