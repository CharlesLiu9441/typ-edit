//! Shared GPU rendering-fidelity harness for `typst-gpu-canvas`.
//!
//! Included (`mod common;`) by the GPU fidelity integration test. It compares a
//! captured egui frame against `typst-render`'s rasterization of the same frame
//! with `dify`'s anti-aliasing-aware per-pixel diff. It provides:
//!
//! * [`capture_gpu`] - render a [`Frame`] through a headless, 4x-MSAA,
//!   stencil-capable wgpu harness that executes the canvas callback.
//! * [`reference`] - rasterize the same frame through `typst-render` at its
//!   default 2 px/pt.
//! * [`over_white`] - composite a premultiplied RGBA image over opaque white.
//! * [`diff`] / [`diff_and_assert_at`] - run `dify` and assert the
//!   failing-pixel count stays within a calibrated bound, dumping
//!   reference/capture/diff PNGs on failure.
//!
//! Both harness and reference produce a `size * scale` pixel image; every
//! fixture in the suite is a 128-point frame rendered at `scale = 2.0`, so both
//! sides are 256x256. The capture window is `frame.size() * scale` logical
//! points at the harness's default 1.0 pixels-per-point, mirroring the
//! reference's pixel grid exactly.
//!
//! Not every function is used by every test binary, so the module silences
//! `dead_code`.
#![allow(dead_code)]

pub mod fixture;
pub mod world;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::TestRenderer;
use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use egui_wgpu::wgpu;
use image::RgbaImage;
use typst_gpu_canvas::{FrameBvh, GpuCanvas, GpuCanvasConfig, ImagePipelineStats, ImageStrategy};
use typst_library::foundations::{Content, Smart};
use typst_library::layout::{Frame, Sides};

/// The default per-pixel YIQ distance threshold (kittest's default).
pub const THRESHOLD: f32 = 0.6;

/// Timeout for waiting on the GPU to finish.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Serializes GPU capture so parallel tests do not contend for one device.
pub static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// The MSAA sample count every fidelity fixture is captured with.
pub const MSAA_SAMPLES: u32 = GpuTestRenderer::MSAA_SAMPLES;

/// Builds the render state the fidelity harness uses (4x MSAA, `Stencil8`), for
/// capability assertions that do not need a full capture.
pub fn harness_render_state() -> egui_wgpu::RenderState {
    GpuTestRenderer::new().render_state
}

/// The GPU-canvas-capable headless test renderer.
///
/// Draws into a 4x MSAA color target and configures `egui_wgpu` for
/// `msaa_samples = 4` so the GPU canvas pipelines (created with the same sample
/// count) are compatible. The pass keeps a `Stencil8` attachment because the
/// canvas pipelines declare that depth-stencil format.
struct GpuTestRenderer {
    render_state: egui_wgpu::RenderState,
}

impl GpuTestRenderer {
    /// The MSAA sample count the harness configures and `capture_gpu` passes
    /// to [`GpuCanvasConfig`].
    const MSAA_SAMPLES: u32 = 4;

    fn new() -> Self {
        let mut options = egui_wgpu::RendererOptions::PREDICTABLE;
        options.msaa_samples = Self::MSAA_SAMPLES;
        options.depth_stencil_format = Some(wgpu::TextureFormat::Stencil8);
        options.predictable_texture_filtering = false;
        Self {
            render_state: create_render_state(default_wgpu_setup(), options),
        }
    }
}

impl TestRenderer for GpuTestRenderer {
    fn handle_delta(&mut self, delta: &mut egui::TexturesDelta) {
        let mut renderer = self.render_state.renderer.write();
        for (id, images) in delta.set.drain() {
            for image in images {
                renderer.update_texture(
                    &self.render_state.device,
                    &self.render_state.queue,
                    id,
                    &image,
                );
            }
        }
        for id in delta.free.drain() {
            renderer.free_texture(&id);
        }
    }

