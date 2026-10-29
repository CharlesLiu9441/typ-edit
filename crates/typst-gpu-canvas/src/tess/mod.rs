//! Tessellation of typst paths into egui triangle meshes with an anti-aliasing
//! feather.
//!
//! All [`Path2`] coordinates and stroke widths are in *path space* (typst
//! points, x-right / y-down). [`TessCtx::to_screen`] maps path space into egui
//! screen points. Fills are tessellated in screen space so curve detail adapts
//! to zoom, while strokes are outlined in path space so that a non-uniform
//! transform scales the stroke exactly like typst does. Glyph meshes are baked
//! once at a fixed absolute size (with a proportional feather) and scaled to the
//! text size; see `text.rs`. Curve flattening and the feather width are both
//! measured in target-space points.

mod feather;
mod fill;
mod path;
mod stroke;

pub(crate) use fill::{fill, fill_with, place_core};
pub(crate) use path::{Path2, Seg, quad_to_cubic};
pub(crate) use stroke::stroke_with;

/// Feather (anti-aliasing) band width in screen points.
pub(crate) const FEATHER_PX: f32 = 1.0;
/// Curve flattening tolerance in screen points.
pub(crate) const FLATTEN_TOLERANCE_PX: f32 = 0.1;

/// One tessellated vertex.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TessVertex {
    /// Position in egui screen points.
    pub pos: egui::Pos2,
    /// Position the vertex corresponds to: path space for a mesh built by
    /// `fill`/`fill_core`, or item-local space after `place_core` places a
    /// cached core.
    pub local: egui::Pos2,
    /// Coverage in `[0, 1]`; 1 is fully inside the shape.
    pub coverage: f32,
}

/// A triangle soup produced by a fill or a stroke.
#[derive(Clone, Default, Debug)]
pub(crate) struct TessMesh {
    /// Vertices referenced by [`Self::indices`].
    pub vertices: Vec<TessVertex>,
    /// Triangle indices into [`Self::vertices`], three per triangle.
    pub indices: Vec<u32>,
}

impl TessMesh {
    /// Whether the mesh contains no vertices.
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// Appends `other`'s triangles into `self`, rebasing its indices.
    pub(crate) fn append(&mut self, other: &Self) {
        let base = self.vertices.len() as u32;
        self.indices.extend(other.indices.iter().map(|i| i + base));
        self.vertices.extend_from_slice(&other.vertices);
    }

    /// Builds the vertices of an opaque `color` mesh, with vertex alpha scaled
    /// by `coverage` (premultiplied sRGB) and the white UV.
    #[allow(dead_code)]
    pub(crate) fn solid_vertices(&self, color: egui::Color32) -> Vec<egui::epaint::Vertex> {
        self.vertices
            .iter()
            .map(|v| egui::epaint::Vertex {
                pos: v.pos,
                uv: egui::epaint::WHITE_UV,
                color: scale_color(color, v.coverage),
            })
            .collect()
    }

    /// Builds the vertices of a textured mesh; `uv_from_local` maps a `local`
    /// position to an egui UV. Vertex color is premultiplied white scaled by
    /// `coverage`.
    #[allow(dead_code)]
    pub(crate) fn textured_vertices(
        &self,
        uv_from_local: impl Fn(egui::Pos2) -> egui::Pos2,
    ) -> Vec<egui::epaint::Vertex> {
        self.vertices
            .iter()
            .map(|v| egui::epaint::Vertex {
                pos: v.pos,
                uv: uv_from_local(v.local),
                color: scale_color(egui::Color32::WHITE, v.coverage),
            })
            .collect()
    }

    /// Builds an opaque `color` mesh, with vertex alpha scaled by `coverage`
    /// (premultiplied sRGB).
    #[allow(dead_code)]
    pub fn solid(&self, color: egui::Color32) -> egui::Mesh {
        egui::Mesh {
            vertices: self.solid_vertices(color),
            indices: self.indices.clone(),
            ..Default::default()
        }
    }

    /// Builds a textured mesh; `uv_from_local` maps a `local` position to an
    /// egui UV. Vertex color is premultiplied white scaled by `coverage`.
    #[allow(dead_code)]
    pub fn textured(
        &self,
        texture_id: egui::TextureId,
        uv_from_local: impl Fn(egui::Pos2) -> egui::Pos2,
    ) -> egui::Mesh {
        let mut mesh = egui::Mesh::with_texture(texture_id);
        mesh.vertices = self.textured_vertices(uv_from_local);
        mesh.indices = self.indices.clone();
        mesh
    }
}

