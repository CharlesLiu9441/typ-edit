//! GPU canvas renderer: a self-contained wgpu replay path for typst frames.
//!
//! [`GpuCanvas::paint_page`] records a page's items in typst paint order and
//! appends one `egui_wgpu` callback that replays them with per-paint pipelines
//! and MSAA anti-aliasing. Recordings are view-independent: a recording is
//! cached by BVH id, scale bucket, and pixels-per-point, and the callback pans
//! and zooms it on the GPU through a per-callback transform uniform. The
//! transformed recording bounds feed egui's callback viewport; `egui-wgpu`
//! skips `paint` for a callback whose viewport clamps to zero.
//!
//! A page is recorded **in full**: viewport culling is not applied while
//! recording, so any unsupported item anywhere on a painted page makes
//! `paint_page` return [`RenderError::UnsupportedCanvasContent`] and the host
//! falls back to raster for the whole generation (previously viewport culling
//! could hide off-screen unsupported items).
//!
//! Geometry is tessellated in screen space by the internal tessellator with
//! the feather disabled; MSAA (the host render pass's sample count) provides
//! coverage.
//!
//! # Requirements
//!
//! * The host render pass must be configured with the same MSAA sample count
//!   and color format as [`GpuCanvasConfig`], and must carry a `Stencil8`
//!   attachment (curve clips and the depth-stencil format of every canvas
//!   pipeline require it). wgpu requires a pipeline's depth-stencil format to
//!   match the pass exactly.

use std::sync::Arc;

use egui::Ui;
use egui_wgpu::RenderState;
use egui_wgpu::wgpu;

use crate::bvh::FrameBvh;
use crate::error::RenderError;
use crate::view::{View, scale_bucket};

mod callback;
mod canvas;
mod capability;
mod draw;
mod gradient;
mod loader;
mod mipmap;
mod paint;
mod premultiply;
mod resources;
mod svg;
mod traverse;

use callback::PageCallback;
use canvas::{Command, Page};
use resources::GpuResources;

pub(crate) use resources::{ImageKey, ImageMode};

pub use capability::is_supported;

/// Counters for the image conversion and upload pipeline.
///
/// The counters are cheap atomics and are intended for diagnostics and
/// regression tests, not for per-frame UI rendering. Times are wall-clock
/// nanoseconds spent converting/uploading on the caller or loader thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImagePipelineStats {
    /// Image lookups that found a ready texture.
    pub cache_hits: u64,
    /// Image lookups that did not find a ready texture.
    pub cache_misses: u64,
    /// Conversions performed inline on the caller (small-image path).
    pub sync_conversions: u64,
    /// Conversions enqueued on the loader thread.
    pub async_jobs: u64,
    /// Conversions that completed successfully.
    pub completed: u64,
    /// Conversions that panicked.
    pub failed: u64,
    /// Total conversion time across sync and async jobs.
    pub conversion_ns: u64,
    /// Full prepared-page rebuilds caused by image state changes.
    pub prepared_rebuilds: u64,
    /// Image textures evicted by the LRU.
    pub evictions: u64,
    /// Bytes uploaded into image textures, including mip levels.
    pub upload_bytes: u64,
    /// Image conversions currently in flight.
    pub in_flight: u64,
    /// Ready image textures currently cached.
    pub cache_entries: u64,
    /// GPU bytes currently held by cached image textures.
    pub cache_bytes: u64,
}

/// Selects how raster images enter the GPU canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageStrategy {
    /// Use the GPU source-texture path for large smooth images and the exact
    /// CPU target-size path for small, pixelated, or thumbnail-like images.
    #[default]
    Auto,
    /// Keep the exact CPU target-size path for every image.
    Cpu,
    /// Force the GPU source-texture path for raster images.
    Gpu,
}

/// One recorded page produced off the UI thread.
///
/// The recording is view-independent; the host replays it with the transform
/// for the current position and zoom via [`GpuCanvas::append_recording`].
pub struct PageRecording {
    page: Arc<Page>,
}

impl PageRecording {
    pub fn is_empty(&self) -> bool {
        self.page.is_empty()
    }

    pub(crate) fn arc(&self) -> Arc<Page> {
        Arc::clone(&self.page)
    }
}

