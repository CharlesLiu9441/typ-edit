//! Stroke tessellation, including path-space dashing.

use egui::pos2;
use lyon::path::PathEvent;
use lyon::path::iterator::PathIterator;
use lyon::tessellation::{
    BuffersBuilder, LineCap as LyonCap, LineJoin as LyonJoin, StrokeOptions, StrokeTessellator,
    VertexBuffers,
};
use typst_library::visualize::{FixedStroke, LineCap, LineJoin};

use super::feather::add_feather;
use super::{FLATTEN_TOLERANCE_PX, Path2, TessCtx, TessMesh, TessVertex};

/// Strokes `path` into `out` with an explicit feather width, or `None` for a
/// core-only mesh (the GPU canvas renderer relies on MSAA instead of a feather
/// band).
///
/// The outline is built in path space (so non-uniform transforms scale the
/// stroke exactly like typst), then mapped to screen space, where the
/// anti-aliasing feather is added. The flattening tolerance is divided by the
/// path-space scale so it stays [`FLATTEN_TOLERANCE_PX`] in screen points.
pub(crate) fn stroke_with(
    path: &Path2,
    stroke: &FixedStroke,
    ctx: &TessCtx,
    feather_px: Option<f32>,
    out: &mut TessMesh,
) {
    let thickness = stroke.thickness.to_pt();
    if !thickness.is_finite() || thickness <= 0.0 {
        return;
    }

    let scale = ((ctx.to_screen.sx.abs() + ctx.to_screen.sy.abs()) / 2.0).max(1e-6);
    let tolerance = (FLATTEN_TOLERANCE_PX as f64 / scale) as f32;
    let options = StrokeOptions::default()
        .with_tolerance(tolerance)
        .with_line_width(thickness as f32)
        .with_start_cap(map_cap(stroke.cap))
        .with_end_cap(map_cap(stroke.cap))
        .with_line_join(map_join(stroke.join))
        .with_miter_limit((stroke.miter_limit.get() as f32).max(1.0));

    let lyon_path = path.to_lyon(None);
    let mut core = TessMesh::default();
    match dash_params(stroke) {
        None => tessellate(&lyon_path, &options, ctx, &mut core),
        Some((pattern, phase)) => {
            let polylines = flatten_polylines(&lyon_path, tolerance);
            let mut runs = Vec::new();
            for polyline in &polylines {
                dash_polyline(polyline, &pattern, phase, &mut runs);
            }
            for run in runs {
                if !run.is_empty() {
                    tessellate(&run_path(&run), &options, ctx, &mut core);
                }
            }
        }
    }

    if core.vertices.is_empty() {
        return;
    }
    out.append(&core);
    if let Some(feather_px) = feather_px {
        add_feather(&core.vertices, &core.indices, ctx, feather_px, out);
    }
}

/// Tessellates one lyon path into `core`, mapping path-space vertices to screen.
fn tessellate(
    path: &lyon::path::Path,
    options: &StrokeOptions,
    ctx: &TessCtx,
    core: &mut TessMesh,
) {
    let to_screen = ctx.to_screen;
    let mut geometry: VertexBuffers<TessVertex, u32> = VertexBuffers::new();
    {
        let mut builder =
            BuffersBuilder::new(&mut geometry, |v: lyon::tessellation::StrokeVertex| {
                let p = v.position();
                let (sx, sy) = to_screen.map(p.x as f64, p.y as f64);
                TessVertex {
                    pos: pos2(sx as f32, sy as f32),
                    local: pos2(p.x, p.y),
                    coverage: 1.0,
                }
            });
        let mut tess = StrokeTessellator::new();
        if tess.tessellate_path(path, options, &mut builder).is_err() {
            return;
        }
    }
    core.append(&TessMesh {
        vertices: geometry.vertices,
        indices: geometry.indices,
    });
}