/// Scales every premultiplied sRGB channel by `coverage`, rounding to nearest.
pub(crate) fn scale_color(color: egui::Color32, coverage: f32) -> egui::Color32 {
    let scale = |c: u8| (c as f32 * coverage).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_premultiplied(
        scale(color.r()),
        scale(color.g()),
        scale(color.b()),
        scale(color.a()),
    )
}

/// Tessellation context: how path space maps onto the egui screen.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TessCtx {
    /// Maps path space to egui screen points.
    pub to_screen: crate::affine::Affine2,
}

impl TessCtx {
    /// Creates a context from a path-to-screen transform.
    pub fn new(to_screen: crate::affine::Affine2) -> Self {
        Self { to_screen }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::TessMesh;
    use egui::{Pos2, pos2};

    pub(crate) fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    pub(crate) fn solid_bounds(mesh: &TessMesh) -> (f32, f32, f32, f32) {
        let mut b = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for v in &mesh.solid(egui::Color32::WHITE).vertices {
            b = (
                b.0.min(v.pos.x),
                b.1.min(v.pos.y),
                b.2.max(v.pos.x),
                b.3.max(v.pos.y),
            );
        }
        b
    }

    fn point_in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
        let cross = |p1: Pos2, p2: Pos2, p3: Pos2| {
            (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y)
        };
        let (d1, d2, d3) = (cross(p, a, b), cross(p, b, c), cross(p, c, a));
        let eps = 1e-4;
        let negative = d1 < -eps || d2 < -eps || d3 < -eps;
        let positive = d1 > eps || d2 > eps || d3 > eps;
        !(negative && positive)
    }

    /// `true` when `(x, y)` lies inside a fully covered (core) triangle.
    pub(crate) fn covered(mesh: &TessMesh, x: f32, y: f32) -> bool {
        let p = pos2(x, y);
        for tri in mesh.indices.as_chunks::<3>().0 {
            let cov = |i: u32| mesh.vertices[i as usize].coverage;
            if cov(tri[0]) < 0.5 || cov(tri[1]) < 0.5 || cov(tri[2]) < 0.5 {
                continue;
            }
            let a = mesh.vertices[tri[0] as usize].pos;
            let b = mesh.vertices[tri[1] as usize].pos;
            let c = mesh.vertices[tri[2] as usize].pos;
            if point_in_triangle(p, a, b, c) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(coverage: f32) -> TessVertex {
        TessVertex {
            pos: egui::pos2(1.0, 2.0),
            local: egui::pos2(3.0, 4.0),
            coverage,
        }
    }

    #[test]
    fn solid_scales_premultiplied_channels_and_uses_white_uv() {
        let mesh = TessMesh {
            vertices: vec![vertex(0.5)],
            indices: vec![],
        };
        let out = mesh.solid(egui::Color32::from_rgba_premultiplied(100, 200, 40, 200));
        let v = out.vertices[0];
        assert_eq!(
            (v.color.r(), v.color.g(), v.color.b(), v.color.a()),
            (50, 100, 20, 100)
        );
        assert_eq!(v.uv, egui::epaint::WHITE_UV);
        assert_eq!(out.texture_id, egui::TextureId::default());
    }

    #[test]
    fn textured_maps_local_through_the_closure() {
        let mesh = TessMesh {
            vertices: vec![vertex(1.0)],
            indices: vec![],
        };
        let id = egui::TextureId::User(7);
        let out = mesh.textured(id, |l| egui::pos2(l.x * 0.1, l.y * 0.2));
        assert_eq!(out.texture_id, id);
        let v = out.vertices[0];
        assert!(approx(v.uv.x, 0.3));
        assert!(approx(v.uv.y, 0.8));
    }

    #[test]
    fn append_rebases_indices() {
        let mut a = TessMesh {
            vertices: vec![vertex(1.0)],
            indices: vec![],
        };
        let b = TessMesh {
            vertices: vec![vertex(1.0), vertex(1.0)],
            indices: vec![0, 1, 1],
        };
        a.append(&b);
        assert_eq!(a.vertices.len(), 3);
        assert_eq!(a.indices, vec![1, 2, 2]);
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }
}