    fn render(
        &mut self,
        ctx: &egui::Context,
        output: &egui::FullOutput,
    ) -> Result<RgbaImage, String> {
        let mut renderer = self.render_state.renderer.write();

        let mut encoder =
            self.render_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("typst-gpu-canvas command encoder"),
                });

        let size = ctx.content_rect().size() * ctx.pixels_per_point();
        let screen = egui_wgpu::ScreenDescriptor {
            pixels_per_point: ctx.pixels_per_point(),
            size_in_pixels: [size.x.round() as u32, size.y.round() as u32],
        };

        let tessellated = ctx.tessellate(output.shapes.clone(), ctx.pixels_per_point());

        let user_buffers = renderer.update_buffers(
            &self.render_state.device,
            &self.render_state.queue,
            &mut encoder,
            &tessellated,
            &screen,
        );

        let extent = wgpu::Extent3d {
            width: screen.size_in_pixels[0],
            height: screen.size_in_pixels[1],
            depth_or_array_layers: 1,
        };
        let sample_count = Self::MSAA_SAMPLES;

        let color = self
            .render_state
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("typst-gpu-canvas msaa color"),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: self.render_state.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

        let resolve = self
            .render_state
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("typst-gpu-canvas resolve"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.render_state.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());

        let stencil = self
            .render_state
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("typst-gpu-canvas stencil"),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Stencil8,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
        let stencil_view = stencil.create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("typst-gpu-canvas pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &color_view,
                        resolve_target: Some(&resolve_view),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Discard,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &stencil_view,
                        depth_ops: None,
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0),
                            store: wgpu::StoreOp::Discard,
                        }),
                    }),
                    ..Default::default()
                })
                .forget_lifetime();

            renderer.render(&mut pass, &tessellated, &screen);
        }

        self.render_state.queue.submit(
            user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );

        self.render_state
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(WAIT_TIMEOUT),
            })
            .map_err(|err| format!("PollError: {err}"))?;

        Ok(texture_to_image(
            &self.render_state.device,
            &self.render_state.queue,
            &resolve,
        ))
    }
}

/// Reads a `Rgba8Unorm` texture back into an image, honoring the 256-byte row
/// alignment of `copy_texture_to_buffer`.
fn texture_to_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> RgbaImage {
    let width = texture.width() as usize;
    let height = texture.height() as usize;
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("typst-gpu-canvas readback"),
        size: (padded_bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("typst-gpu-canvas readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &output_buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row as u32),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: texture.width(),
            height: texture.height(),
            depth_or_array_layers: 1,
        },
    );

    let submission_index = queue.submit(std::iter::once(encoder.finish()));

    let buffer_slice = output_buffer.slice(..);
    let (sender, receiver) = channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        drop(sender.send(result));
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission_index),
            timeout: Some(WAIT_TIMEOUT),
        })
        .expect("Failed to poll device");
    receiver.recv().unwrap().unwrap();

    let data = buffer_slice
        .get_mapped_range()
        .expect("Failed to get mapped range");
    let pixels: Vec<u8> = data
        .chunks_exact(padded_bytes_per_row)
        .flat_map(|row| row.iter().take(unpadded_bytes_per_row).copied())
        .collect();
    RgbaImage::from_raw(texture.width(), texture.height(), pixels)
        .expect("readback matches the texture size")
}

/// State driving the capture harness: one canvas, one page, and the sequence
/// of `(page position, scale)` steps to paint, one per UI frame.
struct CaptureState {
    canvas: GpuCanvas,
    bvh: FrameBvh,
    steps: Vec<(Pos2, f32)>,
    frame_index: usize,
    size: Vec2,
    clip: Rect,
}

/// Captures `frame` through [`GpuCanvas`] into a 4x-MSAA `Rgba8Unorm` window of
/// `round(frame.size() * scale)` pixels, over an opaque white ground.
pub fn capture_gpu(frame: Frame, scale: f32) -> RgbaImage {
    capture_gpu_at(frame, scale, Pos2::ZERO)
}

/// Captures `frame` freshly painted at `page_pos` (a cold recording cache).
pub fn capture_gpu_at(frame: Frame, scale: f32, page_pos: Pos2) -> RgbaImage {
    capture_gpu_positions(frame, scale, &[page_pos])
}

/// Captures `frame` after painting it once per entry of `positions`, all at
/// `scale`, in order (one UI frame each), rendering only the final frame.
pub fn capture_gpu_positions(frame: Frame, scale: f32, positions: &[Pos2]) -> RgbaImage {
    let steps: Vec<(Pos2, f32)> = if positions.is_empty() {
        vec![(Pos2::ZERO, scale)]
    } else {
        positions.iter().map(|pos| (*pos, scale)).collect()
    };
    capture_gpu_steps(frame, &steps)
}

