//! Shared wgpu resources for the GPU canvas renderer.
//!
//! Pipelines and bind groups are created once per
//! [`GpuCanvas`](super::GpuCanvas) for the host render pass's MSAA sample count
//! and color format (both are configuration, never hardcoded), while per-frame
//! vertex/index buffers are uploaded by the callback's `prepare` step. Raster
//! image textures are converted, uploaded, and LRU-evicted here on first use;
//! all but the smallest are converted and uploaded asynchronously on a worker
//! thread and swapped in once ready (see [`super::loader`]).
//!
//! Pipelines declare a `Stencil8` depth-stencil state because wgpu requires a
//! pipeline's depth-stencil format to match the render pass exactly, and the
//! host pass is required to carry that attachment. Curve-clip masks reuse the
//! same state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use typst_library::visualize::{Image, ImageKind};

use super::callback::Prepared;
use super::gradient::{GradientKey, GradientPlan, LUT_SIZE};
use super::loader;
use super::{GpuCanvasConfig, ImagePipelineStats};

/// Stencil attachment format expected from the host render pass.
pub(crate) const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// Number of prepared page buffer sets kept across frames.
///
/// Must be at least the maximum number of pages the host paints in one frame
/// (matching `MAX_CACHED_PAGES` in `gpu/mod.rs`): with a smaller cap the
/// prepared cache clears mid-frame and re-uploads vertex/index buffers for the
/// remaining pages on every frame.
const MAX_PREPARED_PAGES: usize = 16;

/// Budget for uploaded image textures, in bytes; least-recently-used entries
/// are evicted once the cache exceeds it.
///
/// The budget approximates live textures: a texture can survive an eviction
/// only while a frame's callback still references it, and dropping the last
/// cached `Prepared` that pins it releases the GPU allocation. Callers evicting
/// from this cache must therefore also purge prepared pages that reference the
/// evicted keys (see `purge_prepared_images`). One residual pin remains: an
/// image captured earlier during the same `Prepared` construction is not
/// visible to that purge until the entry turns over, so the budget is a soft
/// target.
const IMAGE_TEXTURE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Which conversion path produced one image texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ImageMode {
    /// A CPU-resized texture sampled with nearest filtering.
    CpuTarget,
    /// A source-resolution texture sampled with linear filtering and mipmaps.
    GpuSource,
}

/// Scheduler priority for an image conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ImagePriority {
    /// Not currently visible; load only if the page is already being prepared.
    Background,
    /// Intersects the current viewport; load before background work.
    Visible,
}

/// Cache key for one uploaded image texture.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImageKey {
    /// The immutable typst image (including its scaling policy).
    pub(crate) image: Image,
    /// Uploaded texture width in pixels.
    pub(crate) width: u32,
    /// Uploaded texture height in pixels.
    pub(crate) height: u32,
    /// Conversion and sampling path.
    pub(crate) mode: ImageMode,
}

impl ImageKey {
    /// Creates a cache key for an uploaded image texture.
    pub(crate) fn new(image: Image, width: u32, height: u32, mode: ImageMode) -> Self {
        Self {
            image,
            width,
            height,
            mode,
        }
    }
}

/// Images whose source or target exceeds this many pixels convert on the loader
/// thread instead of inline. Keeping small images synchronous avoids a
/// one-frame placeholder flash and keeps the small fidelity fixtures
/// deterministic.
const SYNC_IMAGE_MAX_PIXELS: usize = 512 * 512;

/// Whether an image's texture is ready to draw or still being converted.
pub(crate) enum ImageState {
    /// The texture is uploaded; the bind group can be drawn immediately.
    Ready(wgpu::BindGroup),
    /// The texture is loading; draw a placeholder until it is ready.
    Pending,
}

/// Vertex layout for every canvas pipeline.
///
/// * `pos`: position in logical screen points (the screen uniform scales it by
///   `pixels_per_point`).
/// * `local`: paint-space coordinates, interpreted per pipeline (gradient
///   parameter space in M1, image UV in M1); unused by the solid pipeline.
/// * `color`: premultiplied sRGB vertex color.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CanvasVertex {
    /// Position in logical screen points.
    pub pos: [f32; 2],
    /// Paint-space coordinates for gradient/image pipelines.
    pub local: [f32; 2],
    /// Premultiplied sRGB color.
    pub color: [u8; 4],
}

