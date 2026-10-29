//! Pan/zoom state and the "at least one page ≥10 px visible" clamp.
//!
//! Pure geometry — unit tests live at the bottom of this file. The clamp follows
//! `docs/architecture.md` §4:
//!
//! - X: a single interval derived from the widest page, `thx = min(10, zoom * w_max)`.
//! - Y: one interval per page, `I_i = [th_i - zoom*b_i, H - th_i - zoom*t_i]`; when the
//!   offset lands in a gap dead zone, snap to the nearest interval.
//! - Final validation: if no single page has its own `th_i = min(10, zoom*extent_i)`
//!   visible in both axes, snap the offset to the nearest page.

/// Screen position of a page in document space (pt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeom {
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// Vertical page stack layout with gaps.
///
/// Pages are stacked top-to-bottom, all sharing `x = 0`; `gap` is inserted between
/// consecutive pages (not before the first one).
pub fn layout_pages(page_sizes: &[(f32, f32)], gap: f32) -> Vec<PageGeom> {
    let mut top = 0.0f32;
    page_sizes
        .iter()
        .enumerate()
        .map(|(i, &(width, height))| {
            if i > 0 {
                top += gap;
            }
            let geom = PageGeom { top, width, height };
            top += height;
            geom
        })
        .collect()
}

/// Margin (screen px) kept between a followed cursor point and the viewport
/// edge by [`PreviewView::ensure_point_visible`].
pub(crate) const FOLLOW_MARGIN: f32 = 48.0;

/// Pan/zoom transform: `screen = offset + zoom * doc` (all in screen points).
#[derive(Clone, Copy, Debug)]
pub struct PreviewView {
    pub zoom: f32,
    pub offset: egui::Vec2,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: egui::Vec2::new(48.0, 48.0),
        }
    }
}

impl PreviewView {
    pub const MIN_ZOOM: f32 = 0.01;
    pub const MAX_ZOOM: f32 = 100.0;
    /// Minimal visible extent of a page, in screen px.
    pub const MIN_VISIBLE: f32 = 10.0;

    pub fn doc_to_screen(&self, p: egui::Pos2) -> egui::Pos2 {
        (p.to_vec2() * self.zoom + self.offset).to_pos2()
    }

    pub fn screen_to_doc(&self, p: egui::Pos2) -> egui::Pos2 {
        ((p.to_vec2() - self.offset) / self.zoom).to_pos2()
    }

    /// Zooms about a widget-local anchor, then clamps. Never violates the invariant.
    pub fn zoom_about(
        &mut self,
        anchor: egui::Pos2,
        new_zoom: f32,
        viewport: egui::Rect,
        pages: &[PageGeom],
    ) {
        let new_zoom = new_zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let ratio = if self.zoom > 0.0 {
            new_zoom / self.zoom
        } else {
            1.0
        };
        let anchor = anchor.to_vec2();
        // offset' = anchor - (anchor - offset) * (z' / z)
        self.offset = anchor - (anchor - self.offset) * ratio;
        self.zoom = new_zoom;
        self.clamp(viewport, pages);
    }