/// Captures `frame` after painting it once per `(page_pos, scale)` step, in
/// order (one UI frame each), rendering only the final frame.
///
/// The window is sized from the largest step scale so every frame fits. With
/// in-bucket scale changes the recording cache hits after the first frame, so
/// this exercises the GPU transform-replay path (a hit reuses the recording and
/// transforms it) rather than a fresh recording.
pub fn capture_gpu_steps(frame: Frame, steps: &[(Pos2, f32)]) -> RgbaImage {
    capture_gpu_steps_with_stats(frame, steps).0
}

/// Captures a frame sequence and returns the image plus image-pipeline counters.
pub fn capture_gpu_steps_with_stats(
    frame: Frame,
    steps: &[(Pos2, f32)],
) -> (RgbaImage, ImagePipelineStats) {
    capture_gpu_steps_with_strategy_stats(frame, steps, ImageStrategy::Cpu)
}

/// Captures a frame sequence with an explicit image strategy.
pub fn capture_gpu_steps_with_strategy_stats(
    frame: Frame,
    steps: &[(Pos2, f32)],
    image_strategy: ImageStrategy,
) -> (RgbaImage, ImagePipelineStats) {
    capture_gpu_steps_with_strategy_clip_stats(frame, steps, image_strategy, Rect::EVERYTHING)
}

pub fn capture_gpu_steps_with_strategy_clip_stats(
    frame: Frame,
    steps: &[(Pos2, f32)],
    image_strategy: ImageStrategy,
    clip: Rect,
) -> (RgbaImage, ImagePipelineStats) {
    let _guard = CAPTURE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let steps = if steps.is_empty() {
        vec![(Pos2::ZERO, 1.0)]
    } else {
        steps.to_vec()
    };
    let window_scale = steps
        .iter()
        .map(|(_, scale)| *scale)
        .fold(0.0_f32, f32::max);
    let px = (frame.size().x.to_pt() * f64::from(window_scale)).round() as u32;
    let py = (frame.size().y.to_pt() * f64::from(window_scale)).round() as u32;
    assert!(px > 0 && py > 0, "capture window must be non-empty");

    let count = steps.len();

    let bvh = FrameBvh::new(frame);
    let renderer = GpuTestRenderer::new();
    let canvas = GpuCanvas::new(
        &renderer.render_state,
        GpuCanvasConfig {
            msaa_samples: GpuTestRenderer::MSAA_SAMPLES,
            target_format: renderer.render_state.target_format,
            loading_label: "加载中",
            image_strategy,
            image_max_side: 4096,
        },
    )
    .expect("the fidelity harness adapter supports the canvas render pass");
    let state = CaptureState {
        canvas,
        bvh,
        steps,
        frame_index: 0,
        size: Vec2::new(px as f32, py as f32),
        clip,
    };
    let mut harness = egui_kittest::Harness::builder()
        .with_size(state.size)
        .renderer(renderer)
        .build_ui_state(
            move |ui, state: &mut CaptureState| {
                let (page_pos, scale) = state.steps[state.frame_index.min(state.steps.len() - 1)];
                ui.set_clip_rect(state.clip);
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::ZERO, state.size),
                    egui::CornerRadius::ZERO,
                    Color32::WHITE,
                );
                state
                    .canvas
                    .paint_page(ui, &state.bvh, page_pos, scale)
                    .expect("gpu paint_page succeeds");
            },
            state,
        );
    for index in 1..count {
        harness.state_mut().frame_index = index;
        harness.step();
    }
    // `run_ok` (not `run`) settles without panicking when a still-loading image
    // keeps requesting repaints past the harness's default step budget.
    harness.run_ok();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Only accept a frame whose UI phase saw no pending uploads: such a
        // frame can draw neither a placeholder nor a loading label, and any
        // upload that landed during the render pass was drained before its
        // prepare, so the painted image is final. A frame drawn while an
        // upload was still pending may keep its label even though the very
        // same render pass swapped the real texture in.
        let pending_before = harness.state_mut().canvas.has_pending_images();
        harness.step();
        let image = harness.render().expect("wgpu render");
        if !pending_before && !harness.state_mut().canvas.has_pending_images() {
            let stats = harness.state_mut().canvas.image_pipeline_stats();
            return (image, stats);
        }
        assert!(
            Instant::now() < deadline,
            "image uploads did not finish within the timeout"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Captures a single frame and returns the image plus image-pipeline counters.
pub fn capture_gpu_with_stats(frame: Frame, scale: f32) -> (RgbaImage, ImagePipelineStats) {
    capture_gpu_steps_with_stats(frame, &[(Pos2::ZERO, scale)])
}

/// Captures one frame with an explicit image strategy.
pub fn capture_gpu_with_strategy(
    frame: Frame,
    scale: f32,
    image_strategy: ImageStrategy,
) -> RgbaImage {
    capture_gpu_steps_with_strategy_stats(frame, &[(Pos2::ZERO, scale)], image_strategy).0
}

pub fn capture_gpu_with_clip_stats(
    frame: Frame,
    scale: f32,
    clip: Rect,
) -> (RgbaImage, ImagePipelineStats) {
    capture_gpu_steps_with_strategy_clip_stats(
        frame,
        &[(Pos2::ZERO, scale)],
        ImageStrategy::Cpu,
        clip,
    )
}

/// Renders `frame` through `typst-render` at its default 2 px/pt.
///
/// The page is filled with white (`Smart::Auto`) and framed exactly at the
/// frame's size, so the result is the fixture over opaque white on the same
/// `2 * size` pixel grid as [`capture_gpu`].
pub fn reference(frame: Frame) -> RgbaImage {
    let page = typst_layout::Page {
        frame,
        bleed: Sides::default(),
        fill: Smart::Auto,
        numbering: None,
        supplement: Content::empty(),
        number: 1,
    };
    let pixmap = typst_render::render(&page, &typst_render::RenderOptions::default());
    let (width, height) = (pixmap.width(), pixmap.height());
    RgbaImage::from_raw(width, height, pixmap.take()).expect("pixmap is RGBA8")
}

/// Composites a premultiplied RGBA image over opaque white.
pub fn over_white(mut image: RgbaImage) -> RgbaImage {
    for pixel in image.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let backdrop = (1.0 - f32::from(a) / 255.0) * 255.0;
        let channel = |c: u8| (f32::from(c) + backdrop).round().clamp(0.0, 255.0) as u8;
        *pixel = image::Rgba([channel(r), channel(g), channel(b), u8::MAX]);
    }
    image
}

