//! Fill tessellation.

use egui::pos2;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillRule as LyonFillRule, FillTessellator, VertexBuffers,
};
use typst_library::visualize::FillRule;

use super::feather::{add_feather, add_feather_placed};
use super::{FEATHER_PX, FLATTEN_TOLERANCE_PX, Path2, TessCtx, TessMesh, TessVertex};
use crate::affine::Affine2;

/// Fills `path` into `out`, optionally adding the anti-aliasing feather.
///
/// The path is transformed to screen space before tessellation so the curve
/// flattening tolerance used is measured in screen points. On tessellation
/// error nothing is appended.
pub(crate) fn fill(path: &Path2, rule: FillRule, ctx: &TessCtx, feather: bool, out: &mut TessMesh) {
    fill_with(
        path,
        rule,
        ctx,
        FLATTEN_TOLERANCE_PX,
        feather.then_some(FEATHER_PX),
        out,
    );
}

/// Fills `path` into `out` with an explicit flattening tolerance and optional
/// feather width, both measured in the target space of `ctx`.
pub(crate) fn fill_with(
    path: &Path2,
    rule: FillRule,
    ctx: &TessCtx,
    tolerance: f32,
    feather_px: Option<f32>,
    out: &mut TessMesh,
) {
    if path.is_empty() {
        return;
    }

    let lyon_path = path.to_lyon(Some(&ctx.to_screen));
    let options = FillOptions::tolerance(tolerance).with_fill_rule(map_rule(rule));
    let inv = ctx.to_screen.invert();

    let mut geometry: VertexBuffers<TessVertex, u32> = VertexBuffers::new();
    {
        let mut builder =
            BuffersBuilder::new(&mut geometry, |v: lyon::tessellation::FillVertex| {
                let p = v.position();
                let pos = pos2(p.x, p.y);
                let local = match inv {
                    Some(m) => {
                        let (x, y) = m.map(p.x as f64, p.y as f64);
                        pos2(x as f32, y as f32)
                    }
                    None => pos,
                };
                TessVertex {
                    pos,
                    local,
                    coverage: 1.0,
                }
            });
        let mut tess = FillTessellator::new();
        if tess
            .tessellate_path(&lyon_path, &options, &mut builder)
            .is_err()
        {
            return;
        }
    }

    let core = TessMesh {
        vertices: geometry.vertices,
        indices: geometry.indices,
    };
    if core.vertices.is_empty() {
        return;
    }
    out.append(&core);
    if let Some(feather_px) = feather_px {
        add_feather(&core.vertices, &core.indices, ctx, feather_px, out);
    }
}

/// Places a baked `core` into `out`, optionally with a screen-space feather.
///
/// Each core vertex is mapped from its bake space into item-local space by
/// `place` (`local`) and then onto screen points by `to_screen` (`pos`). A baked
/// core carries no feather of its own; when `feather_px` is `Some`, a band of
/// that screen-space width is generated around the placed geometry.
pub(crate) fn place_core(
    core: &TessMesh,
    place: &Affine2,
    to_screen: &Affine2,
    feather_px: Option<f32>,
    out: &mut TessMesh,
) {
    if core.vertices.is_empty() {
        return;
    }

    let base = out.vertices.len() as u32;
    out.vertices.reserve(core.vertices.len());
    for v in &core.vertices {
        let (x, y) = place.map(v.pos.x as f64, v.pos.y as f64);
        let local = pos2(x as f32, y as f32);
        let (sx, sy) = to_screen.map(x, y);
        out.vertices.push(TessVertex {
            pos: pos2(sx as f32, sy as f32),
            local,
            coverage: v.coverage,
        });
    }
    out.indices.extend(core.indices.iter().map(|i| i + base));
    if let Some(feather_px) = feather_px {
        add_feather_placed(core, place, to_screen, feather_px, out);
    }
}

