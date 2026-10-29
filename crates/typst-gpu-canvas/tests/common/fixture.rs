//! Frame and item constructors shared by the fidelity suites.
//!
//! These are the small hand-built typst values (`Point`, `Frame`, `FrameItem`,
//! `Curve`, `Gradient`, `Image`, `TextItem`) that more than one suite needs to
//! build a fixture. Suite-specific geometry (lines, polylines, clips) stays in
//! the suite that owns it.

use std::sync::Arc;

use typst_library::foundations::{Bytes, Smart};
use typst_library::layout::{Abs, Angle, Em, Frame, FrameItem, Point, Ratio, Size};
use typst_library::text::{Font, FontVariant, FontVariations, Glyph, Lang, TextItem};
use typst_library::visualize::{
    Color, ColorSpace, Curve, FixedStroke, Geometry, Gradient, Image, ImageKind, LinearGradient,
    Paint, PixelEncoding, PixelFormat, ProcessColorSpace, RasterImage, RelativeTo, SvgImage,
};
use typst_syntax::Span;

/// Parses `svg` into a typst SVG image.
pub fn svg_image(svg: &str) -> Image {
    let svg = SvgImage::new(Bytes::new(svg.as_bytes().to_vec())).expect("svg parses");
    Image::plain(ImageKind::Svg(svg))
}

/// A `data:` URI holding a `side`x`side` RGBA gradient PNG, for embedding in an
/// SVG `<image>` element.
pub fn raster_png_data_uri(side: u32) -> String {
    use image::ImageEncoder;

    let denominator = side.saturating_sub(1).max(1);
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let r = (x * 255 / denominator) as u8;
            let g = (y * 255 / denominator) as u8;
            rgba.extend_from_slice(&[r, g, 128, 255]);
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, side, side, image::ExtendedColorType::Rgba8)
        .expect("encode png");
    format!("data:image/png;base64,{}", base64(&png))
}

/// Minimal standard base64 encoder for embedding test images.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A typst point at `(x, y)` in points.
pub fn p(x: f64, y: f64) -> Point {
    Point::new(Abs::pt(x), Abs::pt(y))
}

/// A square soft frame of edge `size` points.
pub fn rect_frame(size: f64) -> Frame {
    Frame::soft(Size::new(Abs::pt(size), Abs::pt(size)))
}

/// A solid-filled rectangle item of `w` x `h` points.
pub fn solid_rect(w: f64, h: f64, color: Color) -> FrameItem {
    FrameItem::Shape(
        Geometry::Rect(Size::new(Abs::pt(w), Abs::pt(h))).filled(Paint::Solid(color)),
        Span::detached(),
    )
}

/// A circle (four cubics) centered at `(cx, cy)` with radius `r`; `reversed`
/// flips the contour winding.
pub fn ellipse_curve(cx: f64, cy: f64, r: f64, reversed: bool) -> Curve {
    let m = 0.551_784 * r;
    let at = |x: f64, y: f64| p(cx + x, cy + y);
    let mut c = Curve::new();
    c.move_(at(-r, 0.0));
    if reversed {
        c.cubic(at(-r, m), at(-m, r), at(0.0, r));
        c.cubic(at(m, r), at(r, m), at(r, 0.0));
        c.cubic(at(r, -m), at(m, -r), at(0.0, -r));
        c.cubic(at(-m, -r), at(-r, -m), at(-r, 0.0));
    } else {
        c.cubic(at(-r, -m), at(-m, -r), at(0.0, -r));
        c.cubic(at(m, -r), at(r, -m), at(r, 0.0));
        c.cubic(at(r, m), at(m, r), at(0.0, r));
        c.cubic(at(-m, r), at(-r, m), at(-r, 0.0));
    }
    c.close();
    c
}

/// A rounded-rectangle curve spanning `(x, y)..(x + w, y + h)`.
pub fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Curve {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.552_284_749_8 * r;
    let (x0, y0, x1, y1) = (x, y, x + w, y + h);
    let mut c = Curve::new();
    c.move_(p(x0 + r, y0));
    c.line(p(x1 - r, y0));
    c.cubic(p(x1 - r + k, y0), p(x1, y0 + r - k), p(x1, y0 + r));
    c.line(p(x1, y1 - r));
    c.cubic(p(x1, y1 - r + k), p(x1 - r + k, y1), p(x1 - r, y1));
    c.line(p(x0 + r, y1));
    c.cubic(p(x0 + r - k, y1), p(x0, y1 - r + k), p(x0, y1 - r));
    c.line(p(x0, y0 + r));
    c.cubic(p(x0, y0 + r - k), p(x0 + r - k, y0), p(x0 + r, y0));
    c.close();
    c
}

