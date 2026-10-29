//! The `egui_wgpu` callback that replays one recorded [`Page`].
//!
//! `prepare` drains finished image uploads, resolves the page's vertex/index
//! buffers (cached across frames in [`Prepared`]), and writes this callback's
//! own transform uniform. An image whose texture is still loading is drawn as a
//! solid placeholder quad in its place, and a repaint is requested until it
//! lands. A cached [`Prepared`] whose placeholder's texture has since arrived
//! is dropped and rebuilt with the real texture. `paint` re-establishes the
//! full-screen viewport and the egui clip scissor, then issues one draw per
//! prepared command with its pipeline.

use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

use super::canvas::{Command, Page};
use super::draw;
use super::resources::{
    CanvasVertex, GpuResources, ImageKey, ImagePriority, ImageState, mesh_buffers,
};

/// Replays one recorded page into egui's render pass.
pub(crate) struct PageCallback {
    /// Shared pipelines, uniform, and texture caches.
    pub(crate) resources: Arc<GpuResources>,
    /// The recorded page.
    pub(crate) page: Arc<Page>,
    /// The live transform `[ratio, offset_x, offset_y, pad]` mapping the
    /// recording onto the current page position and zoom.
    pub(crate) transform: [f32; 4],
    /// Context used to request repaints while image textures are loading.
    pub(crate) ctx: egui::Context,
    /// Fill color of the placeholder quad drawn in place of a loading image.
    pub(crate) placeholder_fill: egui::Color32,
    /// Visible screen-space rectangle for this callback.
    pub(crate) visible_rect: egui::Rect,
    /// Per-page GPU buffers, resolved by `prepare` (cached across frames by
    /// [`GpuResources::prepared_page`]).
    pub(crate) prepared: Mutex<Option<Arc<Prepared>>>,
    /// This callback's own transform uniform and group-0 bind group, created
    /// lazily in `prepare`.
    ///
    /// It is per-callback rather than per-recording: `egui-wgpu` runs every
    /// `prepare` before any `paint`, so two callbacks sharing one recording
    /// must not share a transform buffer or the last write would win.
    pub(crate) transform_gpu: Mutex<Option<(wgpu::Buffer, wgpu::BindGroup)>>,
}

impl PageCallback {
    /// Creates a callback for `page` drawing through `resources`.
    pub(crate) fn new(
        resources: Arc<GpuResources>,
        page: Arc<Page>,
        transform: [f32; 4],
        ctx: egui::Context,
        placeholder_fill: egui::Color32,
        visible_rect: egui::Rect,
    ) -> Self {
        Self {
            resources,
            page,
            transform,
            ctx,
            placeholder_fill,
            visible_rect,
            prepared: Mutex::new(None),
            transform_gpu: Mutex::new(None),
        }
    }

    fn image_visible(&self, vertices: &[CanvasVertex]) -> bool {
        let rect = draw::transform_rect(
            super::quad_rect(vertices),
            self.transform[0],
            egui::vec2(self.transform[1], self.transform[2]),
        );
        rect.intersects(self.visible_rect)
    }

    fn request_visible_images(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        for command in &self.page.commands {
            let Command::Image {
                vertices,
                key,
                image,
                ..
            } = command
            else {
                continue;
            };
            if self.image_visible(vertices) {
                let _ = self.resources.image_state(
                    device,
                    queue,
                    key,
                    image,
                    ImagePriority::Visible,
                    true,
                );
            }
        }
    }
}

/// GPU buffers for one prepare phase, cached across frames by page id.
pub(crate) struct Prepared {
    /// Prepared commands in paint order.
    pub(crate) commands: Vec<PreparedCommand>,
    /// Every image command's texture key, ready or pending (deduplicated), so
    /// completing or evicting a key drops the entries that depicted it — a
    /// placeholder for a pending key, or a now-stale texture for an evicted
    /// one.
    pub(crate) image_keys: Vec<ImageKey>,
    /// Keys drawn as placeholders in this `Prepared`. If one becomes ready
    /// before the entry is used, the entry is dropped and rebuilt so the real
    /// texture is drawn.
    pub(crate) placeholder_keys: Vec<ImageKey>,
}

/// Serializes a transform into the 16-byte uniform layout the shaders expect.
fn transform_bytes(transform: [f32; 4]) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    for (index, value) in transform.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// One prepared command, with buffers already uploaded.
pub(crate) enum PreparedCommand {
    /// A solid mesh.
    Solid {
        /// Vertex buffer.
        vertices: wgpu::Buffer,
        /// Index buffer.
        indices: wgpu::Buffer,
        /// Index count.
        index_count: u32,
    },
    /// A gradient mesh with its (cached) gradient bind group.
    Gradient {
        /// Vertex buffer.
        vertices: wgpu::Buffer,
        /// Index buffer.
        indices: wgpu::Buffer,
        /// Index count.
        index_count: u32,
        /// Gradient parameter/LUT bind group.
        bind_group: wgpu::BindGroup,
    },
    /// A textured image quad with its (cached) image bind group.
    Image {
        /// Vertex buffer.
        vertices: wgpu::Buffer,
        /// Index buffer.
        indices: wgpu::Buffer,
        /// Index count.
        index_count: u32,
        /// Image texture bind group.
        bind_group: wgpu::BindGroup,
    },
    /// Scissor intersection in screen points.
    PushScissor {
        /// Clip rectangle in screen points.
        rect: egui::Rect,
    },
    /// Restore the previous scissor.
    PopScissor,
    /// Stencil-mask triangles that increment the stencil.
    PushStencil {
        /// Vertex buffer.
        vertices: wgpu::Buffer,
        /// Index buffer.
        indices: wgpu::Buffer,
        /// Index count.
        index_count: u32,
    },
    /// Stencil-mask triangles that decrement the stencil, referencing a push.
    PopStencil {
        /// Index of the matching [`PreparedCommand::PushStencil`].
        push: usize,
    },
}

