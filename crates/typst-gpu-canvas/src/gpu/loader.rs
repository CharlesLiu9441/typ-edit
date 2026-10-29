//! Asynchronous first-use image texture conversion and upload.
//!
//! Converting a raster image to a device-resolution texture and uploading it is
//! expensive enough that doing it on the render thread stalls large pages; an
//! SVG that is not vectorizable (see [`super::svg::is_vectorizable`]) is instead
//! rasterized here with resvg. [`convert_and_upload`] is the shared
//! implementation (its filter rules are pinned by the fidelity tests); on native
//! targets [`spawn_worker`] runs it on one dedicated thread, while on wasm32 (no
//! threads) the caller runs it inline. [`Job`] requests and [`ImageOutcome`]
//! results are the only values crossing the channel.
//!
//! The worker owns clones of the [`wgpu::Device`], [`wgpu::Queue`], image bind
//! group layout, and NEAREST image sampler, and exits when the job sender
//! disconnects. Each conversion is wrapped in `catch_unwind`: a panicking
//! conversion is reported as [`ImageOutcome::Failed`] rather than killing the
//! worker, so the key is not left pending forever.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use egui::Color32;
use egui_wgpu::wgpu;
use image::imageops::FilterType;
use typst_library::foundations::Smart;
use typst_library::visualize::{Image, ImageKind, ImageScaling, RasterImage, SvgImage};

use super::premultiply::PremultiplyPipeline;
use super::resources::{ImageKey, ImageMode, ImagePriority};

/// One conversion request: the cache key and the source image (a cheap `Arc`
/// clone).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct Job {
    /// Texture cache key (image plus target size).
    pub(crate) key: ImageKey,
    /// Source image to convert.
    pub(crate) image: Image,
    /// Document generation that requested this conversion.
    pub(crate) generation: u64,
    /// Visible work runs before background work.
    pub(crate) priority: ImagePriority,
    /// Monotonic enqueue order, used to break equal-priority ties.
    pub(crate) sequence: u64,
}

/// Priority queue shared by the image worker and the UI thread.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct JobQueue {
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
    closed: AtomicBool,
    generation: AtomicU64,
}

#[cfg(not(target_arch = "wasm32"))]
impl JobQueue {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn push(&self, job: Job) -> bool {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        jobs.push(job);
        drop(jobs);
        self.ready.notify_one();
        true
    }

    pub(crate) fn pop(&self) -> Option<Job> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some((index, _)) = jobs
                .iter()
                .enumerate()
                .max_by_key(|(_, job)| (job.priority, job.sequence))
            {
                return Some(jobs.remove(index));
            }
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            jobs = self
                .ready
                .wait(jobs)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_all();
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// The result of one conversion attempt.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) enum ImageOutcome {
    /// The texture was converted and uploaded.
    Done(ImageUpload),
    /// The conversion panicked; `key` stays on a stable placeholder.
    Failed(ImageKey),
    /// The requesting generation is stale; the key is no longer pending.
    Cancelled(ImageKey),
}

pub(crate) struct UploadResources {
    pub(crate) image_layout: wgpu::BindGroupLayout,
    pub(crate) image_sampler: wgpu::Sampler,
    pub(crate) image_sampler_linear: wgpu::Sampler,
    pub(crate) mipmap_pipeline: Arc<wgpu::RenderPipeline>,
    pub(crate) premultiply_pipeline: Arc<PremultiplyPipeline>,
}

/// One completed conversion: a texture and bind group ready for the image cache.
pub(crate) struct ImageUpload {
    /// Texture cache key.
    pub(crate) key: ImageKey,
    /// Uploaded texture.
    pub(crate) texture: wgpu::Texture,
    /// Bind group over `texture` and the shared NEAREST sampler.
    pub(crate) bind_group: wgpu::BindGroup,
    /// Approximate GPU bytes held by the texture.
    pub(crate) bytes: usize,
    /// Wall-clock time spent converting and uploading this image.
    pub(crate) elapsed_ns: u64,
}

