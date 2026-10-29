//! Low-level draw helpers for the GPU canvas callback's replay pass.

use egui_wgpu::wgpu;

use super::resources::ContentPipelines;

/// Scissor rectangle in physical pixels: x, y, width, height.
pub(crate) type ScissorPx = (i32, i32, i32, i32);

/// The callback's initial scissor, from egui's clip rectangle.
///
/// egui-wgpu sets a scissor before invoking a callback, but the callback
/// re-establishes the full-screen viewport, so it must also re-establish the
/// clip explicitly from [`egui::PaintCallbackInfo::clip_rect_in_pixels`].
pub(crate) fn clip_scissor(info: &egui::PaintCallbackInfo) -> ScissorPx {
    let viewport = info.clip_rect_in_pixels();
    (
        viewport.left_px.max(0),
        viewport.top_px.max(0),
        viewport.width_px.max(0),
        viewport.height_px.max(0),
    )
}

/// Applies a recording transform `(ratio, offset)` to a point-space rect.
pub(crate) fn transform_rect(rect: egui::Rect, ratio: f32, offset: egui::Vec2) -> egui::Rect {
    egui::Rect::from_min_max(rect.min * ratio + offset, rect.max * ratio + offset)
}

/// Converts a point-space rectangle into a clamped pixel scissor.
pub(crate) fn rect_scissor(rect: egui::Rect, ppp: f32, screen: [u32; 2]) -> ScissorPx {
    let width = screen[0] as i32;
    let height = screen[1] as i32;
    let left = ((ppp * rect.min.x).round() as i32).clamp(0, width);
    let top = ((ppp * rect.min.y).round() as i32).clamp(0, height);
    let right = ((ppp * rect.max.x).round() as i32).clamp(left, width);
    let bottom = ((ppp * rect.max.y).round() as i32).clamp(top, height);
    (left, top, right - left, bottom - top)
}

/// The intersection of two scissors, collapsing to zero area when disjoint.
pub(crate) fn intersect(a: ScissorPx, b: ScissorPx) -> ScissorPx {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

/// Applies `scissor` to the render pass.
pub(crate) fn set_scissor(render_pass: &mut wgpu::RenderPass<'static>, scissor: ScissorPx) {
    render_pass.set_scissor_rect(
        scissor.0 as u32,
        scissor.1 as u32,
        scissor.2 as u32,
        scissor.3 as u32,
    );
}

/// The content pipeline and stencil reference for a nesting `level` (see the
/// mask scheme in `resources.rs`).
pub(crate) fn content_target(
    pipelines: &ContentPipelines,
    level: u32,
) -> (&wgpu::RenderPipeline, u32) {
    if level >= 2 {
        (&pipelines.nested, level)
    } else if level == 1 {
        (&pipelines.outer, 0)
    } else {
        (&pipelines.plain, 0)
    }
}

/// Binds the solid pipeline, the screen uniform, and the mesh buffers, then
/// issues the draw.
pub(crate) fn draw_solid(
    render_pass: &mut wgpu::RenderPass<'static>,
    pipeline: &wgpu::RenderPipeline,
    screen: &wgpu::BindGroup,
    vertices: &wgpu::Buffer,
    indices: &wgpu::Buffer,
    index_count: u32,
) {
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, screen, &[]);
    render_pass.set_vertex_buffer(0, vertices.slice(..));
    render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
    render_pass.draw_indexed(0..index_count, 0, 0..1);
}

/// Binds a gradient/image pipeline with its paint bind group (group 1), then
/// issues the draw.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_painted(
    render_pass: &mut wgpu::RenderPass<'static>,
    pipeline: &wgpu::RenderPipeline,
    screen: &wgpu::BindGroup,
    paint: &wgpu::BindGroup,
    vertices: &wgpu::Buffer,
    indices: &wgpu::Buffer,
    index_count: u32,
) {
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, screen, &[]);
    render_pass.set_bind_group(1, paint, &[]);
    render_pass.set_vertex_buffer(0, vertices.slice(..));
    render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
    render_pass.draw_indexed(0..index_count, 0, 0..1);
}