impl CanvasVertex {
    /// Vertex buffer layout matching [`CanvasVertex`].
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Unorm8x4,
        ],
    };
}

/// One paint kind's pipelines for the three stencil contexts.
pub(crate) struct ContentPipelines {
    /// Outside any clip mask.
    pub(crate) plain: wgpu::RenderPipeline,
    /// Inside the outermost mask: stencil test `NotEqual 0`.
    pub(crate) outer: wgpu::RenderPipeline,
    /// Inside nested masks: stencil test `Equal level`.
    pub(crate) nested: wgpu::RenderPipeline,
}

/// The pipeline set, created for one MSAA sample count and target format.
pub(crate) struct Pipelines {
    /// Solid-color fills.
    pub(crate) solid: ContentPipelines,
    /// Gradient fills sampling a baked LUT.
    pub(crate) gradient: ContentPipelines,
    /// Image-textured quads.
    pub(crate) image: ContentPipelines,
    /// Outermost mask push: `Always`, front increment / back decrement.
    pub(crate) mask_push_outer: wgpu::RenderPipeline,
    /// Nested mask push: `Equal level`, front increment / back decrement.
    pub(crate) mask_push_nested: wgpu::RenderPipeline,
    /// Outermost mask pop: `NotEqual 0`, front decrement / back increment.
    pub(crate) mask_pop_outer: wgpu::RenderPipeline,
    /// Nested mask pop: `Equal level`, front decrement / back increment.
    pub(crate) mask_pop_nested: wgpu::RenderPipeline,
}

/// One cached gradient: its LUT texture, parameter uniform, and bind group.
struct GradientGpu {
    _texture: wgpu::Texture,
    _uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// One cached image: its texture and bind group.
struct ImageGpu {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// Approximate GPU bytes held by the texture.
    bytes: usize,
    /// Monotonic use tick for LRU eviction.
    last_used: u64,
}

/// Least-recently-used cache of uploaded image textures.
#[derive(Default)]
struct ImageCache {
    entries: HashMap<ImageKey, ImageGpu>,
    lru: BTreeMap<u64, ImageKey>,
    tick: u64,
    bytes: usize,
}

/// Cheap atomic counters for the image conversion path.
#[derive(Default)]
pub(crate) struct ImageStats {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    sync_conversions: AtomicU64,
    async_jobs: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    conversion_ns: AtomicU64,
    prepared_rebuilds: AtomicU64,
    evictions: AtomicU64,
    upload_bytes: AtomicU64,
}

impl ImageStats {
    fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }
}

impl ImageCache {
    /// Returns the cached bind group for `key`, marking it most-recently-used.
    fn get(&mut self, key: &ImageKey) -> Option<wgpu::BindGroup> {
        self.tick += 1;
        let tick = self.tick;
        let cache_key = key.clone();
        let (old_tick, bind_group) = {
            let entry = self.entries.get_mut(key)?;
            let old_tick = entry.last_used;
            entry.last_used = tick;
            (old_tick, entry.bind_group.clone())
        };
        self.lru.remove(&old_tick);
        self.lru.insert(tick, cache_key);
        Some(bind_group)
    }

    /// Returns `(entries, bytes)` for diagnostics.
    fn usage(&self) -> (u64, u64) {
        (self.entries.len() as u64, self.bytes as u64)
    }

    /// Inserts `gpu`, then evicts least-recently-used entries until the cache
    /// is within budget, returning the evicted keys. The just-inserted entry is
    /// never evicted, so a single oversized image may exceed the budget
    /// temporarily. Replacing an existing key subtracts the old entry's bytes.
    fn insert(&mut self, key: ImageKey, mut gpu: ImageGpu) -> Vec<ImageKey> {
        self.tick += 1;
        gpu.last_used = self.tick;
        let new_bytes = gpu.bytes;
        let cache_key = key.clone();
        if let Some(old) = self.entries.insert(key, gpu) {
            self.lru.remove(&old.last_used);
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.lru.insert(self.tick, cache_key);
        self.bytes += new_bytes;

        let mut evicted = Vec::new();
        while self.bytes > IMAGE_TEXTURE_BUDGET_BYTES && self.entries.len() > 1 {
            let Some((_, key)) = self.lru.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
                evicted.push(key);
            }
        }
        evicted
    }
}

