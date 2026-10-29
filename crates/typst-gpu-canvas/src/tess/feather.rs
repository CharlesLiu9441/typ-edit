//! Anti-aliasing feather: turns the boundary edges of a core mesh into a band of
//! triangles whose outer vertices have zero coverage.

use std::collections::{HashMap, HashSet};

use egui::vec2;

use super::{TessCtx, TessMesh, TessVertex};
use crate::affine::Affine2;

/// Appends an anti-aliasing band of `feather_px` outward width around the
/// boundary of `core` into `out`.
///
/// `core` holds `coverage == 1.0` vertices and `indices` are 0-based into it.
/// A directed triangle edge `(a, b)` is a boundary when the reverse `(b, a)` is
/// absent; the third vertex `c` of that triangle defines the inward side, so the
/// outward normal is `perp(b - a)` flipped to point away from `c`.
pub(super) fn add_feather(
    core: &[TessVertex],
    indices: &[u32],
    ctx: &TessCtx,
    feather_px: f32,
    out: &mut TessMesh,
) {
    add_feather_with(
        indices,
        |i| {
            let v = core[i as usize];
            (v.pos, v.local)
        },
        ctx.to_screen.invert(),
        feather_px,
        out,
    );
}

/// Appends the feather band for a baked `core` placed by `place` and mapped to
/// screen by `to_screen`.
///
/// The band geometry is built in screen space at width `feather_px`, so callers
/// can keep the on-screen feather adaptive (capped for large text, narrowed for
/// small text) while reusing one baked mesh.
pub(super) fn add_feather_placed(
    core: &TessMesh,
    place: &Affine2,
    to_screen: &Affine2,
    feather_px: f32,
    out: &mut TessMesh,
) {
    add_feather_with(
        &core.indices,
        |i| {
            let v = &core.vertices[i as usize];
            let (x, y) = place.map(v.pos.x as f64, v.pos.y as f64);
            let local = egui::pos2(x as f32, y as f32);
            let (sx, sy) = to_screen.map(x, y);
            (egui::pos2(sx as f32, sy as f32), local)
        },
        to_screen.invert(),
        feather_px,
        out,
    );
}

