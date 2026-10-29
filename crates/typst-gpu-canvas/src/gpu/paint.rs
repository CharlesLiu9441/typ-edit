//! CPU-side leaf painting for the GPU canvas renderer.
//!
//! Emits GPU draw commands instead of egui shapes: fills, strokes, and glyph
//! outlines are tessellated in screen space with the feather disabled (MSAA
//! anti-aliases), raster images become textured quads whose texture is
//! converted and uploaded once by the resource cache, and text uses baked glyph
//! mip meshes. A vectorizable SVG is recorded as vector commands by
//! [`super::svg`]; one that is not is rasterized per image through the same
//! texture path as a raster image.

use std::sync::Arc;

use egui::Color32;
use ttf_parser::GlyphId;
use typst_library::layout::{Frame, Point, Size};
use typst_library::text::{TextItem, color::should_outline};
use typst_library::visualize::{Geometry, Image, ImageKind, Paint, Shape};

use crate::affine::Affine2;
use crate::bvh::FrameBvh;
use crate::error::RenderError;
use crate::glyphs::{self, GLYPH_MIP_LEVELS};
use crate::image_helpers::{image_key, quad_corners};
use crate::paint::{PaintCtx, QUAD_INDICES, QUAD_UVS, color32, rect_bbox};
use crate::tess::{self, Path2, TessCtx, TessMesh};
use crate::view::View;

use super::canvas::Page;
use super::gradient::GradientPlan;
use super::resources::CanvasVertex;
use super::traverse::{RecordCtx, unsupported};

/// Records one shape's fill and stroke.
pub(crate) fn paint_shape(
    shape: &Shape,
    pos: Point,
    view: &View,
    record: &RecordCtx<'_>,
    page: &mut Page,
) -> Result<(), RenderError> {
    let ctx = crate::view::paint_ctx(&view.item(pos), false, record.ppp);
    let tess_ctx = TessCtx::new(ctx.to_screen);

    if let Some(fill) = &shape.fill {
        let bbox = rect_bbox(shape.bbox(false));
        let mut mesh = TessMesh::default();
        match &shape.geometry {
            Geometry::Rect(size) => tess::fill(
                &Path2::rect(*size),
                shape.fill_rule,
                &tess_ctx,
                false,
                &mut mesh,
            ),
            Geometry::Curve(curve) => tess::fill(
                &Path2::from_curve(curve),
                shape.fill_rule,
                &tess_ctx,
                false,
                &mut mesh,
            ),
            Geometry::Line(_) => {}
        }
        push_paint(&mesh, fill, bbox, &ctx, page)?;
    }

    let Some(stroke) = &shape.stroke else {
        return Ok(());
    };
    if stroke.thickness.to_pt() <= 0.0 {
        return Ok(());
    }
    let bbox = rect_bbox(shape.bbox(matches!(&stroke.paint, Paint::Gradient(_))));
    let path = match &shape.geometry {
        Geometry::Line(end) => Path2::line(*end),
        Geometry::Rect(size) => Path2::rect(*size),
        Geometry::Curve(curve) => Path2::from_curve(curve),
    };
    let mut mesh = TessMesh::default();
    tess::stroke_with(&path, stroke, &tess_ctx, None, &mut mesh);
    push_paint(&mesh, &stroke.paint, bbox, &ctx, page)
}

