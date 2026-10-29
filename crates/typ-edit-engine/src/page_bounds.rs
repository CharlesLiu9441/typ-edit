//! Conservative one-pass bounds for the top-level items of a page.
//!
//! Used by the raster worker's page BVH ([`crate::page_bvh`]) to decide which items
//! can paint inside a sharp render region. Nothing here re-shapes text or re-runs layout: a
//! text run's vertical extent is estimated from its point size and its embedded newline
//! count; its horizontal extent spans from the run's origin to the page edge because the
//! shaped width is unknown without shaping. Shapes use their geometry bounding box, groups
//! use their frame size under the group transform. Every edge is padded by [`PAD`], so the
//! result only ever over-approximates the ink — dropping non-intersecting items is safe.

use typst::layout::{FrameItem, Point};

pub use typ_edit_geom::Extents;

/// Extra padding added to every edge of an item's conservative extent, in points.
pub const PAD: f64 = 2.0;

/// Conservative 2D extents of a single frame item at `pos`.
///
/// Text runs are deliberately full-width: `pos.x .. page_width`. Without re-shaping the shaped
/// advance is unknown, and a narrower guess could drop a run whose ink reaches further right
/// than its origin suggests. `page_width` is only consulted for text.
pub fn item_extents(pos: Point, item: &FrameItem, page_width: f64) -> Extents {
    let x = pos.x.to_pt();
    let y = pos.y.to_pt();
    let (x0, x1, y0, y1) = match item {
        FrameItem::Text(text) => {
            let lines = text.text.matches('\n').count() as f64 + 1.0;
            let height = text.size.to_pt() * lines * 1.3 + 4.0;
            (x, page_width.max(x), y, y + height)
        }
        FrameItem::Shape(shape, _) => {
            let rect = shape.bbox(true);
            (
                x + rect.min.x.to_pt(),
                x + rect.max.x.to_pt(),
                y + rect.min.y.to_pt(),
                y + rect.max.y.to_pt(),
            )
        }
        FrameItem::Group(group) => {
            let size = group.frame.size();
            let t = group.transform;
            let corners = [
                (0.0, 0.0),
                (size.x.to_pt(), 0.0),
                (0.0, size.y.to_pt()),
                (size.x.to_pt(), size.y.to_pt()),
            ];
            let (mut bx0, mut bx1) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut by0, mut by1) = (f64::INFINITY, f64::NEG_INFINITY);
            for (cx, cy) in corners {
                let tx = t.sx.get() * cx + t.kx.get() * cy + t.tx.to_pt();
                let ty = t.ky.get() * cx + t.sy.get() * cy + t.ty.to_pt();
                bx0 = bx0.min(tx);
                bx1 = bx1.max(tx);
                by0 = by0.min(ty);
                by1 = by1.max(ty);
            }
            (x + bx0, x + bx1, y + by0, y + by1)
        }
        FrameItem::Image(_, size, _) | FrameItem::Link(_, size) => {
            (x, x + size.x.to_pt(), y, y + size.y.to_pt())
        }
        FrameItem::Tag(_) => (x, x, y, y),
    };
    Extents {
        x0: x0 - PAD,
        x1: x1 + PAD,
        y0: y0 - PAD,
        y1: y1 + PAD,
    }
}

/// One-pass conservative 2D extents for every item, in page order.
///
/// Accepts borrowed items so callers can stream `Frame::items()` without cloning.
pub fn compute_extents<'a, I>(items: I, page_width: f64) -> Vec<Extents>
where
    I: IntoIterator<Item = &'a (Point, FrameItem)>,
{
    items
        .into_iter()
        .map(|(pos, item)| item_extents(*pos, item, page_width))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extents_intersection_is_closed_and_symmetric() {
        let a = Extents::from_rect(0.0, 0.0, 10.0, 10.0);
        let b = Extents::from_rect(10.0, 10.0, 5.0, 5.0);
        assert!(a.intersects(b));
        assert!(b.intersects(a));
        assert!(!a.intersects(Extents::from_rect(10.01, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn union_covers_both_inputs() {
        let a = Extents::from_rect(0.0, 0.0, 10.0, 5.0);
        let b = Extents::from_rect(-4.0, 8.0, 2.0, 2.0);
        let u = a.union(b);
        assert_eq!(u.x0, -4.0);
        assert_eq!(u.y1, 10.0);
        assert_eq!(u.x1, 10.0);
        assert!(u.intersects(a) && u.intersects(b));
    }

    #[test]
    fn empty_input_yields_no_extents() {
        let items: Vec<&(Point, FrameItem)> = Vec::new();
        assert!(compute_extents(items, 595.0).is_empty());
    }
}