    /// Clamps pan (and zoom) so that at least one page has ≥10 px visible in both axes
    /// (threshold per page: min(10, zoom*extent)). See docs/architecture.md §4.
    pub fn clamp(&mut self, viewport: egui::Rect, pages: &[PageGeom]) {
        self.zoom = self.zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        if !self.offset.x.is_finite() || !self.offset.y.is_finite() {
            self.offset = egui::Vec2::ZERO;
        }
        if pages.is_empty() {
            return;
        }

        // X: keep the widest page ≥ thx visible horizontally.
        let max_w = pages.iter().map(|p| p.width).fold(0.0f32, f32::max);
        let (x_lo, x_hi) = self.x_interval(viewport, max_w);
        self.offset.x = self.offset.x.clamp(x_lo, x_hi);

        // Y: nearest page interval (handles gap dead zones).
        let mut best: Option<(f32, f32, f32)> = None;
        for page in pages {
            let (lo, hi) = self.y_interval(viewport, page.top, page.height);
            let dist = distance_outside(self.offset.y, lo, hi);
            if best.is_none_or(|(_, _, d)| dist < d) {
                best = Some((lo, hi, dist));
            }
        }
        if let Some((lo, hi, _)) = best {
            self.offset.y = self.offset.y.clamp(lo, hi);
        }

        // Final validation: some single page must satisfy both axes with its own th.
        if !pages.iter().any(|page| self.page_visible(viewport, page)) {
            let mut snap: Option<(f32, f32, f32)> = None;
            for page in pages {
                let (x_lo, x_hi) = self.x_interval(viewport, page.width);
                let (y_lo, y_hi) = self.y_interval(viewport, page.top, page.height);
                let x = self.offset.x.clamp(x_lo, x_hi);
                let y = self.offset.y.clamp(y_lo, y_hi);
                let dist2 = (x - self.offset.x).powi(2) + (y - self.offset.y).powi(2);
                if snap.is_none_or(|(_, _, d)| dist2 < d) {
                    snap = Some((x, y, dist2));
                }
            }
            if let Some((x, y, _)) = snap {
                self.offset = egui::Vec2::new(x, y);
            }
        }
    }