/// Converts `image` to `key`'s texture size and uploads it.
///
/// Rasters resize with the filters that mirror `typst-render` (`Pixelated` →
/// nearest, upscale → Catmull-Rom, otherwise Lanczos3, no resize when the
/// source already matches); a non-vectorizable SVG is rasterized with resvg at
/// the same device size. Shared by the synchronous small path, the worker
/// thread, and the wasm inline path so all three produce identical pixels.
pub(crate) fn convert_and_upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: &UploadResources,
    key: ImageKey,
    image: &Image,
) -> ImageUpload {
    let started = std::time::Instant::now();
    let (width, height) = (key.width, key.height);
    let mip_levels = match key.mode {
        ImageMode::CpuTarget => 1,
        ImageMode::GpuSource => mip_levels(width, height),
    };
    let gpu_premultiply =
        key.mode == ImageMode::GpuSource && matches!(image.kind(), ImageKind::Raster(_));
    let bytes = match image.kind() {
        ImageKind::Raster(raster) => {
            raster_rgba8(raster, image.scaling(), width, height, !gpu_premultiply)
        }
        ImageKind::Svg(svg) => svg_rgba8(svg, width, height),
        ImageKind::Pdf(_) => {
            // PDF images are rejected before any command is recorded.
            unreachable!("only raster and svg images are recorded as `Command::Image`")
        }
    };
    let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    if mip_levels > 1 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("typst-gpu-canvas image"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mip_levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    });
    if gpu_premultiply {
        let (padded, bytes_per_row) = padded_rgba8(&bytes, width, height);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("typst-gpu-canvas premultiply source"),
            size: padded.len() as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, &padded);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("typst-gpu-canvas premultiply encoder"),
        });
        super::premultiply::encode(
            device,
            &mut encoder,
            &buffer,
            (padded.len() / 4) as u32,
            &upload.premultiply_pipeline,
        );
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
    } else {
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
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
    if mip_levels > 1 {
        super::mipmap::generate(
            device,
            queue,
            &texture,
            mip_levels,
            &upload.mipmap_pipeline,
            &upload.image_sampler_linear,
            &upload.image_layout,
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = match key.mode {
        ImageMode::CpuTarget => &upload.image_sampler,
        ImageMode::GpuSource => &upload.image_sampler_linear,
    };
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("typst-gpu-canvas image bind group"),
        layout: &upload.image_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    ImageUpload {
        key,
        texture,
        bind_group,
        bytes: mip_bytes(width, height, mip_levels),
        elapsed_ns: started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
    }
}

fn padded_rgba8(bytes: &[u8], width: u32, height: u32) -> (Vec<u8>, u32) {
    let row_bytes = 4 * width;
    let aligned_row = row_bytes.div_ceil(256) * 256;
    let mut padded = vec![0_u8; aligned_row as usize * height as usize];
    for row in 0..height as usize {
        let src = row * row_bytes as usize;
        let dst = row * aligned_row as usize;
        padded[dst..dst + row_bytes as usize]
            .copy_from_slice(&bytes[src..src + row_bytes as usize]);
    }
    (padded, aligned_row)
}

fn mip_levels(width: u32, height: u32) -> u32 {
    32 - width.max(height).leading_zeros()
}

fn mip_bytes(width: u32, height: u32, mip_levels: u32) -> usize {
    (0..mip_levels)
        .map(|level| {
            let w = (width >> level).max(1);
            let h = (height >> level).max(1);
            w as usize * h as usize * 4
        })
        .sum()
}

/// Converts a typst raster to premultiplied RGBA8 bytes at the target size.
fn raster_rgba8(
    raster: &RasterImage,
    scaling: Smart<ImageScaling>,
    width: u32,
    height: u32,
    premultiply: bool,
) -> Vec<u8> {
    let dynamic = raster.dynamic();
    let resized;
    let source = if (width, height) == (dynamic.width(), dynamic.height()) {
        dynamic.as_ref()
    } else {
        let upscale = width > dynamic.width();
        let filter = if matches!(scaling, Smart::Custom(ImageScaling::Pixelated)) {
            FilterType::Nearest
        } else if upscale {
            FilterType::CatmullRom
        } else {
            FilterType::Lanczos3
        };
        resized = dynamic.resize_exact(width, height, filter);
        &resized
    };
    let mut bytes = match source.as_rgba8() {
        Some(rgba) => rgba.as_raw().to_vec(),
        None => source.to_rgba8().into_raw(),
    };
    if premultiply {
        premultiply_rgba8(&mut bytes);
    }
    bytes
}

/// Premultiplies RGBA8 bytes in place with the same rounding as
/// `ColorImage::from_rgba_unmultiplied`.
fn premultiply_rgba8(bytes: &mut [u8]) {
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let color = Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]);
        *pixel = color.to_array();
    }
}