/// Where failing fixtures are dumped for inspection, one directory per suite.
pub fn output_dir(suite: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/{suite}-fidelity"))
}

/// Computes the failing-pixel count between two composited images.
pub fn diff(reference: &RgbaImage, captured: &RgbaImage) -> i32 {
    dify::diff::get_results(
        reference.clone(),
        captured.clone(),
        THRESHOLD,
        true,
        None,
        &None,
        &None,
    )
    .map_or(0, |(count, _)| count)
}

/// Renders, diffs and asserts one fixture, dumping diagnostics to `output_dir`
/// on failure.
pub fn diff_and_assert_at(
    output_dir: &Path,
    name: &str,
    reference: &RgbaImage,
    captured: &RgbaImage,
    max_failed: i32,
) -> i32 {
    let result = dify::diff::get_results(
        reference.clone(),
        captured.clone(),
        THRESHOLD,
        true,
        None,
        &None,
        &None,
    );
    let failed = result.as_ref().map_or(0, |(count, _)| *count);

    eprintln!(
        "fidelity {name}: threshold={THRESHOLD} failed_pixels={failed} max_failed={max_failed}"
    );

    if failed <= max_failed {
        return failed;
    }

    std::fs::create_dir_all(output_dir).expect("create fidelity output dir");
    let reference_path = output_dir.join(format!("{name}.reference.png"));
    let captured_path = output_dir.join(format!("{name}.captured.png"));
    reference.save(&reference_path).expect("save reference");
    captured.save(&captured_path).expect("save capture");

    let mut message = format!(
        "{name}: {failed} pixels exceed threshold {THRESHOLD} (max {max_failed}); wrote {} and {}",
        reference_path.display(),
        captured_path.display(),
    );
    if let Some((_, diff)) = &result {
        let diff_path = output_dir.join(format!("{name}.diff.png"));
        diff.save(&diff_path).expect("save diff");
        message.push_str(&format!(" and {}", diff_path.display()));
    }
    panic!("{message}");
}
