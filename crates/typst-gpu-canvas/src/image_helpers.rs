//! Image sizing helpers shared with the canvas image leaf.

use typst_library::foundations::Smart;
use typst_library::layout::Size;
use typst_library::visualize::{Image, ImageKind, ImageScaling};

use crate::gpu::{ImageKey, ImageMode, ImageStrategy};
use crate::paint::PaintCtx;
use crate::view::scale_bucket;

/// Hard cap on a raster texture side, in device pixels; the actual cap is
/// `min(this, device.limits().max_texture_dimension_2d)`.
pub(crate) const MAX_TEXTURE_SIDE: u32 = 8192;

/// Default longest source-texture side uploaded for the GPU-source path.
pub(crate) const DEFAULT_IMAGE_MAX_SIDE: u32 = 4096;

/// Images at or below this source size stay on the exact CPU target path.
const AUTO_CPU_MAX_SOURCE_PIXELS: u64 = 512 * 512;

const SVG_SOURCE_SCALE: f64 = 2.0;
const SVG_SOURCE_MIN_SIDE: u32 = 256;

/// Typst's raster texture size: preserve the source aspect ratio and size the
/// texture so its longer dimension reaches device resolution.
///
/// Mirrors `typst-render`'s `render_image`: `theta = atan2(-kx, sx)` picks the
/// transform axis further from degenerate (`prefer_sin`), `scale_x` is the
/// device-space x scale quantized to quarter-octave steps (so the size depends
/// only on the zoom bucket, not the page position), and the texture is
/// `ceil(scale_x * max(view_width, aspect * view_height))` wide with its height
/// derived from the aspect ratio. `max_side` is the device texture limit.
pub(crate) fn texture_size(image: &Image, size: Size, ctx: &PaintCtx, max_side: u32) -> (u32, u32) {
    let ppp = f64::from(ctx.pixels_per_point);
    let sx = (ctx.to_screen.sx * ppp) as f32;
    let kx = (ctx.to_screen.kx * ppp) as f32;

    let theta = (-kx).atan2(sx);
    let prefer_sin = theta.sin().abs() > std::f32::consts::FRAC_1_SQRT_2;
    let raw_scale_x = if prefer_sin {
        (kx / theta.sin()).abs()
    } else {
        (sx / theta.cos()).abs()
    };
    // Quantize the physical scale to quarter-octave steps so textures are
    // shared across pan and in-bucket zoom; the result is independent of the
    // page position, so panning never changes the texture size.
    let scale_x = scale_bucket(raw_scale_x);

    let aspect = (image.width() as f32) / (image.height() as f32);
    let view_width = size.x.to_pt() as f32;
    let view_height = size.y.to_pt() as f32;
    let width = (scale_x * view_width.max(aspect * view_height)).ceil() as u32;
    let height = ((width as f32) / aspect).ceil() as u32;

    let (mut width, mut height) = (width.max(1), height.max(1));
    let longest = width.max(height);
    if longest > max_side {
        let factor = f64::from(max_side) / f64::from(longest);
        width = ((f64::from(width) * factor) as u32).max(1);
        height = ((f64::from(height) * factor) as u32).max(1);
    }
    (width, height)
}

/// Chooses the texture key and conversion path for one image.
///
/// The GPU-source path uploads a bounded source texture once and lets the GPU
/// sample it at any zoom. Small images, pixelated images, and images whose
/// target is far below the source stay on the exact CPU target path to avoid
/// uploading large textures and to preserve nearest-neighbor semantics.
pub(crate) fn image_key(
    image: &Image,
    size: Size,
    ctx: &PaintCtx,
    max_side: u32,
    image_max_side: u32,
    strategy: ImageStrategy,
) -> ImageKey {
    let target = texture_size(image, size, ctx, max_side);
    let source_cap = max_side.min(image_max_side).max(1);
    let pixelated = matches!(image.scaling(), Smart::Custom(ImageScaling::Pixelated));

    if matches!(image.kind(), ImageKind::Raster(_)) && !pixelated {
        let (source_width, source_height) = source_size(image);
        let source_pixels = u64::from(source_width) * u64::from(source_height);
        let target_pixels = u64::from(target.0) * u64::from(target.1);
        let use_gpu = match strategy {
            ImageStrategy::Cpu => false,
            ImageStrategy::Gpu => true,
            ImageStrategy::Auto => {
                source_pixels > AUTO_CPU_MAX_SOURCE_PIXELS
                    && target_pixels.saturating_mul(4) >= source_pixels
            }
        };
        if use_gpu {
            let upload = source_upload_size(source_width, source_height, source_cap);
            return ImageKey::new(image.clone(), upload.0, upload.1, ImageMode::GpuSource);
        }
    }

    if matches!(image.kind(), ImageKind::Svg(_)) && strategy != ImageStrategy::Cpu {
        let upload = svg_source_size(image, ctx.pixels_per_point, source_cap);
        return ImageKey::new(image.clone(), upload.0, upload.1, ImageMode::GpuSource);
    }

    ImageKey::new(image.clone(), target.0, target.1, ImageMode::CpuTarget)
}

