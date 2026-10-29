//! Glyph outline extraction and mip-level mesh baking.

use ttf_parser::{GlyphId, OutlineBuilder};
use typst_library::text::FontInstance;
use typst_library::visualize::FillRule;

use crate::affine::Affine2;
use crate::tess::{self, Path2, Seg, TessCtx, TessMesh};

pub(crate) use crate::glyph_cache::{cached_core, cached_outline};

/// Glyph mip levels: the absolute em sizes (logical screen points per em) a
/// glyph mesh is baked at.
///
/// Rendering picks the smallest level at least as large as the on-screen em, so
/// the scale stays in `(0.5, 1]` and zooming out steps to coarser meshes instead
/// of keeping full-resolution triangles on screen.
pub(crate) const GLYPH_MIP_LEVELS: [f64; 7] = [5.0, 10.0, 20.0, 40.0, 80.0, 160.0, 320.0];

/// Flattening tolerance of a baked glyph mesh, in bake-space screen points.
const GLYPH_FLATTEN_PX: f32 = 0.05;

/// Index of the smallest mip level at least as large as `em_px`.
pub(crate) fn mip_level(em_px: f64) -> usize {
    GLYPH_MIP_LEVELS
        .iter()
        .position(|&level| level >= em_px)
        .unwrap_or(GLYPH_MIP_LEVELS.len() - 1)
}

/// Bakes one glyph outline at mip `level` (em = [`GLYPH_MIP_LEVELS`]`[level]`
/// bake pixels), y-down from the baseline pen origin, core only (the feather is
/// generated at render time so its on-screen width can adapt to the text size).
pub(crate) fn bake_glyph(outline: &Path2, units_per_em: f64, level: usize) -> TessMesh {
    let k = GLYPH_MIP_LEVELS[level] / units_per_em;
    let bake = Affine2 {
        sx: k,
        ky: 0.0,
        kx: 0.0,
        sy: -k,
        tx: 0.0,
        ty: 0.0,
    };
    let mut mesh = TessMesh::default();
    tess::fill_with(
        outline,
        FillRule::NonZero,
        &TessCtx::new(bake),
        GLYPH_FLATTEN_PX,
        None,
        &mut mesh,
    );
    mesh
}

/// Extracts a glyph outline in font design units.
pub(crate) fn glyph_outline(font: &FontInstance, id: u16) -> Option<Path2> {
    let mut builder = PathBuilder::default();
    font.ttf().outline_glyph(GlyphId(id), &mut builder)?;
    Some(Path2(builder.segments))
}

#[derive(Default)]
struct PathBuilder {
    segments: Vec<Seg>,
    current: (f64, f64),
}

impl OutlineBuilder for PathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = (f64::from(x), f64::from(y));
        self.current = point;
        self.segments.push(Seg::Move(point.0, point.1));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = (f64::from(x), f64::from(y));
        self.current = point;
        self.segments.push(Seg::Line(point.0, point.1));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = (f64::from(x1), f64::from(y1));
        let end = (f64::from(x), f64::from(y));
        self.segments
            .push(tess::quad_to_cubic(self.current, control, end));
        self.current = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.current = (f64::from(x), f64::from(y));
        self.segments.push(Seg::Cubic(
            f64::from(x1),
            f64::from(y1),
            f64::from(x2),
            f64::from(y2),
            f64::from(x),
            f64::from(y),
        ));
    }

    fn close(&mut self) {
        self.segments.push(Seg::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: (f64, f64), expected: (f64, f64)) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-9 && (actual.1 - expected.1).abs() < 1e-9,
            "got {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn quad_to_converts_to_a_cubic_through_the_control_points() {
        let mut builder = PathBuilder::default();
        builder.move_to(0.0, 0.0);
        builder.quad_to(1.0, 1.0, 2.0, 0.0);
        assert_eq!(builder.segments.len(), 2);
        let Seg::Cubic(x1, y1, x2, y2, x3, y3) = builder.segments[1] else {
            panic!("expected a cubic segment");
        };
        approx((x1, y1), (2.0 / 3.0, 2.0 / 3.0));
        approx((x2, y2), (4.0 / 3.0, 2.0 / 3.0));
        approx((x3, y3), (2.0, 0.0));
    }

    #[test]
    fn move_line_and_close_are_recorded_in_order() {
        let mut builder = PathBuilder::default();
        builder.move_to(1.0, 2.0);
        builder.line_to(3.0, 4.0);
        builder.close();
        assert!(matches!(builder.segments[0], Seg::Move(1.0, 2.0)));
        assert!(matches!(builder.segments[1], Seg::Line(3.0, 4.0)));
        assert!(matches!(builder.segments[2], Seg::Close));
    }

    #[test]
    fn mip_level_picks_the_smallest_level_not_below_the_em() {
        assert_eq!(mip_level(3.0), 0);
        assert_eq!(mip_level(5.0), 0);
        assert_eq!(mip_level(7.0), 1);
        assert_eq!(mip_level(20.0), 2);
        assert_eq!(mip_level(1000.0), GLYPH_MIP_LEVELS.len() - 1);
    }

    #[test]
    fn coarser_mip_levels_use_fewer_triangles() {
        use typst_library::foundations::Bytes;
        use typst_library::layout::Abs;
        use typst_library::text::{Font, FontVariant, FontVariations};

        let data = typst_assets::fonts().next().expect("bundled font");
        let font = Font::new(Bytes::new(data), 0).expect("font parses");
        let instance = font.instantiate(
            FontVariant::default(),
            Abs::pt(20.0),
            &FontVariations::default(),
        );
        let id = instance.ttf().glyph_index('g').expect("glyph").0;
        let outline = glyph_outline(&instance, id).expect("outline");
        let units_per_em = instance.units_per_em();

        let fine = bake_glyph(&outline, units_per_em, 4);
        let coarse = bake_glyph(&outline, units_per_em, 1);
        assert!(!coarse.indices.is_empty());
        assert!(
            coarse.indices.len() < fine.indices.len(),
            "level 1 {} triangles vs level 4 {}",
            coarse.indices.len() / 3,
            fine.indices.len() / 3
        );
    }
}