/// A 4x4 RGBA raster image with a smooth red/green ramp and a blue checker.
pub fn raster_image() -> Image {
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..4u8 {
            let r = x * 85;
            let g = y * 85;
            let b = if (x + y) % 2 == 0 { 40 } else { 220 };
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    let raster = RasterImage::plain(
        Bytes::new(pixels),
        PixelFormat {
            encoding: PixelEncoding::Rgba8,
            width: 4,
            height: 4,
        },
    )
    .expect("raster image decodes");
    Image::plain(ImageKind::Raster(raster))
}

/// A 256x256 RGBA raster image with varying alpha.
pub fn transparent_raster_image() -> Image {
    const SIDE: u32 = 256;
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let alpha = ((x + y) % 256) as u8;
            pixels.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 120, alpha]);
        }
    }
    let raster = RasterImage::plain(
        Bytes::new(pixels),
        PixelFormat {
            encoding: PixelEncoding::Rgba8,
            width: SIDE,
            height: SIDE,
        },
    )
    .expect("transparent raster image decodes");
    Image::plain(ImageKind::Raster(raster))
}

/// A 640x640 solid RGBA raster image.
///
/// Its source pixel count exceeds the crate's synchronous conversion threshold,
/// so first use takes the async loader path. A solid color resizes to an exact
/// constant texture, so the loaded result matches the reference with no
/// calibration slack.
pub fn large_raster_image() -> Image {
    const SIDE: u32 = 640;
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for _ in 0..(SIDE * SIDE) {
        pixels.extend_from_slice(&[200, 80, 40, 255]);
    }
    let raster = RasterImage::plain(
        Bytes::new(pixels),
        PixelFormat {
            encoding: PixelEncoding::Rgba8,
            width: SIDE,
            height: SIDE,
        },
    )
    .expect("raster image decodes");
    Image::plain(ImageKind::Raster(raster))
}

/// A red-to-green-to-blue linear gradient in sRGB with relativeness `relative`.
pub fn linear_gradient(relative: Smart<RelativeTo>) -> Gradient {
    Gradient::Linear(Arc::new(LinearGradient {
        stops: vec![
            (Color::RED, Ratio::zero()),
            (Color::GREEN, Ratio::new(0.5)),
            (Color::BLUE, Ratio::one()),
        ],
        angle: Angle::rad(0.6),
        space: ColorSpace::Process(ProcessColorSpace::Srgb),
        relative,
        anti_alias: true,
    }))
}

/// A text run built from the first bundled `typst-assets` font.
///
/// The size is deliberately large in callers: `typst-render` rasterizes a glyph
/// through `pixglyph` while `ppem <= 100` and through the vector outline path
/// above that, whereas this crate always tessellates the outline. Sizes above
/// `50pt` (i.e. `ppem > 100` at the suite's `2.0` scale) keep both sides on the
/// outline path.
pub fn text_item(text: &str, size_pt: f64, fill: Color, stroke: Option<FixedStroke>) -> TextItem {
    let data = typst_assets::fonts().next().expect("bundled font");
    let font = Font::new(Bytes::new(data), 0).expect("font parses");
    let instance = font.instantiate(
        FontVariant::default(),
        Abs::pt(size_pt),
        &FontVariations::default(),
    );
    let glyphs = text
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let id = instance.ttf().glyph_index(ch).expect("glyph");
            let start = i as u16;
            Glyph {
                id: id.0,
                x_advance: instance.x_advance(id.0).expect("advance"),
                x_offset: Em::zero(),
                y_advance: Em::zero(),
                y_offset: Em::zero(),
                range: start..start + 1,
                span: (Span::detached(), 0),
            }
        })
        .collect();
    TextItem {
        font: instance,
        size: Abs::pt(size_pt),
        fill: Paint::Solid(fill),
        stroke,
        lang: Lang::ENGLISH,
        region: None,
        text: text.into(),
        glyphs,
    }
}

/// A black, unstroked text run.
pub fn text_run(text: &str, size_pt: f64) -> TextItem {
    text_item(text, size_pt, Color::BLACK, None)
}