/// Records one page at `page_pos` and `scale` without touching the caller's UI
/// painter. The result can be sent from a recording worker and replayed later.
pub fn record_page(
    bvh: &FrameBvh,
    page_pos: egui::Pos2,
    scale: f32,
    ppp: f32,
    ctx: &egui::Context,
    max_texture_side: u32,
) -> Result<PageRecording, RenderError> {
    record_page_with_strategy(
        bvh,
        page_pos,
        scale,
        ctx,
        RecordPageOptions {
            ppp,
            max_texture_side,
            image_max_side: crate::image_helpers::DEFAULT_IMAGE_MAX_SIDE,
            image_strategy: ImageStrategy::Auto,
        },
    )
}

/// Records one page with a caller-selected source-image side limit.
pub fn record_page_with_image_max_side(
    bvh: &FrameBvh,
    page_pos: egui::Pos2,
    scale: f32,
    ppp: f32,
    ctx: &egui::Context,
    max_texture_side: u32,
    image_max_side: u32,
) -> Result<PageRecording, RenderError> {
    record_page_with_strategy(
        bvh,
        page_pos,
        scale,
        ctx,
        RecordPageOptions {
            ppp,
            max_texture_side,
            image_max_side,
            image_strategy: ImageStrategy::Auto,
        },
    )
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RecordPageOptions {
    pub(crate) ppp: f32,
    pub(crate) max_texture_side: u32,
    pub(crate) image_max_side: u32,
    pub(crate) image_strategy: ImageStrategy,
}

/// Records one page with explicit recording options.
pub(crate) fn record_page_with_strategy(
    bvh: &FrameBvh,
    page_pos: egui::Pos2,
    scale: f32,
    ctx: &egui::Context,
    options: RecordPageOptions,
) -> Result<PageRecording, RenderError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(RenderError::NonFiniteScale);
    }
    if !page_pos.x.is_finite() || !page_pos.y.is_finite() {
        return Err(RenderError::NonFinitePosition);
    }
    let view = View::root(page_pos, scale, bvh.frame().size());
    let mut page = Page::default();
    traverse::record_page(
        bvh,
        &view,
        options.ppp,
        ctx,
        paint::ImageOptions {
            max_texture_side: options.max_texture_side,
            max_source_side: options.image_max_side,
            strategy: options.image_strategy,
        },
        &mut page,
    )?;
    page.record_pos = page_pos;
    page.record_scale = scale;
    Ok(PageRecording {
        page: Arc::new(page),
    })
}

/// Host render-pass configuration needed by the GPU canvas renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuCanvasConfig {
    /// MSAA sample count of the host render pass (must match
    /// `NativeOptions.multisampling`; 0/1 = no multisampling).
    pub msaa_samples: u32,
    /// Color format of the host render pass, e.g. the surface format
    /// (`Bgra8Unorm` in the app, `Rgba8Unorm` in `egui_kittest`). Never
    /// assume one: shader output follows the epaint gamma-space convention,
    /// and both Unorm formats receive the same sRGB-encoded bytes.
    pub target_format: wgpu::TextureFormat,
    /// Text drawn centered on each image whose texture is still loading; an
    /// empty string disables it. The host sets a localized label such as
    /// `"加载中"`.
    pub loading_label: &'static str,
    /// Image conversion strategy. Production uses [`ImageStrategy::Auto`];
    /// exact fidelity tests use [`ImageStrategy::Cpu`].
    pub image_strategy: ImageStrategy,
    /// Longest source-image side in pixels before downsampling.
    pub image_max_side: u32,
}

/// The GPU canvas renderer: pipelines and caches bound to one
/// [`RenderState`].
pub struct GpuCanvas {
    resources: Arc<GpuResources>,
    pages: Vec<(u64, Arc<Page>)>,
    loading_label: &'static str,
    image_strategy: ImageStrategy,
    image_max_side: u32,
}

impl GpuCanvas {
    /// Creates the renderer for the host render pass described by `config`, or
    /// `None` when the adapter cannot host it (see [`is_supported`]).
    ///
    /// No pipeline is created when the returned value is `None`, so an
    /// unsupported device falls back to raster instead of tripping wgpu
    /// validation.
    pub fn new(render_state: &RenderState, config: GpuCanvasConfig) -> Option<Self> {
        if !capability::is_supported(render_state, &config) {
            return None;
        }
        let resources = Arc::new(GpuResources::new(
            &render_state.device,
            &render_state.queue,
            config,
        ));
        Some(Self {
            resources,
            pages: Vec::new(),
            loading_label: config.loading_label,
            image_strategy: config.image_strategy,
            image_max_side: config.image_max_side.max(1),
        })
    }

    /// Whether any first-use image upload is still in flight.
    pub fn has_pending_images(&self) -> bool {
        self.resources.has_pending_images()
    }