/// Resources shared by every page callback of one [`GpuCanvas`](super::GpuCanvas).
pub(crate) struct GpuResources {
    screen_buffer: wgpu::Buffer,
    /// Layout for group 0: the screen uniform plus the per-callback transform.
    screen_layout: wgpu::BindGroupLayout,
    /// Pipelines for the configured sample count and target format.
    pub(crate) pipelines: Pipelines,
    gradient_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    upload: Arc<loader::UploadResources>,
    /// Device cap on a raster texture side, in pixels.
    pub(crate) max_texture_side: u32,
    gradients: Mutex<HashMap<GradientKey, GradientGpu>>,
    images: Mutex<ImageCache>,
    prepared: Mutex<HashMap<u64, Arc<Prepared>>>,
    /// Priority queue shared with the loader thread (native only; wasm
    /// converts inline).
    #[cfg(not(target_arch = "wasm32"))]
    job_queue: Arc<loader::JobQueue>,
    /// Monotonic job sequence for equal-priority ordering.
    job_sequence: AtomicU64,
    /// Conversion outcomes sent back by the loader (or pushed inline on wasm).
    result_tx: Mutex<mpsc::Sender<loader::ImageOutcome>>,
    /// Conversion outcomes awaiting processing.
    result_rx: Mutex<mpsc::Receiver<loader::ImageOutcome>>,
    /// Image keys with a conversion in flight, for dedupe and pending checks.
    in_flight: Mutex<HashSet<ImageKey>>,
    /// Image keys whose conversion panicked. They are never retried and stay on
    /// a stable placeholder, so they are reported as not pending.
    failed: Mutex<HashSet<ImageKey>>,
    /// Diagnostic counters for image conversion and cache behavior.
    stats: ImageStats,
}