/// Maps a typst fill rule onto the lyon one.
fn map_rule(rule: FillRule) -> LyonFillRule {
    match rule {
        FillRule::NonZero => LyonFillRule::NonZero,
        FillRule::EvenOdd => LyonFillRule::EvenOdd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::affine::Affine2;
    use crate::tess::test_support::{close, covered, solid_bounds};
    use crate::tess::{FEATHER_PX, Seg};
    use egui::{TextureId, pos2};
    use typst_library::layout::{Abs, Size};

    fn identity() -> TessCtx {
        TessCtx::new(Affine2::IDENTITY)
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Path2 {
        Path2(vec![
            Seg::Move(x0, y0),
            Seg::Line(x1, y0),
            Seg::Line(x1, y1),
            Seg::Line(x0, y1),
            Seg::Close,
        ])
    }

    /// Two same-winding squares, one nested in the other.
    fn nested_squares() -> Path2 {
        Path2(vec![
            Seg::Move(0.0, 0.0),
            Seg::Line(10.0, 0.0),
            Seg::Line(10.0, 10.0),
            Seg::Line(0.0, 10.0),
            Seg::Close,
            Seg::Move(3.0, 3.0),
            Seg::Line(7.0, 3.0),
            Seg::Line(7.0, 7.0),
            Seg::Line(3.0, 7.0),
            Seg::Close,
        ])
    }

    #[test]
    fn fill_rect_without_feather_is_two_triangles() {
        let path = Path2::rect(Size::new(Abs::pt(10.0), Abs::pt(20.0)));
        let mut mesh = TessMesh::default();
        fill(&path, FillRule::NonZero, &identity(), false, &mut mesh);
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        let b = solid_bounds(&mesh);
        assert!(
            close(b.0, 0.0) && close(b.1, 0.0) && close(b.2, 10.0) && close(b.3, 20.0),
            "{b:?}"
        );
    }

    #[test]
    fn fill_square_with_feather_adds_band_and_keeps_core() {
        let path = square(0.0, 0.0, 10.0, 10.0);
        let mut mesh = TessMesh::default();
        fill(&path, FillRule::NonZero, &identity(), true, &mut mesh);
        assert!(mesh.vertices.len() > 4);

        let mut core = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for v in mesh.vertices.iter().filter(|v| v.coverage >= 0.999) {
            core = (
                core.0.min(v.pos.x),
                core.1.min(v.pos.y),
                core.2.max(v.pos.x),
                core.3.max(v.pos.y),
            );
        }
        assert!(
            close(core.0, 0.0) && close(core.1, 0.0) && close(core.2, 10.0) && close(core.3, 10.0),
            "{core:?}"
        );
        assert!(mesh.vertices.iter().any(|v| v.coverage == 0.0));
        assert!(mesh.vertices.iter().any(|v| v.coverage >= 0.999));
        // The band is outside the core square.
        let b = solid_bounds(&mesh);
        assert!(
            close(b.0, -FEATHER_PX) && close(b.2, 10.0 + FEATHER_PX),
            "{b:?}"
        );
    }

    #[test]
    fn nonzero_fills_hole_even_odd_leaves_it_empty() {
        let path = nested_squares();
        let mut nonzero = TessMesh::default();
        fill(&path, FillRule::NonZero, &identity(), false, &mut nonzero);
        let mut even_odd = TessMesh::default();
        fill(&path, FillRule::EvenOdd, &identity(), false, &mut even_odd);

        assert!(covered(&nonzero, 5.0, 5.0));
        assert!(!covered(&even_odd, 5.0, 5.0));
        assert!(covered(&nonzero, 1.0, 1.0));
        assert!(covered(&even_odd, 1.0, 1.0));
    }

    #[test]
    fn local_is_screen_mapped_back_to_path_space() {
        let scale = Affine2::uniform_scale_translate(2.0, 5.0, 7.0);
        let ctx = TessCtx::new(scale);
        let path = Path2::rect(Size::new(Abs::pt(10.0), Abs::pt(10.0)));
        let mut mesh = TessMesh::default();
        fill(&path, FillRule::NonZero, &ctx, false, &mut mesh);
        assert!(!mesh.is_empty());
        for v in &mesh.vertices {
            assert!(close(v.pos.x, v.local.x * 2.0 + 5.0), "{v:?}");
            assert!(close(v.pos.y, v.local.y * 2.0 + 7.0), "{v:?}");
            assert!((-1e-3..=10.0 + 1e-3).contains(&v.local.x), "{v:?}");
            assert!((-1e-3..=10.0 + 1e-3).contains(&v.local.y), "{v:?}");
        }
    }

    #[test]
    fn textured_maps_local_and_sets_texture_id() {
        let path = Path2::rect(Size::new(Abs::pt(4.0), Abs::pt(6.0)));
        let mut mesh = TessMesh::default();
        fill(&path, FillRule::NonZero, &identity(), false, &mut mesh);
        let id = TextureId::User(7);
        let out = mesh.textured(id, |l| pos2(l.x * 0.1, l.y * 0.2));
        assert_eq!(out.texture_id, id);
        assert_eq!(out.vertices.len(), mesh.vertices.len());
        for (v, src) in out.vertices.iter().zip(&mesh.vertices) {
            assert!(close(v.uv.x, src.local.x * 0.1) && close(v.uv.y, src.local.y * 0.2));
        }
    }

    #[test]
    fn empty_path_fills_nothing() {
        let mut mesh = TessMesh::default();
        fill(
            &Path2::default(),
            FillRule::NonZero,
            &identity(),
            true,
            &mut mesh,
        );
        assert!(mesh.is_empty());
    }

    /// A closed cubic bump with exactly-representable control points.
    fn cubic_bump() -> Path2 {
        Path2(vec![
            Seg::Move(0.0, 0.0),
            Seg::Cubic(0.0, 10.0, 10.0, 10.0, 10.0, 0.0),
            Seg::Close,
        ])
    }

    #[test]
    fn placed_baked_core_matches_a_direct_core_fill_under_translation_and_scale() {
        // Straight edges make both compositions exact up to f32 rounding, so the
        // placed baked core matches a direct core fill within an epsilon. This is
        // the placement invariant the glyph mesh cache relies on.
        let path = square(0.0, 0.0, 10.0, 10.0);
        for (bake_k, place_k, tx, ty) in [(1.0, 1.0, 5.0, 7.0), (2.0, 0.5, -3.0, 2.5)] {
            let bake = Affine2::uniform_scale_translate(bake_k, 0.0, 0.0);
            let place = Affine2::uniform_scale_translate(place_k, 0.0, 0.0);
            let to_screen = Affine2::translate(tx, ty);

            let mut core = TessMesh::default();
            fill_with(
                &path,
                FillRule::NonZero,
                &TessCtx::new(bake),
                FLATTEN_TOLERANCE_PX,
                None,
                &mut core,
            );
            let mut placed = TessMesh::default();
            place_core(&core, &place, &to_screen, None, &mut placed);

            let mut direct = TessMesh::default();
            fill_with(
                &path,
                FillRule::NonZero,
                &TessCtx::new(to_screen),
                FLATTEN_TOLERANCE_PX,
                None,
                &mut direct,
            );

            assert_eq!(placed.vertices.len(), direct.vertices.len());
            assert_eq!(placed.indices, direct.indices);
            for (a, b) in placed.vertices.iter().zip(&direct.vertices) {
                assert!((a.pos.x - b.pos.x).abs() < 1e-4, "{a:?} vs {b:?}");
                assert!((a.pos.y - b.pos.y).abs() < 1e-4, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn placed_baked_core_of_a_curve_stays_geometrically_close() {
        // Curve flattening runs in f32 and picks its subdivision per transform,
        // so a placed baked curve core is only geometrically close, not
        // vertex-identical.
        let path = cubic_bump();
        for (tx, ty) in [(5.0, 7.0), (-3.0, 2.5)] {
            let to_screen = Affine2::translate(tx, ty);
            let mut direct = TessMesh::default();
            fill_with(
                &path,
                FillRule::NonZero,
                &TessCtx::new(to_screen),
                FLATTEN_TOLERANCE_PX,
                None,
                &mut direct,
            );

            let mut core = TessMesh::default();
            fill_with(
                &path,
                FillRule::NonZero,
                &TessCtx::new(Affine2::IDENTITY),
                FLATTEN_TOLERANCE_PX,
                None,
                &mut core,
            );
            let mut placed = TessMesh::default();
            place_core(&core, &Affine2::IDENTITY, &to_screen, None, &mut placed);

            assert!(!placed.is_empty());
            let (pb, db) = (solid_bounds(&placed), solid_bounds(&direct));
            for (a, b) in [(pb.0, db.0), (pb.1, db.1), (pb.2, db.2), (pb.3, db.3)] {
                assert!((a - b).abs() < 1e-4, "placed {pb:?} vs direct {db:?}");
            }
        }
    }
}