/// Returns the resolved dash pattern (lengths and phase in points), or `None`
/// when the stroke is solid.
fn dash_params(stroke: &FixedStroke) -> Option<(Vec<f64>, f64)> {
    let dash = stroke.dash.as_ref()?;
    let mut pattern: Vec<f64> = dash.array.iter().map(|a| a.to_pt().max(0.0)).collect();
    if pattern.is_empty() || pattern.iter().all(|&l| l <= 0.0) {
        return None;
    }
    // An odd number of dashes is repeated to make on/off alternate cleanly.
    if pattern.len() % 2 == 1 {
        let repeat = pattern.clone();
        pattern.extend(repeat);
    }
    Some((pattern, dash.phase.to_pt()))
}

/// Flattens a lyon path into one polyline per subpath, in path space.
fn flatten_polylines(path: &lyon::path::Path, tolerance: f32) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    for event in path.iter().flattened(tolerance) {
        match event {
            PathEvent::Begin { at } => out.push(vec![(at.x as f64, at.y as f64)]),
            PathEvent::Line { to, .. }
            | PathEvent::Quadratic { to, .. }
            | PathEvent::Cubic { to, .. } => {
                if let Some(last) = out.last_mut() {
                    last.push((to.x as f64, to.y as f64));
                }
            }
            PathEvent::End { first, close, .. } => {
                if let (Some(last), true) = (out.last_mut(), close) {
                    last.push((first.x as f64, first.y as f64));
                }
            }
        }
    }
    out
}

/// Walks one polyline, pushing each "on" run into `runs`.
fn dash_polyline(
    points: &[(f64, f64)],
    pattern: &[f64],
    phase: f64,
    runs: &mut Vec<Vec<(f64, f64)>>,
) {
    if points.is_empty() {
        return;
    }
    let n = pattern.len();
    let total: f64 = pattern.iter().sum();
    if pattern.is_empty() || !total.is_finite() || total <= 0.0 {
        return;
    }
    let (mut idx, mut remaining, mut on) = seed(pattern, phase);
    let mut current: Vec<(f64, f64)> = Vec::new();
    if on {
        current.push(points[0]);
    }

    for window in points.windows(2) {
        let (p0, p1) = (window[0], window[1]);
        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
        let seg = (dx * dx + dy * dy).sqrt();
        if seg <= 0.0 {
            continue;
        }
        if on && current.is_empty() {
            current.push(p0);
        }
        let mut t = 0.0;
        while t < seg - 1e-9 {
            let step = remaining.min(seg - t);
            t += step;
            remaining -= step;
            let point = (p0.0 + dx * (t / seg), p0.1 + dy * (t / seg));
            if on {
                current.push(point);
            }
            if remaining <= 1e-9 {
                idx += 1;
                remaining = pattern[idx % n];
                let next_on = idx.is_multiple_of(2);
                if on && !next_on {
                    if !current.is_empty() {
                        runs.push(std::mem::take(&mut current));
                    }
                } else if !on && next_on {
                    current.push(point);
                }
                on = next_on;
            }
        }
    }

    if on && !current.is_empty() {
        runs.push(current);
    }
}

/// Finds the pattern element containing `phase`.
fn seed(pattern: &[f64], phase: f64) -> (usize, f64, bool) {
    let n = pattern.len();
    let total: f64 = pattern.iter().sum();
    let mut consumed = phase.rem_euclid(total);
    let mut idx = 0usize;
    while idx + 1 < n && consumed >= pattern[idx] {
        consumed -= pattern[idx];
        idx += 1;
    }
    (idx, pattern[idx] - consumed, idx.is_multiple_of(2))
}

/// Builds an open lyon path through `run`.
fn run_path(run: &[(f64, f64)]) -> lyon::path::Path {
    let mut builder = lyon::path::Path::builder();
    builder.begin(lyon::math::point(run[0].0 as f32, run[0].1 as f32));
    for p in &run[1..] {
        builder.line_to(lyon::math::point(p.0 as f32, p.1 as f32));
    }
    builder.end(false);
    builder.build()
}

