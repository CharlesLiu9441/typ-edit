//! Pure geometry for the raster preview: visible rect, region expansion, coverage
//! and the pixel-budget clamp. Unit tests live at the bottom of this file.

use super::view::{PageGeom, PreviewView};

/// A page-local rectangle in typographic points: `(x, y, w, h)`.
pub type Region = (f32, f32, f32, f32);

/// The rendered region is this many times the viewport on each axis, so small pans
/// stay sharp without a re-render.
pub const REGION_EXPAND: f32 = 1.5;

/// Smallest region edge, in page points.
const MIN_REGION_PT: f32 = 1.0;

/// Lower bound for a derived pixels-per-point value.
const MIN_PIXEL_PER_PT: f32 = 1e-3;

/// Tolerance for the coverage comparison, in points.
const COVER_EPS: f32 = 0.05;

/// Relative tolerance for the scale comparison.
const SCALE_TOL: f32 = 0.02;

/// The page-local rect visible through `viewport`, clamped to the page.
///
/// Returns `None` when the page has no visible area.
pub fn visible_rect(view: &PreviewView, viewport: egui::Rect, page: &PageGeom) -> Option<Region> {
    if !view.zoom.is_finite() || view.zoom <= 0.0 {
        return None;
    }
    let screen = view.page_screen_rect(viewport, page);
    let visible = screen.intersect(viewport);
    let width = visible.width();
    let height = visible.height();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }

    let zoom = view.zoom;
    let x0 = ((visible.left() - screen.left()) / zoom).clamp(0.0, page.width);
    let y0 = ((visible.top() - screen.top()) / zoom).clamp(0.0, page.height);
    let x1 = ((visible.right() - screen.left()) / zoom).clamp(0.0, page.width);
    let y1 = ((visible.bottom() - screen.top()) / zoom).clamp(0.0, page.height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// Expands `visible` by [`REGION_EXPAND`] per axis, centred and clamped to the page.
pub fn expand_region(visible: Region, page: (f32, f32)) -> Region {
    let (x, y, w, h) = visible;
    let (page_w, page_h) = page;
    let min_w = MIN_REGION_PT.min(page_w);
    let min_h = MIN_REGION_PT.min(page_h);
    let want_w = (w * REGION_EXPAND).clamp(min_w, page_w);
    let want_h = (h * REGION_EXPAND).clamp(min_h, page_h);
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let rx = (cx - want_w / 2.0).clamp(0.0, (page_w - want_w).max(0.0));
    let ry = (cy - want_h / 2.0).clamp(0.0, (page_h - want_h).max(0.0));
    (rx, ry, want_w, want_h)
}

/// Whether `region` fully contains `visible` (within [`COVER_EPS`]).
pub fn covers(region: Region, visible: Region) -> bool {
    let (rx, ry, rw, rh) = region;
    let (vx, vy, vw, vh) = visible;
    vx >= rx - COVER_EPS
        && vy >= ry - COVER_EPS
        && vx + vw <= rx + rw + COVER_EPS
        && vy + vh <= ry + rh + COVER_EPS
}

/// Whether two pixels-per-point values are close enough to reuse a texture.
pub fn scale_matches(a: f32, b: f32) -> bool {
    (a - b).abs() <= SCALE_TOL * a.abs().max(b.abs()).max(1.0)
}

/// Clamps `wanted` pixels-per-point so `region` rasterizes within `max_side` pixels.
pub fn clamp_pixel_per_pt(region: Region, wanted: f32, max_side: u32) -> f32 {
    let max_dim = region.2.max(region.3).max(MIN_REGION_PT);
    let limit = max_side.max(1) as f32 / max_dim;
    wanted.min(limit).max(MIN_PIXEL_PER_PT)
}

/// Whole-page scale for the fallback: `dpi / 72`, capped so a page that is small on screen
/// (zoomed out) never allocates more pixels than it can show. The view cap is quantized up to a
/// power of two, so small zoom changes reuse the same fallback texture instead of re-rendering.
pub fn fallback_pixel_per_pt(page: (f32, f32), dpi: f32, max_side: u32, view_scale: f32) -> f32 {
    let view_cap = view_scale.max(MIN_PIXEL_PER_PT).log2().ceil().exp2();
    let wanted = (dpi / 72.0).max(MIN_PIXEL_PER_PT).min(view_cap);
    clamp_pixel_per_pt((0.0, 0.0, page.0, page.1), wanted, max_side)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(zoom: f32, offset: (f32, f32)) -> PreviewView {
        PreviewView {
            zoom,
            offset: egui::Vec2::new(offset.0, offset.1),
        }
    }

    fn page() -> PageGeom {
        PageGeom {
            top: 0.0,
            width: 100.0,
            height: 200.0,
        }
    }

    fn viewport(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(w, h))
    }

    #[test]
    fn visible_rect_covers_the_whole_page_when_it_fits() {
        // Given: zoom 2, no offset -> a 200x400 screen-page inside a 400x400 viewport.
        let vp = viewport(400.0, 400.0);
        // When/Then: the visible page-local rect is the whole page.
        assert_eq!(
            visible_rect(&view(2.0, (0.0, 0.0)), vp, &page()),
            Some((0.0, 0.0, 100.0, 200.0))
        );
    }

    #[test]
    fn visible_rect_is_clipped_by_a_smaller_viewport() {
        // Given: zoom 2 and a 100x100 viewport -> 50x50 page points are visible.
        let vp = viewport(100.0, 100.0);
        // When/Then: only the top-left quarter of the page is visible.
        assert_eq!(
            visible_rect(&view(2.0, (0.0, 0.0)), vp, &page()),
            Some((0.0, 0.0, 50.0, 50.0))
        );
    }

    #[test]
    fn visible_rect_is_none_when_the_page_is_offscreen() {
        let vp = viewport(400.0, 400.0);
        // The page starts at screen x = -1000 with a 200 px width.
        assert_eq!(visible_rect(&view(2.0, (-1000.0, 0.0)), vp, &page()), None);
    }

    #[test]
    fn expand_region_centres_and_clamps_at_a_corner() {
        // Given: a 50x50 visible corner at the page origin.
        // When/Then: the 75x75 region is anchored to the origin instead of sticking out.
        assert_eq!(
            expand_region((0.0, 0.0, 50.0, 50.0), (100.0, 200.0)),
            (0.0, 0.0, 75.0, 75.0)
        );
    }

    #[test]
    fn expand_region_is_centred_in_the_middle() {
        assert_eq!(
            expand_region((25.0, 25.0, 50.0, 50.0), (100.0, 200.0)),
            (12.5, 12.5, 75.0, 75.0)
        );
    }

    #[test]
    fn expand_region_clamps_to_the_far_edge() {
        // Near the right edge the 37.5-wide region is clamped to x = 100 - 37.5.
        let (x, _, w, _) = expand_region((75.0, 0.0, 25.0, 100.0), (100.0, 200.0));
        assert!((x - 62.5).abs() < 1e-4, "x = {x}");
        assert!((w - 37.5).abs() < 1e-4, "w = {w}");
    }

    #[test]
    fn expand_region_of_the_whole_page_is_the_page() {
        assert_eq!(
            expand_region((0.0, 0.0, 100.0, 200.0), (100.0, 200.0)),
            (0.0, 0.0, 100.0, 200.0)
        );
    }

    #[test]
    fn covers_accepts_a_region_that_contains_the_visible_rect() {
        assert!(covers((10.0, 10.0, 80.0, 80.0), (20.0, 20.0, 40.0, 40.0)));
    }

    #[test]
    fn covers_rejects_a_region_that_misses_an_edge() {
        assert!(!covers((10.0, 10.0, 80.0, 80.0), (20.0, 20.0, 90.0, 40.0)));
    }

    #[test]
    fn clamp_pixel_per_pt_applies_the_texture_budget() {
        // A 100x200 pt region: the 200 pt side must stay within 100 px -> 0.5 px/pt.
        let region = (0.0, 0.0, 100.0, 200.0);
        assert!((clamp_pixel_per_pt(region, 2.0, 100) - 0.5).abs() < 1e-6);
        // Below the budget the wanted scale is kept.
        assert!((clamp_pixel_per_pt(region, 0.2, 100) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn fallback_scale_uses_dpi_and_respects_the_texture_budget() {
        let page = (595.0, 842.0);
        assert!((fallback_pixel_per_pt(page, 144.0, 4096, 4.0) - 2.0).abs() < 1e-6);
        let huge = fallback_pixel_per_pt(page, 600.0, 1000, 4.0);
        assert!(huge <= 1000.0 / 842.0 + 1e-6, "huge = {huge}");
    }

    #[test]
    fn fallback_scale_shrinks_when_the_page_is_small_on_screen() {
        let page = (595.0, 842.0);
        let tiny = fallback_pixel_per_pt(page, 300.0, 4096, 0.01);
        assert!(tiny <= 0.02 + 1e-6, "tiny = {tiny}");
        let a = fallback_pixel_per_pt(page, 300.0, 4096, 0.30);
        let b = fallback_pixel_per_pt(page, 300.0, 4096, 0.45);
        assert!((a - b).abs() < 1e-6, "a = {a}, b = {b}");
        assert!((fallback_pixel_per_pt(page, 144.0, 4096, 8.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn scale_matches_is_relative_and_tolerant() {
        assert!(scale_matches(100.0, 100.5));
        assert!(!scale_matches(100.0, 103.0));
    }
}