fn svg_source_size(image: &Image, ppp: f32, max_side: u32) -> (u32, u32) {
    let scale = f64::from(ppp) * SVG_SOURCE_SCALE;
    let width = (image.width().max(1.0) * scale).round().max(1.0) as u32;
    let height = (image.height().max(1.0) * scale).round().max(1.0) as u32;
    let cap = max_side.max(1);
    let capped = cap_source_size(width, height, cap);
    let longest = capped.0.max(capped.1);
    if longest >= SVG_SOURCE_MIN_SIDE.min(cap) {
        return capped;
    }
    let factor = f64::from(SVG_SOURCE_MIN_SIDE.min(cap)) / f64::from(longest);
    cap_source_size(
        (f64::from(capped.0) * factor).round().max(1.0) as u32,
        (f64::from(capped.1) * factor).round().max(1.0) as u32,
        cap,
    )
}

/// A raster image's source dimensions in pixels, or a vector image's natural
/// dimensions rounded to pixels as a conservative sizing hint.
fn source_size(image: &Image) -> (u32, u32) {
    let width = image.width().max(1.0).round() as u32;
    let height = image.height().max(1.0).round() as u32;
    (width, height)
}

/// Caps the source upload to the device and renderer limits while preserving
/// aspect ratio.
fn source_upload_size(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    cap_source_size(width, height, max_side.max(1))
}

fn cap_source_size(width: u32, height: u32, cap: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= cap {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(cap) / f64::from(longest);
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
}

/// The four screen-space corners of the image's box, in `[top-left, top-right,
/// bottom-right, bottom-left]` order.
pub(crate) fn quad_corners(ctx: &PaintCtx, size: Size) -> [egui::Pos2; 4] {
    let (w, h) = (size.x.to_pt(), size.y.to_pt());
    [
        ctx.to_screen.map(0.0, 0.0),
        ctx.to_screen.map(w, 0.0),
        ctx.to_screen.map(w, h),
        ctx.to_screen.map(0.0, h),
    ]
    .map(|(x, y)| egui::pos2(x as f32, y as f32))
}

#[cfg(test)]
mod tests {
    use typst_library::foundations::Bytes;
    use typst_library::layout::Abs;
    use typst_library::visualize::{PixelEncoding, PixelFormat, RasterImage};

    use super::*;
    use crate::affine::Affine2;

    fn raster() -> Image {
        let raster = RasterImage::plain(
            Bytes::new(vec![255; 4 * 4 * 4]),
            PixelFormat {
                encoding: PixelEncoding::Rgba8,
                width: 4,
                height: 4,
            },
        )
        .expect("raster");
        Image::plain(ImageKind::Raster(raster))
    }

    fn ctx() -> PaintCtx {
        PaintCtx {
            to_screen: Affine2::IDENTITY,
            container_to_screen: Affine2::IDENTITY,
            container_size: (100.0, 100.0),
            pixels_per_point: 1.0,
            on_text: false,
        }
    }

    #[test]
    fn image_max_side_caps_the_gpu_source_key() {
        let image = raster();
        let size = Size::new(Abs::pt(100.0), Abs::pt(100.0));
        let key = image_key(&image, size, &ctx(), 4096, 2, ImageStrategy::Gpu);
        assert_eq!((key.width, key.height), (2, 2));
        assert_eq!(key.mode, ImageMode::GpuSource);

        let uncapped = image_key(
            &image,
            size,
            &ctx(),
            4096,
            DEFAULT_IMAGE_MAX_SIDE,
            ImageStrategy::Gpu,
        );
        assert_eq!((uncapped.width, uncapped.height), (4, 4));
    }
}