impl CallbackTrait for PageCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        self.resources.drain_image_results();
        self.request_visible_images(device, queue);
        self.resources.update_screen(
            queue,
            screen_descriptor.size_in_pixels,
            screen_descriptor.pixels_per_point,
        );

        {
            let mut transform_gpu = self
                .transform_gpu
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if transform_gpu.is_none() {
                *transform_gpu = Some(self.resources.create_transform_bind_group(device));
            }
            if let Some((buffer, _)) = transform_gpu.as_ref() {
                queue.write_buffer(buffer, 0, &transform_bytes(self.transform));
            }
        }

        let mut guard = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(prepared) = self.resources.prepared_page(self.page.id) {
            let stale = prepared
                .placeholder_keys
                .iter()
                .any(|key| self.resources.is_image_ready(key));
            if !stale {
                *guard = Some(prepared);
                if self.resources.has_pending_images() {
                    self.ctx.request_repaint();
                }
                return Vec::new();
            }
            // A placeholder's image finished while this entry was cached; fall
            // through and rebuild with the real texture.
        }

        self.resources.record_prepared_rebuild();
        let mut prepared = Prepared {
            commands: Vec::with_capacity(self.page.commands.len()),
            image_keys: Vec::new(),
            placeholder_keys: Vec::new(),
        };
        let mut mask_stack: Vec<Option<usize>> = Vec::new();
        for command in &self.page.commands {
            match command {
                Command::Solid { vertices, indices } => {
                    if let Some((vertices, indices, index_count)) =
                        mesh_buffers(device, vertices, indices)
                    {
                        prepared.commands.push(PreparedCommand::Solid {
                            vertices,
                            indices,
                            index_count,
                        });
                    }
                }
                Command::Gradient {
                    vertices,
                    indices,
                    plan,
                } => {
                    let bind_group = self.resources.ensure_gradient(device, queue, plan);
                    if let Some((vertices, indices, index_count)) =
                        mesh_buffers(device, vertices, indices)
                    {
                        prepared.commands.push(PreparedCommand::Gradient {
                            vertices,
                            indices,
                            index_count,
                            bind_group,
                        });
                    }
                }
                Command::Image {
                    vertices,
                    indices,
                    key,
                    image,
                } => {
                    let visible = self.image_visible(vertices);
                    let state = self.resources.image_state(
                        device,
                        queue,
                        key,
                        image,
                        if visible {
                            ImagePriority::Visible
                        } else {
                            ImagePriority::Background
                        },
                        visible,
                    );
                    if !prepared.image_keys.contains(key) {
                        prepared.image_keys.push(key.clone());
                    }
                    match state {
                        ImageState::Ready(bind_group) => {
                            if let Some((vertices, indices, index_count)) =
                                mesh_buffers(device, vertices, indices)
                            {
                                prepared.commands.push(PreparedCommand::Image {
                                    vertices,
                                    indices,
                                    index_count,
                                    bind_group,
                                });
                            }
                        }
                        ImageState::Pending => {
                            if !prepared.placeholder_keys.contains(key) {
                                prepared.placeholder_keys.push(key.clone());
                            }
                            let placeholder: Vec<CanvasVertex> = vertices
                                .iter()
                                .map(|vertex| CanvasVertex {
                                    pos: vertex.pos,
                                    local: vertex.local,
                                    color: self.placeholder_fill.to_array(),
                                })
                                .collect();
                            if let Some((vertices, indices, index_count)) =
                                mesh_buffers(device, &placeholder, indices)
                            {
                                prepared.commands.push(PreparedCommand::Solid {
                                    vertices,
                                    indices,
                                    index_count,
                                });
                            }
                        }
                    }
                }
                Command::PushScissor { rect } => {
                    prepared
                        .commands
                        .push(PreparedCommand::PushScissor { rect: *rect });
                }
                Command::PopScissor => prepared.commands.push(PreparedCommand::PopScissor),
                Command::PushClip { vertices, indices } => {
                    match mesh_buffers(device, vertices, indices) {
                        Some((vertices, indices, index_count)) => {
                            mask_stack.push(Some(prepared.commands.len()));
                            prepared.commands.push(PreparedCommand::PushStencil {
                                vertices,
                                indices,
                                index_count,
                            });
                        }
                        None => mask_stack.push(None),
                    }
                }
                Command::PopClip => {
                    if let Some(Some(push)) = mask_stack.pop() {
                        prepared.commands.push(PreparedCommand::PopStencil { push });
                    }
                }
            }
        }

        let prepared = Arc::new(prepared);
        self.resources
            .store_prepared(self.page.id, prepared.clone());
        *guard = Some(prepared.clone());
        if self.resources.has_pending_images() {
            self.ctx.request_repaint();
        }
        // An image may have finished while this entry was being built, leaving a
        // stale placeholder. Drop the just-stored entry and repaint so the next
        // frame rebuilds with the real texture; this frame still draws the
        // placeholder. A callback that already prepared earlier in the same
        // frame can still show one stale frame — that is cosmetic and bounded.
        let stale = prepared
            .placeholder_keys
            .iter()
            .any(|key| self.resources.is_image_ready(key));
        if stale {
            self.resources.drop_prepared(self.page.id);
            self.ctx.request_repaint();
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        _callback_resources: &CallbackResources,
    ) {
        let guard = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(prepared) = guard.as_ref() else {
            return;
        };
        let transform_gpu = self
            .transform_gpu
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((_, page_bind_group)) = transform_gpu.as_ref() else {
            return;
        };

        // Vertices are absolute screen points, so draw over the whole screen
        // and start from the callback's clip scissor.
        render_pass.set_viewport(
            0.0,
            0.0,
            info.screen_size_px[0] as f32,
            info.screen_size_px[1] as f32,
            0.0,
            1.0,
        );
        let screen = info.screen_size_px;
        let mut current = draw::clip_scissor(&info);
        draw::set_scissor(render_pass, current);
        let mut saved_scissors: Vec<draw::ScissorPx> = Vec::new();
        let mut level: u32 = 0;

        for command in &prepared.commands {
            match command {
                PreparedCommand::Solid {
                    vertices,
                    indices,
                    index_count,
                } => {
                    let (pipeline, reference) =
                        draw::content_target(&self.resources.pipelines.solid, level);
                    render_pass.set_stencil_reference(reference);
                    draw::draw_solid(
                        render_pass,
                        pipeline,
                        page_bind_group,
                        vertices,
                        indices,
                        *index_count,
                    );
                }
                PreparedCommand::Gradient {
                    vertices,
                    indices,
                    index_count,
                    bind_group,
                } => {
                    let (pipeline, reference) =
                        draw::content_target(&self.resources.pipelines.gradient, level);
                    render_pass.set_stencil_reference(reference);
                    draw::draw_painted(
                        render_pass,
                        pipeline,
                        page_bind_group,
                        bind_group,
                        vertices,
                        indices,
                        *index_count,
                    );
                }
                PreparedCommand::Image {
                    vertices,
                    indices,
                    index_count,
                    bind_group,
                } => {
                    let (pipeline, reference) =
                        draw::content_target(&self.resources.pipelines.image, level);
                    render_pass.set_stencil_reference(reference);
                    draw::draw_painted(
                        render_pass,
                        pipeline,
                        page_bind_group,
                        bind_group,
                        vertices,
                        indices,
                        *index_count,
                    );
                }
                PreparedCommand::PushScissor { rect } => {
                    saved_scissors.push(current);
                    let rect = draw::transform_rect(
                        *rect,
                        self.transform[0],
                        egui::vec2(self.transform[1], self.transform[2]),
                    );
                    current = draw::intersect(
                        current,
                        draw::rect_scissor(rect, info.pixels_per_point, screen),
                    );
                    draw::set_scissor(render_pass, current);
                }
                PreparedCommand::PopScissor => {
                    if let Some(previous) = saved_scissors.pop() {
                        current = previous;
                        draw::set_scissor(render_pass, current);
                    }
                }
                PreparedCommand::PushStencil {
                    vertices,
                    indices,
                    index_count,
                } => {
                    let pipeline = if level == 0 {
                        render_pass.set_stencil_reference(0);
                        &self.resources.pipelines.mask_push_outer
                    } else {
                        render_pass.set_stencil_reference(level);
                        &self.resources.pipelines.mask_push_nested
                    };
                    draw::draw_solid(
                        render_pass,
                        pipeline,
                        page_bind_group,
                        vertices,
                        indices,
                        *index_count,
                    );
                    level += 1;
                }
                PreparedCommand::PopStencil { push } => {
                    if let Some(PreparedCommand::PushStencil {
                        vertices,
                        indices,
                        index_count,
                    }) = prepared.commands.get(*push)
                    {
                        let pipeline = if level <= 1 {
                            render_pass.set_stencil_reference(0);
                            &self.resources.pipelines.mask_pop_outer
                        } else {
                            render_pass.set_stencil_reference(level);
                            &self.resources.pipelines.mask_pop_nested
                        };
                        draw::draw_solid(
                            render_pass,
                            pipeline,
                            page_bind_group,
                            vertices,
                            indices,
                            *index_count,
                        );
                    }
                    level = level.saturating_sub(1);
                }
            }
        }
    }
}