impl GpuResources {
    /// Creates the shared resources for `config`.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: GpuCanvasConfig) -> Self {
        let solid = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("typst-gpu-canvas solid shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/solid.wgsl").into()),
        });

        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("typst-gpu-canvas screen layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let screen_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("typst-gpu-canvas screen uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let gradient_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("typst-gpu-canvas gradient shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/gradient.wgsl").into()),
        });
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("typst-gpu-canvas image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
        });

        let gradient_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("typst-gpu-canvas gradient layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(80),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("typst-gpu-canvas image layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let solid_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("typst-gpu-canvas solid layout"),
            bind_group_layouts: &[Some(&screen_layout)],
            immediate_size: 0,
        });
        let gradient_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("typst-gpu-canvas gradient pipeline layout"),
                bind_group_layouts: &[Some(&screen_layout), Some(&gradient_layout)],
                immediate_size: 0,
            });
        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("typst-gpu-canvas image pipeline layout"),
                bind_group_layouts: &[Some(&screen_layout), Some(&image_layout)],
                immediate_size: 0,
            });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("typst-gpu-canvas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // The CPU image path uploads with `TextureOptions::NEAREST`; the resize
        // to the device resolution happens before upload, so nearest sampling
        // reproduces it exactly.
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("typst-gpu-canvas image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let image_sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("typst-gpu-canvas linear image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let mipmap_pipeline = Arc::new(super::mipmap::create_pipeline(device, &image_layout));
        let premultiply_pipeline = Arc::new(super::premultiply::create_pipeline(device));
        let upload = Arc::new(loader::UploadResources {
            image_layout: image_layout.clone(),
            image_sampler: image_sampler.clone(),
            image_sampler_linear: image_sampler_linear.clone(),
            mipmap_pipeline: Arc::clone(&mipmap_pipeline),
            premultiply_pipeline: Arc::clone(&premultiply_pipeline),
        });

        let (result_tx, result_rx) = mpsc::channel::<loader::ImageOutcome>();
        #[cfg(not(target_arch = "wasm32"))]
        let job_queue = {
            let job_queue = loader::JobQueue::new();
            loader::spawn_worker(
                device.clone(),
                queue.clone(),
                Arc::clone(&upload),
                Arc::clone(&job_queue),
                result_tx.clone(),
            );
            job_queue
        };

        let sample_count = config.msaa_samples.max(1);
        let solid = content_pipelines(
            device,
            &solid,
            &solid_layout,
            sample_count,
            config.target_format,
        );
        let gradient = content_pipelines(
            device,
            &gradient_shader,
            &gradient_pipeline_layout,
            sample_count,
            config.target_format,
        );
        let image = content_pipelines(
            device,
            &image_shader,
            &image_pipeline_layout,
            sample_count,
            config.target_format,
        );

        // Curve-clip masks use a nesting scheme: the outer mask
        // increments/decrements the NonZero winding with `Always`, nested
        // masks only count where the stencil already equals the previous level
        // with `Equal`.
        use wgpu::CompareFunction as Cmp;
        use wgpu::StencilOperation::{DecrementWrap as Dec, IncrementWrap as Inc};
        let mask_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("typst-gpu-canvas mask shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mask.wgsl").into()),
        });
        let mask_pipeline =
            |compare: Cmp, front_op: wgpu::StencilOperation, back_op: wgpu::StencilOperation| {
                mask_pipeline(
                    device,
                    &mask_shader,
                    &solid_layout,
                    sample_count,
                    config.target_format,
                    compare,
                    front_op,
                    back_op,
                )
            };
        let mask_push_outer = mask_pipeline(Cmp::Always, Inc, Dec);
        let mask_push_nested = mask_pipeline(Cmp::Equal, Inc, Dec);
        let mask_pop_outer = mask_pipeline(Cmp::NotEqual, Dec, Inc);
        let mask_pop_nested = mask_pipeline(Cmp::Equal, Dec, Inc);

        let max_texture_side =
            crate::image_helpers::MAX_TEXTURE_SIDE.min(device.limits().max_texture_dimension_2d);

        Self {
            screen_buffer,
            screen_layout,
            pipelines: Pipelines {
                solid,
                gradient,
                image,
                mask_push_outer,
                mask_push_nested,
                mask_pop_outer,
                mask_pop_nested,
            },
            gradient_layout,
            sampler,
            upload,
            max_texture_side,
            gradients: Mutex::new(HashMap::new()),
            images: Mutex::new(ImageCache::default()),
            prepared: Mutex::new(HashMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            job_queue,
            job_sequence: AtomicU64::default(),
            result_tx: Mutex::new(result_tx),
            result_rx: Mutex::new(result_rx),
            in_flight: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashSet::new()),
            stats: ImageStats::default(),
        }
    }

    /// The prepared buffers for a page recording, if cached.
    pub(crate) fn prepared_page(&self, page_id: u64) -> Option<Arc<Prepared>> {
        let cache = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(&page_id).cloned()
    }

    /// Caches the prepared buffers of a page recording.
    pub(crate) fn store_prepared(&self, page_id: u64, prepared: Arc<Prepared>) {
        let mut cache = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() >= MAX_PREPARED_PAGES {
            cache.clear();
        }
        cache.insert(page_id, prepared);
    }

    /// Returns the cached bind group for `plan`, creating and uploading its
    /// LUT texture and parameter uniform on first use.
    pub(crate) fn ensure_gradient(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GradientPlan,
    ) -> wgpu::BindGroup {
        {
            let cache = self
                .gradients
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = cache.get(&plan.key) {
                return entry.bind_group.clone();
            }
        }

        let width = LUT_SIZE;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("typst-gpu-canvas gradient lut"),
            size: wgpu::Extent3d {
                width,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = Vec::with_capacity(plan.lut.len() * 8);
        for color in &plan.lut {
            for channel in color {
                bytes.extend_from_slice(&f32_to_f16(*channel).to_le_bytes());
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8 * width),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("typst-gpu-canvas gradient uniform"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, &plan.params.to_bytes());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("typst-gpu-canvas gradient bind group"),
            layout: &self.gradient_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut cache = self
            .gradients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = cache.entry(plan.key.clone()).or_insert(GradientGpu {
            _texture: texture,
            _uniform: uniform,
            bind_group,
        });
        entry.bind_group.clone()
    }

    /// Creates a per-callback transform uniform buffer and a bind group over
    /// the shared screen uniform (group 0: screen, transform).
    pub(crate) fn create_transform_bind_group(
        &self,
        device: &wgpu::Device,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("typst-gpu-canvas transform uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("typst-gpu-canvas page bind group"),
            layout: &self.screen_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.screen_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        (buffer, bind_group)
    }

    /// Resolves the bind group for `key`, or reports that its texture is still
    /// loading.
    ///
    /// Small images convert inline; larger ones are enqueued on the loader
    /// thread and reported [`ImageState::Pending`] until
    /// [`Self::drain_image_results`] swaps the finished texture in. A key whose
    /// conversion panicked is reported [`ImageState::Pending`] permanently
    /// without retrying — it keeps a stable placeholder and is not counted as
    /// pending, so it draws no label and requests no repaints.
    pub(crate) fn image_state(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: &ImageKey,
        image: &Image,
        priority: ImagePriority,
        allow_load: bool,
    ) -> ImageState {
        {
            let mut cache = self
                .images
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(bind_group) = cache.get(key) {
                ImageStats::add(&self.stats.cache_hits, 1);
                return ImageState::Ready(bind_group);
            }
        }
        ImageStats::add(&self.stats.cache_misses, 1);
        if self
            .failed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
        {
            return ImageState::Pending;
        }
        if self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
        {
            return ImageState::Pending;
        }
        if !allow_load {
            return ImageState::Pending;
        }

        let source_pixels = match image.kind() {
            ImageKind::Raster(raster) => {
                let dynamic = raster.dynamic();
                dynamic.width() as usize * dynamic.height() as usize
            }
            ImageKind::Svg(svg) => {
                let size = svg.tree().size();
                (f64::from(size.width()) * f64::from(size.height())).round() as usize
            }
            ImageKind::Pdf(_) => {
                // `Command::Image` is only recorded for raster and SVG images.
                unreachable!("only raster and svg images are recorded as `Command::Image`")
            }
        };
        let target_pixels = key.width as usize * key.height as usize;
        if source_pixels.max(target_pixels) <= SYNC_IMAGE_MAX_PIXELS {
            let started = std::time::Instant::now();
            let upload =
                loader::convert_and_upload(device, queue, &self.upload, key.clone(), image);
            ImageStats::add(&self.stats.sync_conversions, 1);
            ImageStats::add(
                &self.stats.conversion_ns,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            );
            let (bind_group, invalidated) = self.insert_image(upload);
            self.purge_prepared_images(&invalidated);
            return ImageState::Ready(bind_group);
        }

        self.enqueue_image(device, queue, key, image, priority)
    }

    /// Records one full prepared-page rebuild for diagnostics.
    pub(crate) fn record_prepared_rebuild(&self) {
        ImageStats::add(&self.stats.prepared_rebuilds, 1);
    }

    /// Returns a snapshot of the image pipeline counters and current cache state.
    pub(crate) fn image_pipeline_stats(&self) -> ImagePipelineStats {
        let (cache_entries, cache_bytes) = self
            .images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .usage();
        let in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len() as u64;
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        ImagePipelineStats {
            cache_hits: load(&self.stats.cache_hits),
            cache_misses: load(&self.stats.cache_misses),
            sync_conversions: load(&self.stats.sync_conversions),
            async_jobs: load(&self.stats.async_jobs),
            completed: load(&self.stats.completed),
            failed: load(&self.stats.failed),
            conversion_ns: load(&self.stats.conversion_ns),
            prepared_rebuilds: load(&self.stats.prepared_rebuilds),
            evictions: load(&self.stats.evictions),
            upload_bytes: load(&self.stats.upload_bytes),
            in_flight,
            cache_entries,
            cache_bytes,
        }
    }

    /// Inserts a finished upload into the image LRU and returns the bind group
    /// plus the keys whose prepared pages must be invalidated.
    fn insert_image(&self, upload: loader::ImageUpload) -> (wgpu::BindGroup, Vec<ImageKey>) {
        let loader::ImageUpload {
            key,
            texture,
            bind_group,
            bytes,
            ..
        } = upload;
        let result = bind_group.clone();
        let mut purged = {
            let mut cache = self
                .images
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.insert(
                key.clone(),
                ImageGpu {
                    _texture: texture,
                    bind_group,
                    bytes,
                    last_used: 0,
                },
            )
        };
        ImageStats::add(&self.stats.evictions, purged.len() as u64);
        ImageStats::add(&self.stats.completed, 1);
        ImageStats::add(&self.stats.upload_bytes, bytes as u64);
        purged.push(key);
        (result, purged)
    }

    /// Claims `key` and enqueues one conversion on the loader thread (or
    /// converts inline on wasm32), deduplicating against [`Self::in_flight`].
    ///
    /// Re-checks the image cache after claiming so a concurrent completion
    /// between the caller's probe and the claim is reused instead of converted
    /// twice; that window is narrowed but not fully eliminated.
    fn enqueue_image(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: &ImageKey,
        image: &Image,
        priority: super::resources::ImagePriority,
    ) -> ImageState {
        {
            let mut in_flight = self
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !in_flight.insert(key.clone()) {
                return ImageState::Pending;
            }
        }

        ImageStats::add(&self.stats.async_jobs, 1);
        let cached = {
            let mut cache = self
                .images
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.get(key)
        };
        if let Some(bind_group) = cached {
            self.in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key);
            return ImageState::Ready(bind_group);
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let sequence = self.job_sequence.fetch_add(1, Ordering::Relaxed);
            let job = loader::Job {
                key: key.clone(),
                image: image.clone(),
                generation: self.job_queue.generation(),
                priority,
                sequence,
            };
            if !self.job_queue.push(job) {
                self.push_inline(device, queue, key, image);
            }
        }
        #[cfg(target_arch = "wasm32")]
        self.push_inline(device, queue, key, image);
        ImageState::Pending
    }

    /// Converts `image` inline and pushes the result for the next drain.
    ///
    /// The worker path wraps the conversion in `catch_unwind`; this path runs on
    /// the render thread (`wasm`, or a failed job-channel send), so it needs the
    /// same guard — a panicking conversion (an SVG pixmap allocation, say) must
    /// mark the image failed instead of taking the window down.
    fn push_inline(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: &ImageKey,
        image: &Image,
    ) {
        let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loader::convert_and_upload(device, queue, &self.upload, key.clone(), image)
        }));
        let outcome = match converted {
            Ok(upload) => loader::ImageOutcome::Done(upload),
            Err(_) => loader::ImageOutcome::Failed(key.clone()),
        };
        let _ = self
            .result_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(outcome);
    }

    /// Non-blocking drain of conversion outcomes: uploads go into the image
    /// cache and invalidate placeholders that referenced them; panics record
    /// the key as failed so it is never retried.
    pub(crate) fn drain_image_results(&self) {
        let mut invalidated = Vec::new();
        loop {
            let outcome = self
                .result_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_recv();
            match outcome {
                Ok(loader::ImageOutcome::Done(upload)) => {
                    self.in_flight
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&upload.key);
                    ImageStats::add(&self.stats.conversion_ns, upload.elapsed_ns);
                    let (_, keys) = self.insert_image(upload);
                    invalidated.extend(keys);
                }
                Ok(loader::ImageOutcome::Failed(key)) => {
                    self.in_flight
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&key);
                    ImageStats::add(&self.stats.failed, 1);
                    self.failed
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(key);
                }
                Ok(loader::ImageOutcome::Cancelled(key)) => {
                    self.in_flight
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&key);
                }
                Err(_) => break,
            }
        }
        if !invalidated.is_empty() {
            self.purge_prepared_images(&invalidated);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_generation(&self, generation: u64) {
        self.job_queue.set_generation(generation);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_generation(&self, _generation: u64) {}

    /// Whether any image conversion is in flight.
    pub(crate) fn has_pending_images(&self) -> bool {
        !self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    /// Whether `key`'s texture is still loading.
    pub(crate) fn is_pending(&self, key: &ImageKey) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
    }

    /// Whether `key`'s texture is present in the image cache.
    pub(crate) fn is_image_ready(&self, key: &ImageKey) -> bool {
        self.images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .contains_key(key)
    }

    /// Drops the cached prepared buffers for `page_id`, forcing a rebuild.
    pub(crate) fn drop_prepared(&self, page_id: u64) {
        self.prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&page_id);
    }

    /// Drops cached prepared pages that reference any `evicted` image texture,
    /// releasing the texture's bind group (and thus its GPU allocation).
    ///
    /// Must not be called with the images lock held: it takes the prepared lock.
    fn purge_prepared_images(&self, evicted: &[ImageKey]) {
        let mut cache = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, prepared| !prepared.image_keys.iter().any(|key| evicted.contains(key)));
    }

    /// Writes the screen size (physical pixels) and pixels-per-point uniform.
    pub(crate) fn update_screen(
        &self,
        queue: &wgpu::Queue,
        size_in_pixels: [u32; 2],
        pixels_per_point: f32,
    ) {
        let data = [
            size_in_pixels[0].max(1) as f32,
            size_in_pixels[1].max(1) as f32,
            pixels_per_point,
            0.0_f32,
        ];
        let mut bytes = [0_u8; 16];
        for (i, value) in data.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        queue.write_buffer(&self.screen_buffer, 0, &bytes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for GpuResources {
    fn drop(&mut self) {
        self.job_queue.close();
    }
}

/// Builds a content pipeline for each stencil context over the shared canvas
/// vertex layout.
fn content_pipelines(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    sample_count: u32,
    target_format: wgpu::TextureFormat,
) -> ContentPipelines {
    let build =
        |stencil| content_pipeline(device, module, layout, sample_count, target_format, stencil);
    ContentPipelines {
        plain: build(stencil_state(wgpu::CompareFunction::Always)),
        outer: build(stencil_state(wgpu::CompareFunction::NotEqual)),
        nested: build(stencil_state(wgpu::CompareFunction::Equal)),
    }
}

/// Builds one content pipeline with the given stencil comparison.
fn content_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    sample_count: u32,
    target_format: wgpu::TextureFormat,
    stencil: wgpu::DepthStencilState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("typst-gpu-canvas pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(CanvasVertex::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(premultiplied_blend()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(stencil),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

/// Builds one stencil-mask pipeline writing no color.
#[allow(clippy::too_many_arguments)]
fn mask_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    sample_count: u32,
    target_format: wgpu::TextureFormat,
    compare: wgpu::CompareFunction,
    front_op: wgpu::StencilOperation,
    back_op: wgpu::StencilOperation,
) -> wgpu::RenderPipeline {
    let face = |op| wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: op,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("typst-gpu-canvas mask pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(CanvasVertex::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_mask"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState::stencil(
            STENCIL_FORMAT,
            wgpu::StencilState {
                front: face(front_op),
                back: face(back_op),
                read_mask: 0xFF,
                write_mask: 0xFF,
            },
        )),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

/// Premultiplied-alpha blending, the epaint convention.
pub(crate) fn premultiplied_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

/// Content stencil state: test `compare`, keep the stencil untouched.
pub(crate) fn stencil_state(compare: wgpu::CompareFunction) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: wgpu::StencilOperation::Keep,
    };
    wgpu::DepthStencilState::stencil(
        STENCIL_FORMAT,
        wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xFF,
            write_mask: 0xFF,
        },
    )
}

