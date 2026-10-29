//! Typst frame surgery for clipped region renders, plus the render options.
//!
//! `typst-render` always renders a whole page, so a region request is expressed
//! as a synthetic soft frame holding the original (or a culled subset of its)
//! content, translated and clipped to the region rectangle.

use crate::page_bounds::Extents;
use typst::layout::{Abs, Frame, FrameItem, FrameKind, GroupItem, Point, Size, Transform};
use typst::utils::Scalar;
use typst::visualize::Curve;
use typst_layout::Page;
use typst_render::RenderOptions;

/// A page-space region `(x, y, w, h)` in points.
pub(super) type Region = (f32, f32, f32, f32);

/// Clamps the render region to the page so the query never tests empty page space.
pub(super) fn clamped_region(region: Region, page_width: f64, page_height: f64) -> Extents {
    let (x, y, w, h) = region;
    let x0 = (x as f64).max(0.0);
    let y0 = (y as f64).max(0.0);
    let x1 = ((x as f64) + (w as f64)).min(page_width);
    let y1 = ((y as f64) + (h as f64)).min(page_height);
    Extents {
        x0,
        x1: x1.max(x0),
        y0,
        y1: y1.max(y0),
    }
}

/// Builds a page whose frame is the original page shifted and clipped to `region`.
///
/// `typst-render` always renders a whole page, so the region is expressed as a
/// synthetic soft frame holding one group: the original page frame translated by
/// `-region.origin` and clipped to the region rectangle.
pub(super) fn whole_frame_region_page(page: &Page, region: Region) -> Page {
    let (x, y, w, h) = region;
    let size = Size::new(Abs::pt(w as f64), Abs::pt(h as f64));
    let mut frame = Frame::new(size, FrameKind::Soft);

    let mut clip = Curve::rect(size);
    clip.translate(Point::new(Abs::pt(x as f64), Abs::pt(y as f64)));

    let group = GroupItem {
        frame: page.frame.clone(),
        transform: Transform::translate(Abs::pt(-(x as f64)), Abs::pt(-(y as f64))),
        clip: Some(clip),
        label: None,
        parent: None,
    };
    frame.push(Point::zero(), FrameItem::Group(group));

    page_with_frame(page, frame)
}

/// Builds a page drawing only `selected` items at region-relative positions, in frame order.
///
/// One group clipped to the region holds the queried union; item positions are shifted by
/// `-region.origin`, so the group transform is the identity. This is the pixel-exact culled
/// construction from the band-bench experiment (`build_union_page`).
pub(super) fn selected_region_page(page: &Page, selected: &[u32], region: Region) -> Page {
    let (x, y, w, h) = region;
    let (ox, oy) = (Abs::pt(x as f64), Abs::pt(y as f64));
    let size = Size::new(Abs::pt(w as f64), Abs::pt(h as f64));
    let mut frame = Frame::new(size, FrameKind::Soft);

    let mut sub = Frame::new(size, FrameKind::Soft);
    let mut remaining = selected.iter().copied().peekable();
    for (index, (pos, item)) in page.frame.items().enumerate() {
        if remaining.peek() != Some(&(index as u32)) {
            continue;
        }
        remaining.next();
        sub.push(Point::new(pos.x - ox, pos.y - oy), item.clone());
    }

    let group = GroupItem {
        frame: sub,
        transform: Transform::identity(),
        clip: Some(Curve::rect(size)),
        label: None,
        parent: None,
    };
    frame.push(Point::zero(), FrameItem::Group(group));

    page_with_frame(page, frame)
}

/// Rebuilds `page` around a replacement frame, keeping all page metadata.
fn page_with_frame(page: &Page, frame: Frame) -> Page {
    Page {
        frame,
        bleed: page.bleed,
        fill: page.fill.clone(),
        numbering: page.numbering.clone(),
        supplement: page.supplement.clone(),
        number: page.number,
    }
}

/// Builds render options at `pixel_per_pt`, never including bleed.
pub(super) fn options(pixel_per_pt: f32) -> RenderOptions {
    RenderOptions {
        pixel_per_pt: Scalar::new(pixel_per_pt as f64),
        render_bleed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_bvh::PageBvh;
    use crate::raster::compiled_document;

    /// A page whose frame holds four known 20x20 group items, at the given corners.
    fn synthetic_page() -> Page {
        let (_, document) = compiled_document();
        let base = &document.pages()[0];
        let mut frame = Frame::new(base.frame.size(), FrameKind::Soft);
        for (x, y) in [(10.0, 10.0), (200.0, 10.0), (10.0, 300.0), (200.0, 300.0)] {
            frame.push(Point::new(Abs::pt(x), Abs::pt(y)), group_item(20.0, 20.0));
        }
        page_with_frame(base, frame)
    }

    /// An empty group item of `width` x `height` points.
    fn group_item(width: f64, height: f64) -> FrameItem {
        let frame = Frame::new(Size::new(Abs::pt(width), Abs::pt(height)), FrameKind::Soft);
        FrameItem::Group(GroupItem {
            frame,
            transform: Transform::identity(),
            clip: None,
            label: None,
            parent: None,
        })
    }

    /// The single group held by a synthetic region page.
    fn region_group(page: &Page) -> &GroupItem {
        page.frame
            .items()
            .find_map(|(_, item)| match item {
                FrameItem::Group(group) => Some(group),
                _ => None,
            })
            .expect("region page holds one group")
    }

    #[test]
    fn bvh_culled_sub_frame_contains_exactly_the_queried_items_in_order() {
        let page = synthetic_page();
        let page_width = page.frame.size().x.to_pt();
        let extents = crate::page_bounds::compute_extents(page.frame.items(), page_width);
        let bvh = PageBvh::build(&extents);

        // A wide, short region covers the two top items only.
        let query = Extents::from_rect(5.0, 5.0, 395.0, 35.0);
        let selected = bvh.query(query);
        assert_eq!(selected, vec![0, 1]);

        let region = (5.0, 5.0, 395.0, 35.0);
        let synthetic = selected_region_page(&page, &selected, region);
        let positions: Vec<Point> = region_group(&synthetic)
            .frame
            .items()
            .map(|(pos, _)| *pos)
            .collect();
        assert_eq!(
            positions,
            vec![
                Point::new(Abs::pt(5.0), Abs::pt(5.0)),
                Point::new(Abs::pt(195.0), Abs::pt(5.0)),
            ]
        );
    }
}