/// Rasterizes a typst SVG at the device texture size with resvg.
///
/// Mirrors `typst-render`'s `render_image`: the tree is scaled by
/// `(target / tree_size)` and rendered over the whole pixmap. `Pixmap::data`
/// is already premultiplied RGBA, matching the raster path's byte convention.
fn svg_rgba8(svg: &SvgImage, width: u32, height: u32) -> Vec<u8> {
    let tree = svg.tree();
    let tree_size = tree.size();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).expect("svg pixmap allocation failed");
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / tree_size.width(),
        height as f32 / tree_size.height(),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    pixmap.data().to_vec()
}

/// Spawns the single image-loader worker thread as a detached thread.
///
/// The worker survives a panicking conversion (reported as
/// [`ImageOutcome::Failed`]) and keeps serving later jobs.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_worker(
    device: wgpu::Device,
    queue: wgpu::Queue,
    upload: Arc<UploadResources>,
    job_queue: Arc<JobQueue>,
    result_tx: std::sync::mpsc::Sender<ImageOutcome>,
) {
    std::thread::Builder::new()
        .name("typst-gpu-canvas-images".into())
        .spawn(move || {
            while let Some(job) = job_queue.pop() {
                let key = job.key;
                if job.generation != job_queue.generation() {
                    let _ = result_tx.send(ImageOutcome::Cancelled(key));
                    continue;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    convert_and_upload(&device, &queue, &upload, key.clone(), &job.image)
                }));
                let outcome = match result {
                    Ok(upload) => ImageOutcome::Done(upload),
                    Err(_) => ImageOutcome::Failed(key),
                };
                if result_tx.send(outcome).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn typst-gpu-canvas image loader thread");
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use image::ImageEncoder;
    use typst_library::foundations::Bytes;
    use typst_library::visualize::{ExchangeFormat, RasterImage};

    use super::*;

    fn job(priority: ImagePriority, sequence: u64) -> Job {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[1, 2, 3, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode test image");
        let raster =
            RasterImage::plain(Bytes::new(png), ExchangeFormat::Png).expect("decode test image");
        let image = Image::plain(ImageKind::Raster(raster));
        let key = ImageKey::new(image.clone(), 1, 1, ImageMode::CpuTarget);
        Job {
            key,
            image,
            generation: 0,
            priority,
            sequence,
        }
    }

    #[test]
    fn queue_prefers_visible_then_newest() {
        let queue = JobQueue::new();
        assert!(queue.push(job(ImagePriority::Background, 1)));
        assert!(queue.push(job(ImagePriority::Visible, 2)));
        assert!(queue.push(job(ImagePriority::Visible, 3)));
        assert_eq!(queue.pop().unwrap().sequence, 3);
        assert_eq!(queue.pop().unwrap().sequence, 2);
        assert_eq!(queue.pop().unwrap().sequence, 1);
        queue.close();
        assert!(queue.pop().is_none());
    }

    #[test]
    fn queue_tracks_generation() {
        let queue = JobQueue::new();
        assert_eq!(queue.generation(), 0);
        queue.set_generation(9);
        assert_eq!(queue.generation(), 9);
        queue.close();
    }
}