    /// Scrolls minimally so the doc-space `target` is inside the viewport
    /// shrunk by `margin` on each edge, then applies the regular [`Self::clamp`].
    ///
    /// A point already inside leaves the offset untouched; a point outside
    /// shifts the offset by exactly the overshoot per axis, so it lands on the
    /// margin edge. Never changes the zoom.
    pub(crate) fn ensure_point_visible(
        &mut self,
        viewport: egui::Rect,
        pages: &[PageGeom],
        target_doc: egui::Pos2,
        margin: f32,
    ) {
        // `doc_to_screen` is viewport-relative, so the visible area starts at
        // (0, 0) and has the viewport's size.
        let screen = self.doc_to_screen(target_doc);
        let margin = margin
            .max(0.0)
            .min(viewport.width().min(viewport.height()) * 0.4);
        let visible = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport.size()).shrink(margin);
        if screen.x < visible.left() {
            self.offset.x += visible.left() - screen.x;
        } else if screen.x > visible.right() {
            self.offset.x -= screen.x - visible.right();
        }
        if screen.y < visible.top() {
            self.offset.y += visible.top() - screen.y;
        } else if screen.y > visible.bottom() {
            self.offset.y -= screen.y - visible.bottom();
        }
        self.clamp(viewport, pages);
    }

    /// The clamped target offset that would bring `target_doc` inside the
    /// margined viewport, without mutating `self` (used to start the follow tween).
    pub(crate) fn ensure_target_offset(
        &self,
        viewport: egui::Rect,
        pages: &[PageGeom],
        target_doc: egui::Pos2,
        margin: f32,
    ) -> egui::Vec2 {
        let mut probe = *self;
        probe.ensure_point_visible(viewport, pages, target_doc, margin);
        probe.offset
    }

    /// Index of the page containing `doc_y` (or nearest), if any.
    pub fn page_at(&self, pages: &[PageGeom], doc_y: f32) -> Option<usize> {
        if pages.is_empty() {
            return None;
        }
        if let Some(i) = pages
            .iter()
            .position(|p| doc_y >= p.top && doc_y <= p.top + p.height)
        {
            return Some(i);
        }
        pages
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                distance_to_page(doc_y, a).total_cmp(&distance_to_page(doc_y, b))
            })
            .map(|(i, _)| i)
    }

    /// Page rectangle in screen points (viewport-relative `offset` added to `viewport.min`).
    pub(crate) fn page_screen_rect(&self, viewport: egui::Rect, page: &PageGeom) -> egui::Rect {
        let min = viewport.min + self.doc_to_screen(egui::pos2(0.0, page.top)).to_vec2();
        let size = egui::vec2(self.zoom * page.width, self.zoom * page.height);
        egui::Rect::from_min_size(min, size)
    }

    /// `offset.x` interval that keeps a page of `width` (at doc `x = 0`) ≥ th visible.
    fn x_interval(&self, viewport: egui::Rect, width: f32) -> (f32, f32) {
        let th = Self::MIN_VISIBLE.min(self.zoom * width);
        interval(th - self.zoom * width, viewport.width() - th)
    }

    /// `offset.y` interval that keeps a page spanning `[top, top+height]` ≥ th visible.
    fn y_interval(&self, viewport: egui::Rect, top: f32, height: f32) -> (f32, f32) {
        let th = Self::MIN_VISIBLE.min(self.zoom * height);
        interval(
            th - self.zoom * (top + height),
            viewport.height() - th - self.zoom * top,
        )
    }

    /// Whether `page` has ≥ `min(10, zoom*extent)` visible in both axes.
    ///
    /// Equivalent to the screen-rect overlap test (for viewports larger than the
    /// threshold), but computed from the interval bounds so that offsets clamped to
    /// an interval boundary compare exactly instead of suffering f32 cancellation.
    fn page_visible(&self, viewport: egui::Rect, page: &PageGeom) -> bool {
        const EPS: f32 = 1e-3;
        let (x_lo, x_hi) = self.x_interval(viewport, page.width);
        let (y_lo, y_hi) = self.y_interval(viewport, page.top, page.height);
        self.offset.x >= x_lo - EPS
            && self.offset.x <= x_hi + EPS
            && self.offset.y >= y_lo - EPS
            && self.offset.y <= y_hi + EPS
    }

    /// `offset` that puts the doc-space `target` at the viewport centre at `zoom`.
    pub fn centering_offset(
        viewport_size: egui::Vec2,
        zoom: f32,
        target: egui::Pos2,
    ) -> egui::Vec2 {
        viewport_size * 0.5 - target.to_vec2() * zoom
    }

    /// Zoom that makes a page `page_width` pt wide span the full `viewport_width`.
    pub fn fit_width_zoom(viewport_width: f32, page_width: f32) -> f32 {
        if !page_width.is_finite()
            || page_width <= 0.0
            || !viewport_width.is_finite()
            || viewport_width <= 0.0
        {
            return 1.0;
        }
        (viewport_width / page_width).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM)
    }

    /// The reachable `offset` ranges of [`Self::clamp`], one per axis.
    ///
    /// The clamp keeps at least `min(10, zoom*extent)` of a page visible, so the
    /// scrollable travel is wider than `content - viewport` (the pan can go on
    /// until almost the whole page has left the view). The overlay scrollbars
    /// map their tracks onto these ranges, so dragging reaches the exact pan
    /// limits and the handle length reflects the visible share of that travel.
    pub fn scroll_ranges(
        &self,
        viewport: egui::Rect,
        pages: &[PageGeom],
    ) -> (egui::Rangef, egui::Rangef) {
        if pages.is_empty() {
            return (egui::Rangef::new(0.0, 0.0), egui::Rangef::new(0.0, 0.0));
        }
        let max_w = pages.iter().map(|p| p.width).fold(0.0f32, f32::max);
        let (x_lo, x_hi) = self.x_interval(viewport, max_w);
        let mut y_lo = f32::INFINITY;
        let mut y_hi = f32::NEG_INFINITY;
        for page in pages {
            let (lo, hi) = self.y_interval(viewport, page.top, page.height);
            y_lo = y_lo.min(lo);
            y_hi = y_hi.max(hi);
        }
        (egui::Rangef::new(x_lo, x_hi), egui::Rangef::new(y_lo, y_hi))
    }

    /// Widest page width (pt) among pages intersecting `viewport`, if any.
    pub fn widest_visible_width(&self, viewport: egui::Rect, pages: &[PageGeom]) -> Option<f32> {
        pages
            .iter()
            .filter(|page| self.page_screen_rect(viewport, page).intersects(viewport))
            .map(|page| page.width)
            .fold(None, |acc: Option<f32>, width| {
                Some(acc.map_or(width, |best| best.max(width)))
            })
    }

    /// Index of the page nearest the viewport centre (for the page indicator).
    pub fn centered_page(&self, viewport: egui::Rect, pages: &[PageGeom]) -> Option<usize> {
        let center = (viewport.size() * 0.5).to_pos2();
        self.page_at(pages, self.screen_to_doc(center).y)
    }
}