/// Shared feather-band builder.
///
/// `screen_local(i)` yields the screen position and item-local position of the
/// core vertex `i`. `inv` maps an outer screen point back to item-local space
/// (or `None` when `to_screen` is singular, in which case the inner vertex's
/// local position is reused).
///
/// Convex corners keep independent perpendicular offsets per edge: the exterior
/// corner region has (near) zero coverage, so filling it would over-ink. Concave
/// corners instead share one mitered outer vertex per corner, so the two
/// adjacent bands meet edge-to-edge: independent quads would overlap there and
/// blend twice, accumulating alpha on small glyphs.
fn add_feather_with(
    indices: &[u32],
    screen_local: impl Fn(u32) -> (egui::Pos2, egui::Pos2),
    inv: Option<Affine2>,
    feather_px: f32,
    out: &mut TessMesh,
) {
    let mut edges: HashSet<(u32, u32)> = HashSet::with_capacity(indices.len());
    for tri in indices.as_chunks::<3>().0 {
        edges.insert((tri[0], tri[1]));
        edges.insert((tri[1], tri[2]));
        edges.insert((tri[2], tri[0]));
    }

    let mut boundary: Vec<(u32, u32, egui::Vec2)> = Vec::new();
    let mut incoming: HashMap<u32, (u32, egui::Vec2)> = HashMap::new();
    let mut outgoing: HashMap<u32, (u32, egui::Vec2)> = HashMap::new();
    let mut winding = 0.0_f64;
    for tri in indices.as_chunks::<3>().0 {
        let oriented = [
            (tri[0], tri[1], tri[2]),
            (tri[1], tri[2], tri[0]),
            (tri[2], tri[0], tri[1]),
        ];
        for (a, b, c) in oriented {
            if edges.contains(&(b, a)) {
                continue;
            }
            let (pa, _) = screen_local(a);
            let (pb, _) = screen_local(b);
            let (pc, _) = screen_local(c);
            let d = pb - pa;
            let len = d.length();
            if !len.is_finite() || len <= 1e-6 {
                continue;
            }
            let mut normal = vec2(d.y, -d.x) / len;
            if normal.dot(pc - pa) > 0.0 {
                normal = -normal;
            }
            boundary.push((a, b, normal));
            winding += f64::from(pa.x) * f64::from(pb.y) - f64::from(pb.x) * f64::from(pa.y);
            incoming.entry(b).or_insert((a, normal));
            outgoing.entry(a).or_insert((b, normal));
        }
    }

    // One merged outer offset per concave vertex; convex (or ambiguous) vertices
    // are absent and fall back to the per-edge perpendicular offset.
    let winding_sign = if winding < 0.0 { -1.0_f32 } else { 1.0_f32 };
    let mut merged: HashMap<u32, egui::Vec2> = HashMap::new();
    for (&v, &(a, n_in)) in &incoming {
        let Some(&(b, n_out)) = outgoing.get(&v) else {
            continue;
        };
        let (pv, _) = screen_local(v);
        let (pa, _) = screen_local(a);
        let (pb, _) = screen_local(b);
        let d_in = pv - pa;
        let d_out = pb - pv;
        let cross = d_in.x * d_out.y - d_in.y * d_out.x;
        let cross_sign = if cross < 0.0 { -1.0_f32 } else { 1.0_f32 };
        if cross_sign * winding_sign > 0.0 {
            continue;
        }
        let sum = n_in + n_out;
        let len = sum.length();
        if !len.is_finite() || len <= 1e-6 {
            continue;
        }
        let bisector = sum / len;
        let cos_half = bisector.dot(n_out).max(1.0 / MITER_LIMIT);
        merged.insert(
            v,
            bisector * (feather_px / cos_half).min(feather_px * MITER_LIMIT),
        );
    }

    for (a, b, edge_normal) in boundary {
        let (pa, local_a) = screen_local(a);
        let (pb, local_b) = screen_local(b);
        let off_a = merged.get(&a).copied().unwrap_or(edge_normal * feather_px);
        let off_b = merged.get(&b).copied().unwrap_or(edge_normal * feather_px);
        let base = out.vertices.len() as u32;
        out.vertices.push(TessVertex {
            pos: pa,
            local: local_a,
            coverage: 1.0,
        });
        out.vertices.push(TessVertex {
            pos: pb,
            local: local_b,
            coverage: 1.0,
        });
        out.vertices.push(TessVertex {
            pos: pb + off_b,
            local: local_of(inv, pb + off_b, local_b),
            coverage: 0.0,
        });
        out.vertices.push(TessVertex {
            pos: pa + off_a,
            local: local_of(inv, pa + off_a, local_a),
            coverage: 0.0,
        });
        out.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Longest miter, as a multiple of the feather width; sharper corners are
/// clamped so near-reversals cannot spike.
const MITER_LIMIT: f32 = 3.0;

/// Maps a generated screen position back to path space, or falls back to
/// `fallback` when `to_screen` is singular.
fn local_of(inv: Option<Affine2>, screen: egui::Pos2, fallback: egui::Pos2) -> egui::Pos2 {
    match inv {
        Some(m) => {
            let (x, y) = m.map(screen.x as f64, screen.y as f64);
            egui::pos2(x as f32, y as f32)
        }
        None => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn v(x: f32, y: f32) -> TessVertex {
        TessVertex {
            pos: pos2(x, y),
            local: pos2(x, y),
            coverage: 1.0,
        }
    }

    fn quad() -> (Vec<TessVertex>, Vec<u32>) {
        (
            vec![v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(0.0, 1.0)],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    #[test]
    fn adds_one_quad_per_boundary_edge() {
        let (verts, idx) = quad();
        let mut out = TessMesh::default();
        out.append(&TessMesh {
            vertices: verts.clone(),
            indices: idx.clone(),
        });
        add_feather(
            &verts,
            &idx,
            &TessCtx::new(Affine2::IDENTITY),
            1.0,
            &mut out,
        );
        assert_eq!(out.vertices.len(), 4 + 16);
        assert_eq!(out.indices.len(), 6 + 24);
        assert!(out.vertices.iter().any(|v| v.coverage == 0.0));
        assert!(out.vertices.iter().any(|v| v.coverage == 1.0));
    }

    #[test]
    fn band_extends_outward_by_feather_width() {
        let (verts, idx) = quad();
        let mut out = TessMesh::default();
        add_feather(
            &verts,
            &idx,
            &TessCtx::new(Affine2::IDENTITY),
            1.0,
            &mut out,
        );
        let bounds = |f: fn(&TessVertex) -> f32| {
            out.vertices
                .iter()
                .map(f)
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| {
                    (lo.min(x), hi.max(x))
                })
        };
        assert_eq!(bounds(|v| v.pos.x), (-1.0, 2.0));
        assert_eq!(bounds(|v| v.pos.y), (-1.0, 2.0));
    }

    #[test]
    fn degenerate_edges_are_skipped() {
        // Vertices 0 and 1 coincide, so edge (0, 1) is degenerate; only the two
        // real edges (1, 2) and (2, 0) get a band.
        let verts = vec![v(0.0, 0.0), v(0.0, 0.0), v(1.0, 0.0)];
        let idx = vec![0u32, 1, 2];
        let mut out = TessMesh::default();
        add_feather(
            &verts,
            &idx,
            &TessCtx::new(Affine2::IDENTITY),
            1.0,
            &mut out,
        );
        assert_eq!(out.vertices.len(), 8);
        assert_eq!(out.indices.len(), 12);
    }
}