fn map_cap(cap: LineCap) -> LyonCap {
    match cap {
        LineCap::Butt => LyonCap::Butt,
        LineCap::Round => LyonCap::Round,
        LineCap::Square => LyonCap::Square,
    }
}

fn map_join(join: LineJoin) -> LyonJoin {
    match join {
        LineJoin::Miter => LyonJoin::Miter,
        LineJoin::Round => LyonJoin::Round,
        LineJoin::Bevel => LyonJoin::Bevel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::affine::Affine2;
    use crate::tess::FEATHER_PX;
    use crate::tess::test_support::{covered, solid_bounds};
    use typst_library::layout::{Abs, Point};
    use typst_library::visualize::DashPattern;

    fn identity() -> TessCtx {
        TessCtx::new(Affine2::IDENTITY)
    }

    fn line_10() -> Path2 {
        Path2::line(Point::new(Abs::pt(10.0), Abs::pt(0.0)))
    }

    fn stroke_of(thickness: f64, cap: LineCap) -> FixedStroke {
        FixedStroke {
            thickness: Abs::pt(thickness),
            cap,
            ..FixedStroke::default()
        }
    }

    fn render(s: &FixedStroke) -> TessMesh {
        let mut mesh = TessMesh::default();
        stroke_with(&line_10(), s, &identity(), Some(FEATHER_PX), &mut mesh);
        mesh
    }

    #[test]
    fn butt_line_has_width_two_centred_on_the_line() {
        let b = solid_bounds(&render(&stroke_of(2.0, LineCap::Butt)));
        assert!(b.0 <= 0.05 && b.0 >= -1.5, "{b:?}");
        assert!(b.1 <= -0.95 && b.1 >= -2.5, "{b:?}");
        assert!(b.2 >= 9.95 && b.2 <= 11.5, "{b:?}");
        assert!(b.3 >= 0.95 && b.3 <= 2.5, "{b:?}");
    }

    #[test]
    fn round_cap_extends_beyond_butt() {
        let b = solid_bounds(&render(&stroke_of(2.0, LineCap::Butt)));
        let r = solid_bounds(&render(&stroke_of(2.0, LineCap::Round)));
        assert!(r.0 <= b.0 - 0.5, "round {r:?} butt {b:?}");
        assert!(r.2 >= b.2 + 0.5, "round {r:?} butt {b:?}");
    }

    #[test]
    fn dashes_leave_gaps_but_span_the_line() {
        let mut s = stroke_of(1.0, LineCap::Butt);
        s.dash = Some(DashPattern {
            array: vec![Abs::pt(2.0), Abs::pt(2.0)],
            phase: Abs::zero(),
        });
        let mesh = render(&s);
        assert!(!mesh.is_empty());
        assert!(covered(&mesh, 1.0, 0.0));
        assert!(!covered(&mesh, 3.0, 0.0));
        let b = solid_bounds(&mesh);
        assert!(b.0 <= 0.05 && b.2 >= 9.95, "{b:?}");
    }

    #[test]
    fn non_uniform_transform_scales_the_stroke() {
        // A 2x horizontal scale makes the 2pt-wide horizontal stroke twice as
        // tall in screen space (path space keeps it 2pt).
        let ctx = TessCtx::new(Affine2::uniform_scale_translate(2.0, 0.0, 0.0));
        let mut mesh = TessMesh::default();
        stroke_with(
            &line_10(),
            &stroke_of(2.0, LineCap::Butt),
            &ctx,
            Some(FEATHER_PX),
            &mut mesh,
        );
        let b = solid_bounds(&mesh);
        // Core half-width is 1pt -> 2 screen points, plus the 1px feather.
        assert!(b.1 <= -2.95 && b.1 >= -3.05, "{b:?}");
        assert!(b.3 >= 2.95 && b.3 <= 3.05, "{b:?}");
        assert!(b.2 >= 19.95, "{b:?}");
    }
}