/// Sorted, finite interval; collapses to its midpoint when degenerate.
fn interval(a: f32, b: f32) -> (f32, f32) {
    let a = if a.is_finite() { a } else { 0.0 };
    let b = if b.is_finite() { b } else { 0.0 };
    if a <= b {
        (a, b)
    } else {
        let mid = 0.5 * (a + b);
        (mid, mid)
    }
}

/// Distance of `value` outside `[lo, hi]` (0 when inside).
fn distance_outside(value: f32, lo: f32, hi: f32) -> f32 {
    if value < lo {
        lo - value
    } else if value > hi {
        value - hi
    } else {
        0.0
    }
}

/// Vertical distance from `doc_y` to a page (0 when inside).
fn distance_to_page(doc_y: f32, page: &PageGeom) -> f32 {
    if doc_y < page.top {
        page.top - doc_y
    } else if doc_y > page.top + page.height {
        doc_y - (page.top + page.height)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(w, h))
    }

    #[test]
    fn clamp_ignores_viewport_screen_position() {
        let pages = layout_pages(&[(595.0, 842.0), (595.0, 842.0)], 12.0);
        let mut local = PreviewView {
            zoom: 1.3,
            offset: egui::Vec2::new(-500.0, -900.0),
        };
        let mut shifted = PreviewView {
            zoom: 1.3,
            offset: egui::Vec2::new(-500.0, -900.0),
        };
        local.clamp(viewport(800.0, 600.0), &pages);
        let offscreen =
            egui::Rect::from_min_size(egui::pos2(1200.0, 700.0), egui::Vec2::new(800.0, 600.0));
        shifted.clamp(offscreen, &pages);
        assert_eq!(local.offset, shifted.offset);
        assert_eq!(local.zoom, shifted.zoom);
    }

    #[test]
    fn zoom_about_keeps_anchor_doc_point_fixed() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(50.0, 40.0),
        };
        let anchor = egui::pos2(300.0, 220.0);
        let before = view.screen_to_doc(anchor);
        view.zoom_about(anchor, 2.0, vp, &pages);
        let after = view.screen_to_doc(anchor);
        assert!(
            (before - after).length() < 0.01,
            "anchor drifted: {before:?} -> {after:?}"
        );
    }

    fn a4_pages(n: usize) -> Vec<PageGeom> {
        let sizes = vec![(595.0, 842.0); n];
        layout_pages(&sizes, 12.0)
    }

    /// The invariant from docs §4: SOME page has ≥ its own th visible in both axes.
    fn assert_invariant(view: &PreviewView, vp: egui::Rect, pages: &[PageGeom], case: &str) {
        assert!(
            pages.iter().any(|p| view.page_visible(vp, p)),
            "invariant violated ({case}): view={view:?} viewport={vp:?} pages={pages:?}"
        );
    }

    #[test]
    fn layout_pages_stacks_with_gaps() {
        let pages = layout_pages(&[(100.0, 200.0), (100.0, 300.0), (50.0, 50.0)], 12.0);
        assert_eq!(
            pages,
            vec![
                PageGeom {
                    top: 0.0,
                    width: 100.0,
                    height: 200.0
                },
                PageGeom {
                    top: 212.0,
                    width: 100.0,
                    height: 300.0
                },
                PageGeom {
                    top: 524.0,
                    width: 50.0,
                    height: 50.0
                },
            ]
        );
        assert!(layout_pages(&[], 12.0).is_empty());
    }

    #[test]
    fn pan_is_clamped_in_every_direction() {
        let pages = a4_pages(3);
        let vp = viewport(400.0, 300.0);

        let mut far = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(1e9, 1e9),
        };
        far.clamp(vp, &pages);
        assert!(far.offset.x.is_finite() && far.offset.y.is_finite());
        assert!((far.offset.x - (vp.right() - PreviewView::MIN_VISIBLE)).abs() < 1e-3);
        assert!((far.offset.y - (vp.bottom() - PreviewView::MIN_VISIBLE)).abs() < 1e-3);
        assert_invariant(&far, vp, &pages, "far positive");

        let mut far = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(-1e9, -1e9),
        };
        far.clamp(vp, &pages);
        assert!(far.offset.x.is_finite() && far.offset.y.is_finite());
        // X: the widest page must keep MIN_VISIBLE px inside.
        assert!((far.offset.x - (PreviewView::MIN_VISIBLE - 595.0)).abs() < 1e-3);
        // Y: snapped to the last page interval.
        let last = pages.last().unwrap();
        assert!(
            (far.offset.y - (PreviewView::MIN_VISIBLE - (last.top + last.height))).abs() < 1e-3
        );
        assert_invariant(&far, vp, &pages, "far negative");

        // Mixed directions clamp independently.
        let mut mixed = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(1e9, -1e9),
        };
        mixed.clamp(vp, &pages);
        assert!(mixed.offset.x > 0.0 && mixed.offset.y < 0.0);
        assert_invariant(&mixed, vp, &pages, "mixed");
    }

    #[test]
    fn zoom_out_keeps_page_fully_visible() {
        let pages = layout_pages(&[(595.0, 842.0)], 12.0);
        let vp = viewport(400.0, 300.0);
        let mut view = PreviewView {
            zoom: 0.01,
            offset: egui::Vec2::new(-1000.0, 1000.0),
        };
        view.clamp(vp, &pages);

        let page = view.page_screen_rect(vp, &pages[0]);
        let eps = 1e-3;
        assert!(page.left() >= vp.left() - eps, "left {:?}", page);
        assert!(page.top() >= vp.top() - eps, "top {:?}", page);
        assert!(page.right() <= vp.right() + eps, "right {:?}", page);
        assert!(page.bottom() <= vp.bottom() + eps, "bottom {:?}", page);
        assert_invariant(&view, vp, &pages, "zoom 0.01");
    }

    #[test]
    fn huge_zoom_gap_dead_zone_snaps_to_page() {
        // At zoom 100 the viewport falls fully inside the 12 pt gap between pages:
        // no page is visible, so the clamp must snap to the nearest page interval.
        let pages = a4_pages(2);
        let vp = viewport(400.0, 300.0);
        let mut view = PreviewView {
            zoom: 100.0,
            offset: egui::Vec2::new(0.0, -85_000.0),
        };
        view.clamp(vp, &pages);

        assert_invariant(&view, vp, &pages, "huge zoom gap");
        assert!(
            !view.page_visible(vp, &pages[0]),
            "page 0 should stay above the viewport"
        );
        assert!(
            view.page_visible(vp, &pages[1]),
            "page 1 should become visible"
        );
        // Snapped to the upper edge of page 1's interval: its top ends up exactly
        // MIN_VISIBLE px above the viewport bottom.
        let expected = vp.bottom() - PreviewView::MIN_VISIBLE - pages[1].top * 100.0;
        assert!(
            (view.offset.y - expected).abs() < 1e-2,
            "offset.y={} expected={expected}",
            view.offset.y
        );
    }

    #[test]
    fn zoom_about_keeps_anchor_point() {
        let vp = viewport(800.0, 600.0);
        let anchor = egui::pos2(321.0, 123.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(48.0, 48.0),
        };
        let doc_before = view.screen_to_doc(anchor);

        view.zoom_about(anchor, 2.5, vp, &[]);
        assert!((view.zoom - 2.5).abs() < 1e-6);

        let screen_after = view.doc_to_screen(doc_before);
        assert!((screen_after.x - anchor.x).abs() < 1e-3, "{screen_after:?}");
        assert!((screen_after.y - anchor.y).abs() < 1e-3, "{screen_after:?}");

        // Zoom clamps to [MIN_ZOOM, MAX_ZOOM].
        view.zoom_about(anchor, 1e9, vp, &[]);
        assert!((view.zoom - PreviewView::MAX_ZOOM).abs() < 1e-6);
        view.zoom_about(anchor, 0.0, vp, &[]);
        assert!((view.zoom - PreviewView::MIN_ZOOM).abs() < 1e-6);
    }

    #[test]
    fn page_at_finds_containing_and_nearest_page() {
        let pages = layout_pages(&[(100.0, 200.0), (100.0, 300.0)], 12.0);
        let view = PreviewView::default();
        assert_eq!(view.page_at(&pages, 0.0), Some(0));
        assert_eq!(view.page_at(&pages, 199.0), Some(0));
        assert_eq!(view.page_at(&pages, 212.0), Some(1));
        assert_eq!(view.page_at(&pages, 300.0), Some(1));
        // Gap (200..212): nearest by distance — both are 6 pt away, first wins.
        assert!(matches!(view.page_at(&pages, 206.0), Some(0 | 1)));
        // Outside: nearest page.
        assert_eq!(view.page_at(&pages, -50.0), Some(0));
        assert_eq!(view.page_at(&pages, 900.0), Some(1));
        assert_eq!(view.page_at(&[], 0.0), None);
    }

    #[test]
    fn randomized_clamp_keeps_invariant() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
            fn f32(&mut self, lo: f32, hi: f32) -> f32 {
                let t = (self.next() >> 40) as f32 / (1u64 << 24) as f32;
                lo + (hi - lo) * t
            }
        }

        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for case in 0..5000 {
            let n = 1 + (rng.next() % 5) as usize;
            let sizes: Vec<(f32, f32)> = (0..n)
                .map(|_| (rng.f32(50.0, 900.0), rng.f32(50.0, 1200.0)))
                .collect();
            let pages = layout_pages(&sizes, 12.0);
            let vp = viewport(rng.f32(50.0, 1600.0), rng.f32(50.0, 1200.0));
            let mut view = PreviewView {
                zoom: rng.f32(0.01, 100.0),
                offset: egui::Vec2::new(rng.f32(-1e6, 1e6), rng.f32(-1e6, 1e6)),
            };
            view.clamp(vp, &pages);
            assert!(
                view.offset.x.is_finite() && view.offset.y.is_finite(),
                "case {case}"
            );
            assert_invariant(&view, vp, &pages, &format!("random case {case}"));
        }
    }

    #[test]
    fn centering_offset_places_the_target_at_the_viewport_center() {
        let size = egui::Vec2::new(800.0, 600.0);
        let zoom = 2.0;
        let target = egui::pos2(100.0, 200.0);

        let offset = PreviewView::centering_offset(size, zoom, target);
        let view = PreviewView { zoom, offset };
        let screen = view.doc_to_screen(target);

        assert!((screen.x - size.x / 2.0).abs() < 1e-4, "{screen:?}");
        assert!((screen.y - size.y / 2.0).abs() < 1e-4, "{screen:?}");
    }

    #[test]
    fn fit_width_zoom_matches_the_page_width_to_the_viewport() {
        assert!((PreviewView::fit_width_zoom(800.0, 400.0) - 2.0).abs() < 1e-6);
        assert!((PreviewView::fit_width_zoom(100.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((PreviewView::fit_width_zoom(1.0, 100000.0) - PreviewView::MIN_ZOOM).abs() < 1e-6);
        assert!(
            (PreviewView::fit_width_zoom(100000.0, 100.0) - PreviewView::MAX_ZOOM).abs() < 1e-6
        );
    }

    #[test]
    fn widest_visible_width_ignores_offscreen_pages() {
        let pages = layout_pages(&[(300.0, 400.0), (800.0, 400.0)], 12.0);
        let vp = viewport(400.0, 300.0);

        let top = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        assert_eq!(
            PreviewView::widest_visible_width(&top, vp, &pages),
            Some(300.0)
        );

        let scrolled = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(0.0, -412.0),
        };
        assert_eq!(
            PreviewView::widest_visible_width(&scrolled, vp, &pages),
            Some(800.0)
        );
    }

    #[test]
    fn centered_page_tracks_the_page_at_the_viewport_center() {
        let pages = layout_pages(&[(100.0, 200.0), (100.0, 200.0)], 12.0);
        let vp = viewport(400.0, 400.0);

        let lower = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(0.0, -150.0),
        };
        assert_eq!(lower.centered_page(vp, &pages), Some(1));

        let upper = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(0.0, 50.0),
        };
        assert_eq!(upper.centered_page(vp, &pages), Some(0));
        assert_eq!(upper.centered_page(vp, &[]), None);
    }

    #[test]
    fn scroll_ranges_cover_the_clamp_limits() {
        let view = PreviewView::default();
        let vp = viewport(300.0, 100.0);
        let pages = [PageGeom {
            top: 0.0,
            width: 100.0,
            height: 200.0,
        }];
        let (x, y) = view.scroll_ranges(vp, &pages);
        // X: th = min(10, zoom * 100) = 10 -> [10 - 100, 300 - 10].
        assert_eq!((x.min, x.max), (-90.0, 290.0));
        // Y: th = min(10, zoom * 200) = 10 -> [10 - 200, 100 - 10].
        assert_eq!((y.min, y.max), (-190.0, 90.0));
        assert_eq!(
            view.scroll_ranges(vp, &[]),
            (egui::Rangef::new(0.0, 0.0), egui::Rangef::new(0.0, 0.0))
        );
    }

    #[test]
    fn ensure_point_visible_leaves_an_inside_point_untouched() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::new(48.0, 48.0),
        };
        let before = view.offset;

        view.ensure_point_visible(vp, &pages, egui::pos2(100.0, 100.0), FOLLOW_MARGIN);

        assert_eq!(view.offset, before, "an already visible point must not pan");
        assert_eq!(view.zoom, 1.0);
    }

    #[test]
    fn ensure_point_visible_lands_a_far_point_on_the_margin_edge() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        let target = egui::pos2(900.0, 700.0);

        view.ensure_point_visible(vp, &pages, target, FOLLOW_MARGIN);

        let visible = egui::Rect::from_min_size(egui::Pos2::ZERO, vp.size()).shrink(FOLLOW_MARGIN);
        let screen = view.doc_to_screen(target);
        assert!((screen.x - visible.right()).abs() < 1e-4, "{screen:?}");
        assert!((screen.y - visible.bottom()).abs() < 1e-4, "{screen:?}");
    }

    #[test]
    fn ensure_point_visible_lands_a_near_origin_point_symmetrically() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        let target = egui::pos2(-200.0, -100.0);

        view.ensure_point_visible(vp, &pages, target, FOLLOW_MARGIN);

        let screen = view.doc_to_screen(target);
        assert!((screen.x - FOLLOW_MARGIN).abs() < 1e-4, "{screen:?}");
        assert!((screen.y - FOLLOW_MARGIN).abs() < 1e-4, "{screen:?}");
    }

    #[test]
    fn ensure_point_visible_still_respects_the_clamp() {
        let pages = layout_pages(&[(100.0, 100.0)], 12.0);
        let vp = viewport(800.0, 600.0);
        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };

        view.ensure_point_visible(vp, &pages, egui::pos2(10_000.0, 10_000.0), FOLLOW_MARGIN);

        // The shift alone would push the page out of view; the clamp stops at
        // the ≥10 px visible limit: th - zoom*extent = 10 - 100.
        assert!((view.offset.x + 90.0).abs() < 1e-3, "{:?}", view.offset);
        assert!((view.offset.y + 90.0).abs() < 1e-3, "{:?}", view.offset);
    }

    #[test]
    fn ensure_point_visible_clamps_a_large_margin_on_a_tiny_viewport() {
        let pages = a4_pages(1);
        let vp = viewport(10.0, 10.0);
        let margin = FOLLOW_MARGIN.min(vp.width().min(vp.height()) * 0.4);
        let visible = egui::Rect::from_min_size(egui::Pos2::ZERO, vp.size()).shrink(margin);
        assert!(visible.width() > 0.0 && visible.height() > 0.0);

        let mut view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        view.ensure_point_visible(vp, &pages, egui::pos2(100.0, 100.0), FOLLOW_MARGIN);

        let screen = view.doc_to_screen(egui::pos2(100.0, 100.0));
        assert!((screen.x - visible.right()).abs() < 1e-4, "{screen:?}");
        assert!((screen.y - visible.bottom()).abs() < 1e-4, "{screen:?}");
    }

    #[test]
    fn ensure_target_offset_lands_a_right_overflow_point_on_the_right_margin() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        let target = egui::pos2(900.0, 300.0);

        let offset = view.ensure_target_offset(vp, &pages, target, FOLLOW_MARGIN);

        // Horizontal follow: the offset moves left and the point lands exactly on
        // the right margin; the vertical axis stays put (the point is inside it).
        assert!(offset.x < view.offset.x, "{offset:?}");
        assert!((offset.y - view.offset.y).abs() < 1e-3, "{offset:?}");
        let visible = egui::Rect::from_min_size(egui::Pos2::ZERO, vp.size()).shrink(FOLLOW_MARGIN);
        let probe = PreviewView {
            zoom: view.zoom,
            offset,
        };
        let screen = probe.doc_to_screen(target);
        assert!((screen.x - visible.right()).abs() < 1e-3, "{screen:?}");
        assert_eq!(view.offset, egui::Vec2::ZERO, "probe must not mutate self");
    }

    #[test]
    fn ensure_target_offset_lands_a_left_overflow_point_on_the_left_margin() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };
        let target = egui::pos2(-200.0, 300.0);

        let offset = view.ensure_target_offset(vp, &pages, target, FOLLOW_MARGIN);

        // Horizontal follow in the other direction: the offset moves right and the
        // point lands exactly on the left margin.
        assert!(offset.x > view.offset.x, "{offset:?}");
        assert!((offset.y - view.offset.y).abs() < 1e-3, "{offset:?}");
        let visible = egui::Rect::from_min_size(egui::Pos2::ZERO, vp.size()).shrink(FOLLOW_MARGIN);
        let probe = PreviewView {
            zoom: view.zoom,
            offset,
        };
        let screen = probe.doc_to_screen(target);
        assert!((screen.x - visible.left()).abs() < 1e-3, "{screen:?}");
    }

    #[test]
    fn ensure_target_offset_leaves_an_inside_point_untouched() {
        let pages = a4_pages(1);
        let vp = viewport(800.0, 600.0);
        let view = PreviewView {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
        };

        let offset = view.ensure_target_offset(vp, &pages, egui::pos2(300.0, 300.0), FOLLOW_MARGIN);

        assert_eq!(offset, view.offset);
        assert_eq!(view.offset, egui::Vec2::ZERO);
    }
}