    /// Returns a snapshot of the image conversion/upload counters.
    pub fn image_pipeline_stats(&self) -> ImagePipelineStats {
        self.resources.image_pipeline_stats()
    }

    /// Marks the current document generation. Already-queued image conversions
    /// from an older generation are dropped before they start.
    pub fn set_document_generation(&self, generation: u64) {
        self.resources.set_generation(generation);
    }

    /// Updates the longest source-image side and invalidates page recordings.
    pub fn set_image_max_side(&mut self, image_max_side: u32) {
        let image_max_side = image_max_side.max(1);
        if self.image_max_side != image_max_side {
            self.image_max_side = image_max_side;
            self.pages.clear();
        }
    }

    /// Records `bvh` as one egui-wgpu callback appended to `ui`'s painter.
    ///
    /// * `bvh`: the host-cached page BVH.
    /// * `page_pos`: top-left corner of the page in egui screen points.
    /// * `scale`: screen logical points per typst point (the preview zoom).
    ///
    /// Returns [`RenderError::NonFiniteScale`] for a non-finite or non-positive
    /// scale, [`RenderError::NonFinitePosition`] for a non-finite `page_pos`,
    /// and [`RenderError::UnsupportedCanvasContent`] when the page contains
    /// content this renderer cannot draw; nothing is appended in those cases and
    /// the host falls back to its raster path for the page.
    ///
    /// The recording is cached on the BVH's process-unique id, the scale bucket,
    /// and the pixels-per-point — not the page position or clip — so the same
    /// recording replays across pans and in-bucket zooms with the transform
    /// applied on the GPU. A cache miss records the whole page at the bucket
    /// center, so the replay ratio stays within 2^(±1/8). Rebuilding the BVH
    /// every frame (a new id) always misses; build it once and keep it.
    pub fn paint_page(
        &mut self,
        ui: &Ui,
        bvh: &FrameBvh,
        page_pos: egui::Pos2,
        scale: f32,
    ) -> Result<(), RenderError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(RenderError::NonFiniteScale);
        }
        if !page_pos.x.is_finite() || !page_pos.y.is_finite() {
            return Err(RenderError::NonFinitePosition);
        }
        self.resources.drain_image_results();
        let ppp = ui.ctx().pixels_per_point();
        let bucket = scale_bucket(scale);
        let key = page_key(bvh.id(), bucket, ppp);

        if let Some(index) = self.pages.iter().position(|(cached, _)| *cached == key) {
            let (cached, page) = self.pages.remove(index);
            let page = page.clone();
            self.pages.insert(0, (cached, page.clone()));
            self.append_page(ui, &page, page_pos, scale);
            return Ok(());
        }

        let recorded = record_page_with_strategy(
            bvh,
            page_pos,
            bucket,
            ui.ctx(),
            RecordPageOptions {
                ppp,
                max_texture_side: self.resources.max_texture_side,
                image_max_side: self.image_max_side,
                image_strategy: self.image_strategy,
            },
        )?;
        let page = recorded.arc();
        if !page.is_empty() {
            self.append_page(ui, &page, page_pos, scale);
            self.pages.insert(0, (key, page));
            self.pages.truncate(MAX_CACHED_PAGES);
        }
        Ok(())
    }

    /// Replays an off-UI [`PageRecording`] through this canvas's GPU resources.
    ///
    /// Returns whether the recording contained any commands. The caller may use
    /// an older generation's recording while the current one is being recorded.
    pub fn append_recording(
        &self,
        ui: &Ui,
        recording: &PageRecording,
        page_pos: egui::Pos2,
        scale: f32,
    ) -> bool {
        if recording.page.is_empty() {
            return false;
        }
        self.append_page(ui, &recording.page, page_pos, scale);
        true
    }

    fn append_page(&self, ui: &Ui, page: &Arc<Page>, page_pos: egui::Pos2, scale: f32) {
        if page.is_empty() {
            return;
        }
        let transform = page_transform(page.record_pos, page.record_scale, page_pos, scale);
        append_callback(ui, self.resources.clone(), Arc::clone(page), transform);
        self.draw_loading_labels(ui, page, transform);
    }

    /// Draws `loading_label` centered on each unclipped image quad whose texture
    /// is still loading, on top of the appended callback.
    ///
    /// Labels under an active `PushScissor`/`PushClip` are skipped rather than
    /// intersected with the clip, so a label never escapes the page's clipping.
    fn draw_loading_labels(&self, ui: &Ui, page: &Page, transform: [f32; 4]) {
        if self.loading_label.is_empty() {
            return;
        }
        let ratio = transform[0];
        let offset = egui::vec2(transform[1], transform[2]);
        let mut clip_depth: u32 = 0;
        for command in &page.commands {
            match command {
                Command::PushScissor { .. } | Command::PushClip { .. } => {
                    clip_depth += 1;
                }
                Command::PopScissor | Command::PopClip => {
                    clip_depth = clip_depth.saturating_sub(1);
                }
                Command::Image { vertices, key, .. } => {
                    if clip_depth > 0 || !self.resources.is_pending(key) {
                        continue;
                    }
                    let rect = draw::transform_rect(quad_rect(vertices), ratio, offset);
                    if rect.width() <= 40.0 || rect.height() <= 18.0 {
                        continue;
                    }
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        self.loading_label,
                        egui::FontId::proportional(12.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                _ => {}
            }
        }
    }
}

/// The axis-aligned screen-space rect spanned by `vertices`.
fn quad_rect(vertices: &[resources::CanvasVertex]) -> egui::Rect {
    let mut rect = egui::Rect::NOTHING;
    for vertex in vertices {
        rect.extend_with(egui::pos2(vertex.pos[0], vertex.pos[1]));
    }
    rect
}

/// Number of recorded pages kept for replay.
///
/// Must be at least the maximum number of pages the host paints in one frame:
/// with more visible pages than this the MRU cache would evict a page that is
/// re-painted the next frame, forcing a full re-record every frame. Sixteen
/// covers realistic visible-page counts at the minimum vector zoom.
const MAX_CACHED_PAGES: usize = 16;

/// A cache key covering everything a view-independent recording depends on.
fn page_key(bvh_id: u64, scale_bucket: f32, ppp: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bvh_id.hash(&mut hasher);
    scale_bucket.to_bits().hash(&mut hasher);
    ppp.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// The transform `[ratio, offset_x, offset_y, pad]` that maps a recording made
/// at `(record_pos, record_scale)` onto `(page_pos, scale)`.
///
/// A recorded vertex `v = q * record_scale + record_pos` (for page-space `q`)
/// renders as `v * ratio + offset = q * scale + page_pos`.
pub(crate) fn page_transform(
    record_pos: egui::Pos2,
    record_scale: f32,
    page_pos: egui::Pos2,
    scale: f32,
) -> [f32; 4] {
    let ratio = scale / record_scale;
    let offset = page_pos - record_pos * ratio;
    [ratio, offset.x, offset.y, 0.0]
}

/// Appends the page's callback to the UI's painter; the transformed recording
/// bounds set the callback viewport `egui-wgpu` clamps against.
fn append_callback(ui: &Ui, resources: Arc<GpuResources>, page: Arc<Page>, transform: [f32; 4]) {
    let bounds = draw::transform_rect(
        page.bounds,
        transform[0],
        egui::vec2(transform[1], transform[2]),
    );
    let callback = PageCallback::new(
        resources,
        page,
        transform,
        ui.ctx().clone(),
        ui.visuals().widgets.inactive.bg_fill,
        ui.clip_rect(),
    );
    ui.painter()
        .add(egui_wgpu::Callback::new_paint_callback(bounds, callback));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_api_signatures() {
        let _new: fn(&RenderState, GpuCanvasConfig) -> Option<GpuCanvas> = GpuCanvas::new;
        let _paint: fn(&mut GpuCanvas, &Ui, &FrameBvh, egui::Pos2, f32) -> Result<(), RenderError> =
            GpuCanvas::paint_page;
    }

    #[test]
    fn page_transform_is_identity_for_the_recording_view() {
        let transform = page_transform(egui::pos2(5.0, 7.0), 2.0, egui::pos2(5.0, 7.0), 2.0);
        assert_eq!(transform, [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn page_transform_maps_recorded_points_to_live_points() {
        let record_pos = egui::pos2(3.0, -4.0);
        let record_scale = 1.5_f32;
        let page_pos = egui::pos2(10.0, 20.0);
        let scale = 2.0_f32;
        let transform = page_transform(record_pos, record_scale, page_pos, scale);

        let page_point = (7.0_f32, 11.0_f32);
        let recorded = (
            page_point.0 * record_scale + record_pos.x,
            page_point.1 * record_scale + record_pos.y,
        );
        let rendered = (
            recorded.0 * transform[0] + transform[1],
            recorded.1 * transform[0] + transform[2],
        );
        let expected = (
            page_point.0 * scale + page_pos.x,
            page_point.1 * scale + page_pos.y,
        );
        assert!((rendered.0 - expected.0).abs() < 1e-3);
        assert!((rendered.1 - expected.1).abs() < 1e-3);
    }
}