/// Records a tessellated mesh painted with `paint`.
pub(crate) fn push_paint(
    mesh: &TessMesh,
    paint: &Paint,
    bbox: crate::bounds::Extents,
    ctx: &PaintCtx,
    page: &mut Page,
) -> Result<(), RenderError> {
    if mesh.is_empty() {
        return Ok(());
    }
    match paint {
        Paint::Solid(color) => {
            page.push_solid(mesh, color32(color));
            Ok(())
        }
        Paint::Gradient(gradient) => {
            let plan = GradientPlan::new(gradient, bbox, ctx);
            page.push_gradient(mesh, plan);
            Ok(())
        }
        Paint::Tiling(_) => Err(unsupported("tiling")),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageOptions {
    pub(crate) max_texture_side: u32,
    pub(crate) max_source_side: u32,
    pub(crate) strategy: super::ImageStrategy,
}

/// Records one typst image.
///
/// Raster images (and SVGs that are not vectorizable) become textured quads
/// whose texture the resource cache converts and uploads on first use; a
/// vectorizable SVG is recorded as vector draw commands instead. PDFs are still
/// unsupported.
///
/// `alpha` is the inherited group opacity: a raster image inside a translucent
/// SVG group is tinted by it (matching resvg's layer compositing for a single
/// image leaf). Typst images pass `1.0`.
pub(crate) fn paint_image(
    image: &Image,
    size: Size,
    ctx: &PaintCtx,
    alpha: f32,
    options: ImageOptions,
    page: &mut Page,
) -> Result<(), RenderError> {
    match image.kind() {
        ImageKind::Raster(_) => {
            push_image_quad(image, size, ctx, alpha, options, page);
            Ok(())
        }
        ImageKind::Svg(svg) => {
            if super::svg::is_vectorizable(svg.tree()) {
                super::svg::paint_svg(svg, size, ctx, alpha, options, page)
            } else {
                push_image_quad(image, size, ctx, alpha, options, page);
                Ok(())
            }
        }
        ImageKind::Pdf(_) => Err(unsupported("pdf image")),
    }
}

/// Records `image` as a textured quad drawing from the resource texture cache.
fn push_image_quad(
    image: &Image,
    size: Size,
    ctx: &PaintCtx,
    alpha: f32,
    options: ImageOptions,
    page: &mut Page,
) {
    let key = image_key(
        image,
        size,
        ctx,
        options.max_texture_side,
        options.max_source_side,
        options.strategy,
    );
    // Premultiplied white scaled by the inherited alpha: `alpha == 1` is the
    // opaque white tint the image pipeline always used.
    let tint = Color32::from_rgba_unmultiplied(
        255,
        255,
        255,
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    );
    let corners = quad_corners(ctx, size);
    let vertices: Vec<CanvasVertex> = corners
        .iter()
        .zip(QUAD_UVS)
        .map(|(&pos, (u, v))| CanvasVertex {
            pos: [pos.x, pos.y],
            local: [u, v],
            color: tint.to_array(),
        })
        .collect();
    page.push_image(vertices, QUAD_INDICES.to_vec(), key, image.clone());
}

/// Records one typst text run through the baked glyph mip meshes.
///
/// Mirrors the CPU outline path with `feather_px = None`: MSAA anti-aliases
/// the glyph edges, and the SDF shortcut is skipped entirely (a wgpu callback
/// cannot sample egui's texture atlas).
pub(crate) fn paint_text(
    text: &TextItem,
    pos: Point,
    view: &View,
    record: &RecordCtx<'_>,
    page: &mut Page,
) -> Result<(), RenderError> {
    let ctx = crate::view::paint_ctx(&view.item(pos), true, record.ppp);
    let tess_ctx = TessCtx::new(ctx.to_screen);
    let bbox = rect_bbox(text.bbox());

    let design_scale = text.size.to_pt() / text.font.units_per_em();
    let view_scale = (ctx.to_screen.sx.abs() + ctx.to_screen.sy.abs()) / 2.0;
    let em_px = text.size.to_pt() * view_scale;
    let level = glyphs::mip_level(em_px);
    let bake_scale = text.size.to_pt() / GLYPH_MIP_LEVELS[level];
    let (mut advance_x, mut advance_y) = (0.0_f64, 0.0_f64);

    let mut fill_mesh = TessMesh::default();
    let mut stroke_mesh = TessMesh::default();

    for glyph in &text.glyphs {
        if should_outline(&text.font, GlyphId(glyph.id)) {
            let x = advance_x + glyph.x_offset.at(text.size).to_pt();
            let y = advance_y + glyph.y_offset.at(text.size).to_pt();
            let core = match record.egui {
                Some(egui_ctx) => glyphs::cached_core(egui_ctx, &text.font, glyph.id, level),
                None => {
                    let outline = glyphs::glyph_outline(&text.font, glyph.id).unwrap_or_default();
                    Arc::new(glyphs::bake_glyph(
                        &outline,
                        text.font.units_per_em(),
                        level,
                    ))
                }
            };
            // The bake is y-down and em-sized, so placement is a uniform scale
            // from bake pixels to item-local points plus the pen.
            let placement = Affine2 {
                sx: bake_scale,
                ky: 0.0,
                kx: 0.0,
                sy: bake_scale,
                tx: x,
                ty: -y,
            };
            tess::place_core(&core, &placement, &ctx.to_screen, None, &mut fill_mesh);

            if let Some(stroke) = &text.stroke {
                let stroke_placement = Affine2 {
                    sx: design_scale,
                    ky: 0.0,
                    kx: 0.0,
                    sy: -design_scale,
                    tx: x,
                    ty: -y,
                };
                let outline = match record.egui {
                    Some(egui_ctx) => glyphs::cached_outline(egui_ctx, &text.font, glyph.id),
                    None => {
                        Arc::new(glyphs::glyph_outline(&text.font, glyph.id).unwrap_or_default())
                    }
                };
                let path = outline.map(&stroke_placement);
                tess::stroke_with(&path, stroke, &tess_ctx, None, &mut stroke_mesh);
            }
        } else if let Some(glyph_frame) =
            typst_library::text::color::glyph_frame(&text.font, glyph.id)
        {
            // Mirror `typst-render`'s color/bitmap glyph placement: the frame is
            // already y-down in font units, so it is placed at the pen and
            // uniformly scaled by `size / upem`, with no y flip.
            let glyph_to_item = Affine2 {
                sx: design_scale,
                ky: 0.0,
                kx: 0.0,
                sy: design_scale,
                tx: advance_x + glyph.x_offset.at(text.size).to_pt(),
                ty: -(advance_y + glyph.y_offset.at(text.size).to_pt()),
            };
            let to_screen = glyph_to_item.then(ctx.to_screen);
            let frame: Frame = glyph_frame.into();
            let sub_bvh = FrameBvh::new(frame);
            let sub_view = View::from_transform(to_screen, sub_bvh.frame().size());
            super::traverse::record_frame(&sub_bvh, &sub_view, &record.subframe(), page)?;
        }

        advance_x += glyph.x_advance.at(text.size).to_pt();
        advance_y += glyph.y_advance.at(text.size).to_pt();
    }

    push_paint(&fill_mesh, &text.fill, bbox, &ctx, page)?;
    if let Some(stroke) = &text.stroke {
        push_paint(&stroke_mesh, &stroke.paint, bbox, &ctx, page)?;
    }
    Ok(())
}
