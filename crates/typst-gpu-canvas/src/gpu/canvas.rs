//! Recorded draw commands for one page.
//!
//! A [`Page`] is produced while traversing the page's [`FrameBvh`] and is
//! replayed by [`PageCallback`](super::callback::PageCallback) into egui's
//! render pass, in the order the commands were recorded (typst paint order).
//! The recording bakes in the page origin and zoom (the scale bucket center)
//! it was recorded with (`record_pos` / `record_scale`); the callback pans and
//! zooms it on the GPU through a transform uniform, so one recording serves
//! every position within its scale bucket.

use std::sync::atomic::{AtomicU64, Ordering};

use egui::{Color32, Pos2, Rect};
use typst_library::visualize::Image;

use crate::tess::TessMesh;

use super::gradient::GradientPlan;
use super::resources::{CanvasVertex, ImageKey};

/// Source of process-unique [`Page`] ids, used to cache GPU buffers across
/// frames.
static NEXT_PAGE_ID: AtomicU64 = AtomicU64::new(0);

/// One recorded page: commands in paint order plus their screen bounds.
pub(crate) struct Page {
    /// Process-unique recording id (see [`PageCallback`](super::callback::PageCallback)).
    pub(crate) id: u64,
    /// Commands in paint order.
    pub(crate) commands: Vec<Command>,
    /// Conservative screen-space bounding box of all recorded vertices; its
    /// transformed form sets the callback viewport.
    pub(crate) bounds: Rect,
    /// Page origin the recording was made at, in egui screen points.
    pub(crate) record_pos: Pos2,
    /// Screen logical points per typst point the recording was made at.
    pub(crate) record_scale: f32,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            id: NEXT_PAGE_ID.fetch_add(1, Ordering::Relaxed),
            commands: Vec::new(),
            bounds: Rect::NOTHING,
            record_pos: Pos2::ZERO,
            record_scale: 1.0,
        }
    }
}

impl Page {
    /// Whether no command was recorded.
    pub(crate) fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Records a solid mesh.
    pub(crate) fn push_solid(&mut self, mesh: &TessMesh, color: Color32) {
        if mesh.vertices.is_empty() {
            return;
        }
        let vertices = canvas_vertices(mesh, |v| {
            crate::tess::scale_color(color, v.coverage).to_array()
        });
        self.extend_bounds(&vertices);
        self.commands.push(Command::Solid {
            vertices,
            indices: mesh.indices.clone(),
        });
    }

    /// Records a gradient-filled mesh; the plan maps each vertex's path-space
    /// `local` into gradient parameter space.
    pub(crate) fn push_gradient(&mut self, mesh: &TessMesh, plan: GradientPlan) {
        if mesh.vertices.is_empty() {
            return;
        }
        let from_local = plan.from_local;
        let mut local = vec![[0.0_f32; 2]; mesh.vertices.len()];
        for (slot, v) in local.iter_mut().zip(&mesh.vertices) {
            let (x, y) = from_local.map(v.local.x as f64, v.local.y as f64);
            *slot = [x as f32, y as f32];
        }
        let vertices: Vec<CanvasVertex> = canvas_vertices(mesh, |v| {
            crate::tess::scale_color(Color32::WHITE, v.coverage).to_array()
        })
        .into_iter()
        .zip(local)
        .map(|(mut vertex, local)| {
            vertex.local = local;
            vertex
        })
        .collect();
        self.extend_bounds(&vertices);
        self.commands.push(Command::Gradient {
            vertices,
            indices: mesh.indices.clone(),
            plan: Box::new(plan),
        });
    }

    /// Records a textured image quad.
    pub(crate) fn push_image(
        &mut self,
        vertices: Vec<CanvasVertex>,
        indices: Vec<u32>,
        key: ImageKey,
        image: Image,
    ) {
        if vertices.is_empty() || indices.is_empty() {
            return;
        }
        self.extend_bounds(&vertices);
        self.commands.push(Command::Image {
            vertices,
            indices,
            key,
            image,
        });
    }

    /// Records an axis-aligned clip rectangle in screen points; the callback
    /// intersects the render scissor with it until the matching
    /// [`Page::pop_scissor`].
    pub(crate) fn push_scissor(&mut self, rect: Rect) {
        self.commands.push(Command::PushScissor { rect });
    }

    /// Records the matching scissor restore.
    pub(crate) fn pop_scissor(&mut self) {
        self.commands.push(Command::PopScissor);
    }

    /// Records a curved clip mask (screen-space triangles); the callback draws
    /// it into the stencil until the matching [`Page::pop_clip`].
    pub(crate) fn push_clip(&mut self, vertices: Vec<CanvasVertex>, indices: Vec<u32>) {
        if vertices.is_empty() || indices.is_empty() {
            return;
        }
        self.extend_bounds(&vertices);
        self.commands.push(Command::PushClip { vertices, indices });
    }

    /// Records the matching stencil-mask restore.
    pub(crate) fn pop_clip(&mut self) {
        self.commands.push(Command::PopClip);
    }

    fn extend_bounds(&mut self, vertices: &[CanvasVertex]) {
        for v in vertices {
            self.bounds.extend_with(Pos2::new(v.pos[0], v.pos[1]));
        }
    }
}

/// Builds canvas vertices for `mesh`, with `color` scaled by each vertex's
/// coverage; `local` stays the mesh's paint-space position (overridden by
/// [`Page::push_gradient`] where needed).
fn canvas_vertices(
    mesh: &TessMesh,
    color: impl Fn(&crate::tess::TessVertex) -> [u8; 4],
) -> Vec<CanvasVertex> {
    mesh.vertices
        .iter()
        .map(|v| CanvasVertex {
            pos: [v.pos.x, v.pos.y],
            local: [v.local.x, v.local.y],
            color: color(v),
        })
        .collect()
}

/// One recorded draw command.
pub(crate) enum Command {
    /// A solid-color mesh.
    Solid {
        /// Vertices in screen points.
        vertices: Vec<CanvasVertex>,
        /// Triangle indices.
        indices: Vec<u32>,
    },
    /// A gradient-filled mesh.
    Gradient {
        /// Vertices in screen points, with `local` in gradient space.
        vertices: Vec<CanvasVertex>,
        /// Triangle indices.
        indices: Vec<u32>,
        /// CPU-baked gradient parameters and LUT.
        plan: Box<GradientPlan>,
    },
    /// An image-textured quad drawing from the resource texture cache.
    Image {
        /// Vertices in screen points, with `local` as UV.
        vertices: Vec<CanvasVertex>,
        /// Triangle indices.
        indices: Vec<u32>,
        /// Texture cache key.
        key: ImageKey,
        /// Source image to convert and upload on first use.
        image: Image,
    },
    /// Intersect the scissor with `rect` until the matching [`Command::PopScissor`].
    PushScissor {
        /// Clip rectangle in screen points.
        rect: Rect,
    },
    /// Restore the scissor saved by the matching [`Command::PushScissor`].
    PopScissor,
    /// Draw a stencil mask until the matching [`Command::PopClip`].
    PushClip {
        /// Mask triangle vertices in screen points.
        vertices: Vec<CanvasVertex>,
        /// Mask triangle indices.
        indices: Vec<u32>,
    },
    /// Restore the stencil saved by the matching [`Command::PushClip`].
    PopClip,
}
