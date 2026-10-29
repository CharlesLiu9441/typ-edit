//! GPU traversal: group expansion and DrawCmd emission.
//!
//! Mirrors the CPU renderer's traversal structure, but instead of painting
//! shapes it records [`Command`](super::canvas::Command)s into a [`Page`] for
//! the wgpu callback. The full frame is recorded (the recording is
//! view-independent; the callback pans and zooms it on the GPU and its
//! transformed bounds feed the callback viewport that egui-wgpu clamps). Items the
//! renderer cannot draw return [`RenderError::UnsupportedCanvasContent`] so the
//! host can fall back to a raster path for the whole page.

use egui::Context;
use typst_library::layout::FrameItem;
use typst_library::visualize::{Curve, FillRule};

use crate::bvh::FrameBvh;
use crate::clip::{clip_is_axis_aligned_rect, clip_screen_rect};
use crate::error::RenderError;
use crate::tess::{self, Path2, TessCtx, TessMesh};
use crate::view::{View, paint_ctx};

use super::canvas::Page;
use super::paint::{self, ImageOptions};
use super::resources::CanvasVertex;

/// Traversal state that does not change between frame levels.
pub(crate) struct RecordCtx<'a> {
    /// Pixels per logical point of the host screen.
    pub(crate) ppp: f32,
    /// The egui context for glyph mesh caches; `None` only in tests.
    pub(crate) egui: Option<&'a Context>,
    /// Texture and source-size options for image recording.
    pub(crate) image_options: ImageOptions,
}

impl<'a> RecordCtx<'a> {
    /// The context for a glyph subframe: same caches and limits.
    pub(crate) fn subframe(&self) -> RecordCtx<'a> {
        RecordCtx {
            ppp: self.ppp,
            egui: self.egui,
            image_options: self.image_options,
        }
    }
}

/// Records every item of `bvh` into `page`, in typst paint order.
pub(crate) fn record_page(
    bvh: &FrameBvh,
    view: &View,
    ppp: f32,
    egui: &Context,
    image_options: ImageOptions,
    page: &mut Page,
) -> Result<(), RenderError> {
    let ctx = RecordCtx {
        ppp,
        egui: Some(egui),
        image_options,
    };
    record_frame(bvh, view, &ctx, page)
}

/// Records one frame level, in item order.
pub(crate) fn record_frame(
    bvh: &FrameBvh,
    view: &View,
    ctx: &RecordCtx<'_>,
    page: &mut Page,
) -> Result<(), RenderError> {
    for idx in 0..bvh.len() as u32 {
        record_item(bvh, idx, view, ctx, page)?;
    }
    Ok(())
}

/// Records the item at `idx`, expanding groups and applying their clips.
fn record_item(
    bvh: &FrameBvh,
    idx: u32,
    view: &View,
    ctx: &RecordCtx<'_>,
    page: &mut Page,
) -> Result<(), RenderError> {
    let (pos, item) = bvh.item_at(idx);
    match item {
        FrameItem::Group(group) => {
            let Some(child) = bvh.child_bvh(idx) else {
                return Ok(());
            };
            let child_view = view.child(pos, group);
            let Some(clip) = &group.clip else {
                return record_frame(child, &child_view, ctx, page);
            };
            if clip_is_axis_aligned_rect(clip) {
                page.push_scissor(clip_screen_rect(clip, &child_view));
                record_frame(child, &child_view, ctx, page)?;
                page.pop_scissor();
            } else {
                let (vertices, indices) = clip_mask(clip, &child_view);
                page.push_clip(vertices, indices);
                record_frame(child, &child_view, ctx, page)?;
                page.pop_clip();
            }
            Ok(())
        }
        FrameItem::Shape(shape, _) => paint::paint_shape(shape, pos, view, ctx, page),
        FrameItem::Text(text) => paint::paint_text(text, pos, view, ctx, page),
        FrameItem::Image(image, size, _) => {
            let paint_ctx = paint_ctx(&view.item(pos), false, ctx.ppp);
            paint::paint_image(image, *size, &paint_ctx, 1.0, ctx.image_options, page)
        }
        FrameItem::Link(..) | FrameItem::Tag(_) => Ok(()),
    }
}

/// Tessellates `clip` in screen space into stencil-mask canvas vertices.
fn clip_mask(clip: &Curve, view: &View) -> (Vec<CanvasVertex>, Vec<u32>) {
    let mut mesh = TessMesh::default();
    tess::fill(
        &Path2::from_curve(clip),
        FillRule::NonZero,
        &TessCtx::new(view.to_screen),
        false,
        &mut mesh,
    );
    let vertices = mesh
        .vertices
        .iter()
        .map(|v| CanvasVertex {
            pos: [v.pos.x, v.pos.y],
            local: [v.local.x, v.local.y],
            color: egui::Color32::WHITE.to_array(),
        })
        .collect();
    (vertices, mesh.indices.clone())
}

/// Builds an unsupported-content error.
pub(crate) fn unsupported(kind: &'static str) -> RenderError {
    RenderError::UnsupportedCanvasContent(kind)
}
