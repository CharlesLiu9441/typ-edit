//! Self-contained GPU canvas renderer for typst frames on egui's wgpu pass.
//!
//! Extracted from a larger CPU-render crate so it can be consumed without the
//! mesh/SDF render path. [`GpuCanvas::paint_page`]
//! records a page's items in typst paint order and appends one `egui_wgpu`
//! callback that replays them with per-paint pipelines and MSAA
//! anti-aliasing. Recordings are view-independent: they are cached by BVH id,
//! scale bucket, and pixels-per-point, and pan/zoom are applied on the GPU, so
//! scrolling replays a cached recording instead of re-tessellating. The
//! transformed recording bounds set each callback's viewport, which `egui-wgpu`
//! clamps against.
//!
//! A page is recorded in full: viewport culling is not applied while recording,
//! so any unsupported item anywhere on a painted page makes [`GpuCanvas::paint_page`]
//! return [`RenderError::UnsupportedCanvasContent`] and the host falls back to
//! raster for the whole generation.
//!
//! Embedded SVGs are drawn as vector content when every feature maps onto the
//! canvas (fills, strokes, gradients, and stencil clips); otherwise the SVG is
//! rasterized per image with resvg, so an SVG never triggers the whole-document
//! fallback.
//!
//! Image textures are converted and uploaded on first use. Small images convert
//! inline; larger ones load on a background thread and are swapped in once
//! ready. While a texture is loading, a gray placeholder quad is drawn in place
//! of the image with [`GpuCanvasConfig::loading_label`] centered on it, and
//! repaints are requested until it lands (see [`GpuCanvas::has_pending_images`]).
//!
//! Geometry is tessellated in screen space by the internal lyon-based
//! tessellator with the feather disabled; MSAA (the host render pass's sample
//! count) provides coverage.
//!
//! The renderer owns its own `FrameBvh` support code ([`FrameBvh`] is
//! re-exported) so hosts can cache one tree per compiled document.
//!
//! # Requirements
//!
//! * The host render pass must be configured with the same MSAA sample count
//!   and color format as [`GpuCanvasConfig`], and must carry a `Stencil8`
//!   attachment (M3 curve clips and the depth-stencil format of every canvas
//!   pipeline require it).

mod affine;
mod bounds;
mod bvh;
mod clip;
mod error;
mod glyph_cache;
mod glyphs;
mod image_helpers;
mod paint;
mod tess;
mod view;

mod gpu;

pub use bvh::FrameBvh;
pub use error::RenderError;
pub use gpu::{
    GpuCanvas, GpuCanvasConfig, ImagePipelineStats, ImageStrategy, PageRecording, is_supported,
    record_page, record_page_with_image_max_side,
};
pub use view::scale_bucket;

/// A page source a recording worker can read without the UI owning a reference.
pub trait PageFrameSource: Send + Sync {
    /// Returns the page's frame, or `None` when `page` is out of range.
    fn frame(&self, page: usize) -> Option<typst_library::layout::Frame>;
}