/// Creates GPU vertex and index buffers for one mesh, or `None` when empty.
pub(crate) fn mesh_buffers(
    device: &wgpu::Device,
    vertices: &[CanvasVertex],
    indices: &[u32],
) -> Option<(wgpu::Buffer, wgpu::Buffer, u32)> {
    use egui_wgpu::wgpu::util::DeviceExt as _;

    if vertices.is_empty() || indices.is_empty() {
        return None;
    }
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("typst-gpu-canvas vertices"),
        contents: &vertex_bytes(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("typst-gpu-canvas indices"),
        contents: &index_bytes(indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    Some((vertex_buffer, index_buffer, indices.len() as u32))
}

fn vertex_bytes(vertices: &[CanvasVertex]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(vertices));
    for v in vertices {
        out.extend_from_slice(&v.pos[0].to_le_bytes());
        out.extend_from_slice(&v.pos[1].to_le_bytes());
        out.extend_from_slice(&v.local[0].to_le_bytes());
        out.extend_from_slice(&v.local[1].to_le_bytes());
        out.extend_from_slice(&v.color);
    }
    out
}

fn index_bytes(indices: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 4);
    for index in indices {
        out.extend_from_slice(&index.to_le_bytes());
    }
    out
}

/// Converts an f32 in `[0, 1]` (a premultiplied sRGB channel) to IEEE 754
/// binary16 bits, rounding to nearest even. The `Rgba16Float` LUT keeps the
/// sub-byte precision the reference renderer computes with.
fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;
    if exponent == 0 {
        return sign;
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1F {
        return sign | 0x7C00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = (mantissa | 0x0080_0000) >> (1 - half_exponent + 13);
        return sign | mantissa as u16;
    }
    let half = sign | ((half_exponent as u16) << 10) | (mantissa >> 13) as u16;
    let remainder = mantissa & 0x1FFF;
    if remainder > 0x1000 || (remainder == 0x1000 && (half & 1) == 1) {
        half + 1
    } else {
        half
    }
}
