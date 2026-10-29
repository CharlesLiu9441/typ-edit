//! GPU fidelity tests for the canvas renderer.
//!
//! Each fixture is a 128x128-point typst frame whose capture goes through the
//! wgpu harness ([`common::capture_gpu`], 4x MSAA, `Rgba8Unorm`, `Stencil8`
//! attachment) and is compared against `typst-render`'s rasterization at
//! 2 px/pt with `dify`'s anti-aliasing-aware per-pixel diff. Both sides are
//! composited over opaque white.
//!
//! # Calibration
//!
//! MSAA gives 4x coverage at shape edges (analytic in the reference), so
//! edge pixels differ by up to one sample step; `dify` ignores pixels it
//! classifies as anti-aliased, and the bounds below pin the observed counts.
//! A fixture whose bound exceeds ~0.2% of the pixels (131 of 65536) is a red
//! flag.

use std::sync::Arc;

use typst_library::foundations::{Bytes, Smart};
use typst_library::layout::{
    Abs, Angle, Axes, Frame, FrameItem, GroupItem, Ratio, Size, Transform,
};
use typst_library::text::{Font, FontVariant, FontVariations, Glyph, Lang, TextItem};
use typst_library::visualize::{
    Color, ColorSpace, ConicGradient, Curve, DashPattern, FillRule, FixedStroke, Geometry,
    Gradient, Image, LineCap, LineJoin, LinearGradient, Paint, ProcessColorSpace, RadialGradient,
    RelativeTo,
};
use typst_syntax::Span;

mod common;

use common::fixture::*;

/// Fixture edge, in typst points.
const SIZE: u32 = 128;
/// Pixels per point used by the reference renderer and mirrored on our side.
const SCALE: f32 = 2.0;

/// Renders, diffs and asserts one fixture. Returns the failing-pixel count.
fn check(name: &str, frame: Frame, max_failed: i32) -> i32 {
    check_with_strategy(
        name,
        frame,
        max_failed,
        typst_gpu_canvas::ImageStrategy::Cpu,
    )
}

fn check_with_strategy(
    name: &str,
    frame: Frame,
    max_failed: i32,
    strategy: typst_gpu_canvas::ImageStrategy,
) -> i32 {
    let reference = common::over_white(common::reference(frame.clone()));
    let captured = common::over_white(common::capture_gpu_with_strategy(frame, SCALE, strategy));
    common::diff_and_assert_at(
        &common::output_dir("gpu-canvas"),
        name,
        &reference,
        &captured,
        max_failed,
    )
}

/// A filled curve item.
fn filled_curve(curve: Curve, color: Color, fill_rule: FillRule) -> FrameItem {
    let mut shape = Geometry::Curve(curve).filled(Paint::Solid(color));
    shape.fill_rule = fill_rule;
    FrameItem::Shape(shape, Span::detached())
}

/// Wraps `child` in a group clipped to `clip`, inside a `SIZE`-point root.
fn clipped_root(clip: Curve, child: Frame) -> Frame {
    let mut group = GroupItem::new(child);
    group.clip = Some(clip);
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(p(0.0, 0.0), FrameItem::Group(group));
    frame
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Three overlapping opaque rectangles.
fn fixture_solid_rects() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(p(8.0, 10.0), solid_rect(70.0, 50.0, Color::RED));
    frame.push(p(30.0, 40.0), solid_rect(80.0, 60.0, Color::BLUE));
    frame.push(p(60.0, 6.0), solid_rect(40.0, 30.0, Color::GREEN));
    frame
}

/// A filled circle.
fn fixture_solid_circle() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        filled_curve(
            ellipse_curve(64.0, 64.0, 52.0, false),
            Color::RED,
            FillRule::NonZero,
        ),
    );
    frame
}

/// An EvenOdd ring (circle with a concentric hole).
fn fixture_solid_ring_even_odd() -> Frame {
    let mut ring = ellipse_curve(64.0, 64.0, 52.0, false);
    ring.0.extend(ellipse_curve(64.0, 64.0, 26.0, true).0);
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        filled_curve(ring, Color::TEAL, FillRule::EvenOdd),
    );
    frame
}

/// A text run at a size whose `ppem` (116) keeps `typst-render` on its vector
/// outline path, like the existing `frame_fidelity` text fixtures.
fn fixture_text_plain() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(p(6.0, 100.0), FrameItem::Text(text_run("Wg", 58.0)));
    frame
}

/// Body-sized text (24 ppem), where `typst-render` rasterizes glyphs with
/// `pixglyph` while this renderer always tessellates outlines; recorded for the
/// small-size comparison, not held to the outline-path bound.
fn fixture_text_small() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 70.0),
        FrameItem::Text(text_run("Hamburgefonstiv", 12.0)),
    );
    frame
}

/// A CJK text run built from a system font, since `typst-assets` bundles no
/// CJK font. Returns `None` on hosts without one.
fn fixture_text_cjk() -> Option<Frame> {
    let instance = system_cjk_instance(58.0)?;
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(6.0, 100.0),
        FrameItem::Text(text_item_with(instance, "你好世界", 58.0)),
    );
    Some(frame)
}

/// Instantiates a system CJK font at `size_pt`, or `None` when unavailable.
fn system_cjk_instance(size_pt: f64) -> Option<typst_library::text::FontInstance> {
    const CANDIDATES: [&str; 3] = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simsun.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    for path in CANDIDATES {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        let Some(font) = Font::new(Bytes::new(data), 0) else {
            continue;
        };
        return Some(font.instantiate(
            FontVariant::default(),
            Abs::pt(size_pt),
            &FontVariations::default(),
        ));
    }
    None
}

/// Builds a text run over an existing font instance (the `common::fixture`
/// helper always uses the first bundled font).
fn text_item_with(
    instance: typst_library::text::FontInstance,
    text: &str,
    size_pt: f64,
) -> TextItem {
    let glyphs = text
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let id = instance.ttf().glyph_index(ch).expect("glyph");
            let start = i as u16;
            Glyph {
                id: id.0,
                x_advance: instance.x_advance(id.0).expect("advance"),
                x_offset: typst_library::layout::Em::zero(),
                y_advance: typst_library::layout::Em::zero(),
                y_offset: typst_library::layout::Em::zero(),
                range: start..start + 1,
                span: (Span::detached(), 0),
            }
        })
        .collect();
    TextItem {
        font: instance,
        size: Abs::pt(size_pt),
        fill: Paint::Solid(Color::BLACK),
        stroke: None,
        lang: Lang::ENGLISH,
        region: None,
        text: text.into(),
        glyphs,
    }
}

/// A concave star polygon (NonZero).
fn fixture_solid_polygon() -> Frame {
    let mut c = Curve::new();
    c.move_(p(64.0, 8.0));
    c.line(p(78.0, 48.0));
    c.line(p(120.0, 48.0));
    c.line(p(86.0, 73.0));
    c.line(p(99.0, 115.0));
    c.line(p(64.0, 89.0));
    c.line(p(29.0, 115.0));
    c.line(p(42.0, 73.0));
    c.line(p(8.0, 48.0));
    c.line(p(50.0, 48.0));
    c.close();
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        filled_curve(c, Color::ORANGE, FillRule::NonZero),
    );
    frame
}

/// Two overlapping translucent rectangles over the white ground.
fn fixture_solid_translucent() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    let mut shape = Geometry::Rect(Size::new(Abs::pt(70.0), Abs::pt(70.0)))
        .filled(Paint::Solid(Color::RED.with_alpha(0.5)));
    shape.fill_rule = FillRule::NonZero;
    frame.push(p(10.0, 20.0), FrameItem::Shape(shape, Span::detached()));
    let mut shape = Geometry::Rect(Size::new(Abs::pt(70.0), Abs::pt(70.0)))
        .filled(Paint::Solid(Color::BLUE.with_alpha(0.5)));
    shape.fill_rule = FillRule::NonZero;
    frame.push(p(40.0, 40.0), FrameItem::Shape(shape, Span::detached()));
    frame
}

/// A scaled group containing a rectangle (BVH group recursion, no clip).
fn fixture_group_transform() -> Frame {
    let mut child = rect_frame(f64::from(SIZE));
    child.push(p(0.0, 0.0), solid_rect(40.0, 30.0, Color::TEAL));
    let mut group = GroupItem::new(child);
    group.transform = Transform {
        sx: Ratio::new(1.5),
        ky: Ratio::new(0.0),
        kx: Ratio::new(0.0),
        sy: Ratio::new(1.5),
        tx: Abs::pt(20.0),
        ty: Abs::pt(30.0),
    };
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(p(0.0, 0.0), FrameItem::Group(group));
    frame
}

/// A rectangle filled with the shared red/green/blue linear gradient.
fn fixture_gradient_linear() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 14.0),
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(112.0), Abs::pt(100.0)))
                .filled(Paint::Gradient(linear_gradient(Smart::Auto))),
            Span::detached(),
        ),
    );
    frame
}

/// A filled circle with a focal radial gradient (offset focal center + radius).
fn fixture_gradient_radial_focal() -> Frame {
    let gradient = Gradient::Radial(Arc::new(RadialGradient {
        stops: vec![(Color::WHITE, Ratio::zero()), (Color::RED, Ratio::one())],
        center: Axes::splat(Ratio::new(0.5)),
        radius: Ratio::new(0.5),
        focal_center: Axes::new(Ratio::new(0.3), Ratio::new(0.35)),
        focal_radius: Ratio::new(0.12),
        space: ColorSpace::Process(ProcessColorSpace::Srgb),
        relative: Smart::Auto,
        anti_alias: true,
    }));
    let mut frame = rect_frame(f64::from(SIZE));
    let mut shape =
        Geometry::Curve(ellipse_curve(64.0, 64.0, 54.0, false)).filled(Paint::Gradient(gradient));
    shape.fill_rule = FillRule::NonZero;
    frame.push(p(0.0, 0.0), FrameItem::Shape(shape, Span::detached()));
    frame
}

/// A rectangle with a conic gradient.
fn fixture_gradient_conic() -> Frame {
    let gradient = Gradient::Conic(Arc::new(ConicGradient {
        stops: vec![
            (Color::RED, Ratio::zero()),
            (Color::GREEN, Ratio::new(0.25)),
            (Color::BLUE, Ratio::new(0.5)),
            (Color::YELLOW, Ratio::new(0.75)),
            (Color::RED, Ratio::one()),
        ],
        angle: Angle::rad(0.4),
        center: Axes::splat(Ratio::new(0.5)),
        space: ColorSpace::Process(ProcessColorSpace::Srgb),
        relative: Smart::Auto,
        anti_alias: true,
    }));
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 8.0),
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(112.0), Abs::pt(112.0)))
                .filled(Paint::Gradient(gradient)),
            Span::detached(),
        ),
    );
    frame
}

/// The shared linear gradient interpolated in a non-sRGB process color space,
/// which only a LUT baked with typst's own sampler reproduces exactly.
fn fixture_gradient_hsv() -> Frame {
    let gradient = Gradient::Linear(Arc::new(LinearGradient {
        stops: vec![
            (Color::RED, Ratio::zero()),
            (Color::GREEN, Ratio::new(0.5)),
            (Color::BLUE, Ratio::one()),
        ],
        angle: Angle::rad(0.2),
        space: ColorSpace::Process(ProcessColorSpace::Hsv),
        relative: Smart::Auto,
        anti_alias: true,
    }));
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 8.0),
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(112.0), Abs::pt(112.0)))
                .filled(Paint::Gradient(gradient)),
            Span::detached(),
        ),
    );
    frame
}

/// A parent-relative gradient inside a scaled group.
fn fixture_gradient_parent_relative() -> Frame {
    let gradient = Gradient::Linear(Arc::new(LinearGradient {
        stops: vec![(Color::RED, Ratio::zero()), (Color::BLUE, Ratio::one())],
        angle: Angle::rad(0.0),
        space: ColorSpace::Process(ProcessColorSpace::Srgb),
        relative: Smart::Custom(RelativeTo::Parent),
        anti_alias: true,
    }));
    let mut child = rect_frame(f64::from(SIZE));
    child.push(
        p(10.0, 10.0),
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(60.0), Abs::pt(50.0)))
                .filled(Paint::Gradient(gradient)),
            Span::detached(),
        ),
    );
    let mut group = GroupItem::new(child);
    group.transform = Transform {
        sx: Ratio::new(1.2),
        ky: Ratio::new(0.0),
        kx: Ratio::new(0.0),
        sy: Ratio::new(1.2),
        tx: Abs::pt(8.0),
        ty: Abs::pt(12.0),
    };
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(p(0.0, 0.0), FrameItem::Group(group));
    frame
}

/// A scaled 4x4 raster image.
fn fixture_image_raster() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 8.0),
        FrameItem::Image(
            raster_image(),
            Size::new(Abs::pt(112.0), Abs::pt(112.0)),
            Span::detached(),
        ),
    );
    frame
}

/// A non-square raster image (different horizontal and vertical scaling).
///
/// The box height is 72 rather than 70 points on purpose: with a 4x4 square
/// source the texture is `200x200`, and a 140-pixel-tall quad puts nearest
/// sampling exactly on texel boundaries every 7 rows, where GPU hardware and
/// tiny-skia's floor-based lookup disagree on the tie. At 144 pixels no row
/// lands on an exact boundary.
fn fixture_image_raster_nonsquare() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(12.0, 20.0),
        FrameItem::Image(
            raster_image(),
            Size::new(Abs::pt(100.0), Abs::pt(72.0)),
            Span::detached(),
        ),
    );
    frame
}

fn fixture_image_transparent_large() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        FrameItem::Image(
            transparent_raster_image(),
            Size::new(Abs::pt(f64::from(SIZE)), Abs::pt(f64::from(SIZE))),
            Span::detached(),
        ),
    );
    frame
}

/// A large image placed outside the left-hand viewport fixture.
fn fixture_image_offscreen() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(80.0, 8.0),
        FrameItem::Image(
            large_raster_image(),
            Size::new(Abs::pt(40.0), Abs::pt(40.0)),
            Span::detached(),
        ),
    );
    frame
}

/// A large (640x640) raster image, whose source pixel count exceeds the
/// synchronous conversion threshold and so takes the async loader path.
fn fixture_image_raster_large() -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(8.0, 8.0),
        FrameItem::Image(
            large_raster_image(),
            Size::new(Abs::pt(112.0), Abs::pt(112.0)),
            Span::detached(),
        ),
    );
    frame
}

/// Wraps an SVG image in a `SIZE`-point frame, laid out to fill the frame.
fn svg_frame(image: Image) -> Frame {
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        FrameItem::Image(
            image,
            Size::new(Abs::pt(f64::from(SIZE)), Abs::pt(f64::from(SIZE))),
            Span::detached(),
        ),
    );
    frame
}

/// A filled cubic bezier path.
fn fixture_svg_bezier() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><path d="M20 100 C 40 20 88 20 108 100 Z" fill="#3377cc"/></svg>"##,
    ))
}

/// An EvenOdd ring (two circles in one path).
fn fixture_svg_ring_even_odd() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><path fill-rule="evenodd" fill="#22aa88" d="M12 64 a52 52 0 1 0 104 0 a52 52 0 1 0 -104 0 Z M38 64 a26 26 0 1 0 52 0 a26 26 0 1 0 -52 0 Z"/></svg>"##,
    ))
}

/// A round-capped, round-joined stroke plus a dashed stroke.
fn fixture_svg_strokes() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><path d="M16 96 C 40 24 88 24 112 96" fill="none" stroke="#cc3355" stroke-width="14" stroke-linecap="round" stroke-linejoin="round"/><path d="M12 64 H 116" fill="none" stroke="#222222" stroke-width="9" stroke-dasharray="10 6" stroke-dashoffset="3"/></svg>"##,
    ))
}

/// A scaled/skewed group with reduced opacity.
fn fixture_svg_group_transform() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><g transform="translate(20 30) scale(1.5) skewX(20)" opacity="0.6"><rect x="0" y="0" width="40" height="30" fill="#1188aa"/></g></svg>"##,
    ))
}

/// A rectangle filled with a three-stop linear gradient.
fn fixture_svg_linear_gradient() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><linearGradient id="g" gradientUnits="userSpaceOnUse" x1="8" y1="14" x2="120" y2="114"><stop offset="0" stop-color="#ff0000"/><stop offset="0.5" stop-color="#00ff00"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect x="8" y="14" width="112" height="100" fill="url(#g)"/></svg>"##,
    ))
}

/// A circle filled with a focal radial gradient.
fn fixture_svg_radial_gradient() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><radialGradient id="g" gradientUnits="userSpaceOnUse" cx="64" cy="64" r="54" fx="44" fy="48"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#ff0000"/></radialGradient></defs><circle cx="64" cy="64" r="54" fill="url(#g)"/></svg>"##,
    ))
}

/// A group clipped by a clipPath with two union rectangle paths.
fn fixture_svg_clip_path() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><clipPath id="c"><rect x="10" y="10" width="60" height="80"/><rect x="50" y="30" width="60" height="80"/></clipPath></defs><g clip-path="url(#c)"><rect x="0" y="0" width="128" height="128" fill="#dd2222"/><rect x="40" y="34" width="80" height="80" fill="#2222dd"/></g></svg>"##,
    ))
}

/// An SVG embedding a PNG raster.
///
/// The source is `176`px square so it matches the device target for the 88pt
/// box at `SCALE` (88pt * 2px/pt), making both our NEAREST copy and resvg's
/// bilinear copy an exact 1:1 pixel copy.
fn fixture_svg_embedded_image() -> Frame {
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><image x="20" y="20" width="88" height="88" href="{}"/></svg>"##,
        raster_png_data_uri(176)
    );
    svg_frame(svg_image(&svg))
}

/// An SVG using a mask, which must take the per-image resvg raster fallback.
fn fixture_svg_mask_fallback() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><mask id="m"><rect x="0" y="0" width="128" height="128" fill="#ffffff"/><circle cx="64" cy="64" r="40" fill="#000000"/></mask></defs><rect x="10" y="10" width="108" height="108" fill="#3355cc" mask="url(#m)"/></svg>"##,
    ))
}

/// A radial gradient with default `objectBoundingBox` units over a non-square
/// rect, which must render as an ellipse in gradient space.
fn fixture_svg_radial_object_bbox() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><radialGradient id="g"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#ff0000"/></radialGradient></defs><rect x="10" y="30" width="108" height="60" fill="url(#g)"/></svg>"##,
    ))
}

/// A clip path in `objectBoundingBox` units (its bbox transform is applied by
/// `ClipPath::transform`).
fn fixture_svg_clip_object_bbox() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><clipPath id="c" clipPathUnits="objectBoundingBox"><rect x="0.1" y="0.1" width="0.6" height="0.8"/></clipPath></defs><g clip-path="url(#c)"><rect x="20" y="20" width="88" height="88" fill="#dd2222"/></g></svg>"##,
    ))
}

/// A clipped group carrying its own transform (the clip must follow it).
fn fixture_svg_clip_transformed_element() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><clipPath id="c"><rect x="10" y="10" width="60" height="60"/></clipPath></defs><g clip-path="url(#c)" transform="translate(20 20) scale(1.2)"><rect x="0" y="0" width="128" height="128" fill="#dd2222"/></g></svg>"##,
    ))
}

/// A clip path that is itself clipped, which must take the raster fallback.
fn fixture_svg_nested_clip() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><clipPath id="outer"><rect x="10" y="10" width="100" height="100"/></clipPath><clipPath id="inner" clip-path="url(#outer)"><circle cx="64" cy="64" r="40"/></clipPath></defs><rect x="0" y="0" width="128" height="128" fill="#3355cc" clip-path="url(#inner)"/></svg>"##,
    ))
}

/// A linear gradient with `spreadMethod="repeat"`, which must take the raster
/// fallback (the shader only pads).
fn fixture_svg_spread_repeat() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><linearGradient id="g" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="20" y2="0" spreadMethod="repeat"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect x="8" y="8" width="112" height="112" fill="url(#g)"/></svg>"##,
    ))
}

/// Two overlapping rects inside a translucent group, which must take the raster
/// fallback (resvg composites the group as a layer).
fn fixture_svg_opacity_group() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><g opacity="0.5"><rect x="10" y="20" width="70" height="70" fill="#ff0000"/><rect x="40" y="40" width="70" height="70" fill="#0000ff"/></g></svg>"##,
    ))
}

/// A single raster image inside a translucent group, which stays vector and is
/// tinted by the inherited alpha. The source is 176px (88pt * 2px/pt), matching
/// the device target for an exact 1:1 copy.
fn fixture_svg_opacity_image() -> Frame {
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><g opacity="0.5"><image x="20" y="20" width="88" height="88" href="{}"/></g></svg>"##,
        raster_png_data_uri(176)
    );
    svg_frame(svg_image(&svg))
}

/// A path whose quad follows a close, which must restart at the subpath start.
fn fixture_svg_quad_after_close() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><path d="M20 80 L60 80 L60 40 Z Q90 20 108 100 Z" fill="#3388cc"/></svg>"##,
    ))
}

/// A linear gradient with identical endpoints, which paints its last stop.
fn fixture_svg_zero_linear() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><linearGradient id="g" gradientUnits="userSpaceOnUse" x1="64" y1="64" x2="64" y2="64"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect x="8" y="8" width="112" height="112" fill="url(#g)"/></svg>"##,
    ))
}

/// A radial gradient with a zero radius, which tiny-skia renders as `None`;
/// it must take the raster fallback and paint nothing.
fn fixture_svg_zero_radial() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><radialGradient id="g" gradientUnits="userSpaceOnUse" cx="64" cy="64" r="0"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#ff0000"/></radialGradient></defs><rect x="8" y="8" width="112" height="112" fill="url(#g)"/></svg>"##,
    ))
}

/// A translucent group around a path that is both filled and stroked (two paint
/// ops), which must take the raster fallback for correct layer compositing.
fn fixture_svg_opacity_fill_stroke() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><g opacity="0.5"><path d="M24 104 C 40 20 88 20 104 104 Z" fill="#ee2233" stroke="#2244ee" stroke-width="8"/></g></svg>"##,
    ))
}

/// A radial gradient with a non-zero focal radius (`fr="12"`). Our shader's
/// focal-radial formula diverges from tiny-skia's general two-point radial, so
/// the scan rejects `fr > 0` and this takes the raster fallback.
fn fixture_svg_radial_focal_radius() -> Frame {
    svg_frame(svg_image(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><defs><radialGradient id="g" gradientUnits="userSpaceOnUse" cx="64" cy="64" r="54" fx="44" fy="48" fr="12"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#ff0000"/></radialGradient></defs><circle cx="64" cy="64" r="54" fill="url(#g)"/></svg>"##,
    ))
}

/// A thick round-capped cubic stroke.
fn fixture_stroke_curve() -> Frame {
    let mut c = Curve::new();
    c.move_(p(18.0, 106.0));
    c.cubic(p(30.0, 18.0), p(98.0, 18.0), p(110.0, 106.0));
    let stroke = FixedStroke {
        paint: Paint::Solid(Color::BLUE),
        thickness: Abs::pt(16.0),
        cap: LineCap::Round,
        join: LineJoin::Round,
        dash: None,
        miter_limit: Ratio::new(4.0),
    };
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        FrameItem::Shape(Geometry::Curve(c).stroked(stroke), Span::detached()),
    );
    frame
}

/// A dashed horizontal stroke with a phase offset.
fn fixture_stroke_dashed() -> Frame {
    let mut c = Curve::new();
    c.move_(p(12.0, 64.0));
    c.line(p(116.0, 64.0));
    let stroke = FixedStroke {
        paint: Paint::Solid(Color::BLACK),
        thickness: Abs::pt(9.0),
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        dash: Some(DashPattern {
            array: vec![Abs::pt(10.0), Abs::pt(6.0)],
            phase: Abs::pt(3.0),
        }),
        miter_limit: Ratio::new(4.0),
    };
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        FrameItem::Shape(Geometry::Curve(c).stroked(stroke), Span::detached()),
    );
    frame
}

/// A gradient-painted circular stroke.
fn fixture_stroke_gradient() -> Frame {
    let c = ellipse_curve(64.0, 64.0, 44.0, false);
    let stroke = FixedStroke {
        paint: Paint::Gradient(linear_gradient(Smart::Auto)),
        thickness: Abs::pt(14.0),
        cap: LineCap::Round,
        join: LineJoin::Round,
        dash: None,
        miter_limit: Ratio::new(4.0),
    };
    let mut frame = rect_frame(f64::from(SIZE));
    frame.push(
        p(0.0, 0.0),
        FrameItem::Shape(Geometry::Curve(c).stroked(stroke), Span::detached()),
    );
    frame
}

/// Two overlapping solid rectangles under an axis-aligned rectangular clip
/// (the scissor path).
fn fixture_group_clip_rect() -> Frame {
    let mut clip = Curve::rect(Size::new(Abs::pt(84.0), Abs::pt(64.0)));
    clip.translate(p(22.0, 30.0));
    let mut child = rect_frame(f64::from(SIZE));
    child.push(p(0.0, 0.0), solid_rect(128.0, 128.0, Color::RED));
    child.push(p(40.0, 34.0), solid_rect(80.0, 80.0, Color::BLUE));
    clipped_root(clip, child)
}

/// Two overlapping solid rectangles under a rounded clip (the stencil path).
fn fixture_group_clip_curved() -> Frame {
    let clip = rounded_rect(14.0, 14.0, 100.0, 100.0, 34.0);
    let mut child = rect_frame(f64::from(SIZE));
    child.push(p(6.0, 22.0), solid_rect(78.0, 66.0, Color::RED));
    child.push(p(44.0, 40.0), solid_rect(78.0, 66.0, Color::BLUE));
    clipped_root(clip, child)
}

/// A curved clip nested inside another curved clip (exercises the stencil
/// nesting levels).
fn fixture_nested_clips() -> Frame {
    let outer_clip = rounded_rect(10.0, 10.0, 108.0, 108.0, 36.0);
    let inner_clip = ellipse_curve(64.0, 64.0, 44.0, false);

    let mut inner = rect_frame(f64::from(SIZE));
    inner.push(p(6.0, 6.0), solid_rect(112.0, 100.0, Color::RED));
    let mut inner_group = GroupItem::new(inner);
    inner_group.clip = Some(inner_clip);

    let mut outer_child = rect_frame(f64::from(SIZE));
    outer_child.push(p(0.0, 0.0), FrameItem::Group(inner_group));
    outer_child.push(p(4.0, 4.0), solid_rect(120.0, 40.0, Color::BLUE));
    clipped_root(outer_clip, outer_child)
}

/// A linear-gradient rectangle under a curved clip (stencil + gradient
/// pipeline combination).
fn fixture_curved_gradient() -> Frame {
    let clip = ellipse_curve(64.0, 64.0, 50.0, false);
    let mut child = rect_frame(f64::from(SIZE));
    child.push(
        p(10.0, 10.0),
        FrameItem::Shape(
            Geometry::Rect(Size::new(Abs::pt(108.0), Abs::pt(108.0)))
                .filled(Paint::Gradient(linear_gradient(Smart::Auto))),
            Span::detached(),
        ),
    );
    clipped_root(clip, child)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn solid_rects() {
    check("solid_rects", fixture_solid_rects(), 0);
}

#[test]
fn solid_circle() {
    check("solid_circle", fixture_solid_circle(), 0);
}

#[test]
fn solid_ring_even_odd() {
    check("solid_ring_even_odd", fixture_solid_ring_even_odd(), 0);
}

#[test]
fn solid_polygon() {
    check("solid_polygon", fixture_solid_polygon(), 0);
}

#[test]
fn solid_translucent() {
    check("solid_translucent", fixture_solid_translucent(), 0);
}

#[test]
fn group_transform() {
    check("group_transform", fixture_group_transform(), 0);
}

#[test]
fn gradient_linear() {
    check("gradient_linear", fixture_gradient_linear(), 0);
}

#[test]
fn gradient_radial_focal() {
    check("gradient_radial_focal", fixture_gradient_radial_focal(), 0);
}

#[test]
fn gradient_conic() {
    // One hub pixel where the analytic reference and the sampled LUT meet a
    // wrap discontinuity differs without dify classifying it as anti-aliased.
    check("gradient_conic", fixture_gradient_conic(), 1);
}

#[test]
fn gradient_hsv() {
    check("gradient_hsv", fixture_gradient_hsv(), 0);
}

#[test]
fn gradient_parent_relative() {
    check(
        "gradient_parent_relative",
        fixture_gradient_parent_relative(),
        0,
    );
}

#[test]
fn image_raster() {
    check("image_raster", fixture_image_raster(), 0);
}

#[test]
fn image_raster_nonsquare() {
    check(
        "image_raster_nonsquare",
        fixture_image_raster_nonsquare(),
        0,
    );
}

/// Exercises the async image path end to end: the first frame draws a gray
/// placeholder, the worker converts and uploads the texture, and the capture
/// helper waits until the real texture is hot-swapped in. A solid source
/// downscales to an exact constant texture, so the final frame matches
/// `typst-render` at bound 0.
#[test]
fn image_raster_large() {
    check("image_raster_large", fixture_image_raster_large(), 0);
}

/// Phase 0 diagnostics: the large-image fixture must exercise one async
/// conversion and expose a complete cache snapshot. This deliberately does not
/// assert wall-clock times, which vary by GPU and host load.
#[test]
fn image_pipeline_stats_are_observable() {
    let (_, stats) = common::capture_gpu_with_stats(fixture_image_raster_large(), SCALE);
    eprintln!("image_pipeline_stats={stats:?}");
    assert!(
        stats.cache_misses > 0,
        "the fixture must miss the texture cache"
    );
    assert!(
        stats.async_jobs > 0,
        "the large fixture must enqueue an async job"
    );
    assert!(stats.completed > 0, "the upload must complete");
    assert_eq!(
        stats.failed, 0,
        "the fixture must not panic during conversion"
    );
    assert_eq!(stats.in_flight, 0, "the capture helper waits for uploads");
    assert!(stats.cache_entries > 0, "the texture must be cached");
    assert!(
        stats.cache_bytes > 0,
        "the cache must account for uploaded bytes"
    );
}

#[test]
fn image_offscreen_is_not_converted() {
    let (_, stats) = common::capture_gpu_with_clip_stats(
        fixture_image_offscreen(),
        SCALE,
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(64.0, 256.0)),
    );
    eprintln!("image_offscreen_stats={stats:?}");
    assert_eq!(stats.sync_conversions, 0);
    assert_eq!(stats.async_jobs, 0);
    assert_eq!(stats.completed, 0);
    assert_eq!(stats.cache_entries, 0);
}

#[test]
fn image_gpu_source_small_upload() {
    let (_, stats) = common::capture_gpu_steps_with_strategy_stats(
        fixture_image_raster(),
        &[(egui::Pos2::ZERO, SCALE)],
        typst_gpu_canvas::ImageStrategy::Gpu,
    );
    assert!(stats.completed > 0);
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.in_flight, 0);
}

#[test]
fn image_gpu_source_transparent_exact() {
    check_with_strategy(
        "image_gpu_source_transparent",
        fixture_image_transparent_large(),
        0,
        typst_gpu_canvas::ImageStrategy::Gpu,
    );
}

#[test]
fn image_gpu_source_large_with_mips() {
    let (captured, stats) = common::capture_gpu_steps_with_strategy_stats(
        fixture_image_raster_large(),
        &[(egui::Pos2::ZERO, SCALE)],
        typst_gpu_canvas::ImageStrategy::Gpu,
    );
    let reference = common::over_white(common::reference(fixture_image_raster_large()));
    common::diff_and_assert_at(
        &common::output_dir("gpu-canvas"),
        "image_gpu_source_large",
        &reference,
        &common::over_white(captured),
        0,
    );
    eprintln!("image_gpu_source_stats={stats:?}");
    assert!(
        stats.async_jobs > 0,
        "the source texture must load asynchronously"
    );
    assert!(stats.completed > 0, "the source texture must complete");
    assert_eq!(stats.failed, 0);
    assert!(
        stats.upload_bytes > 640 * 640 * 4,
        "the source texture mip chain must be accounted for"
    );
}

#[test]
fn svg_bezier() {
    check("svg_bezier", fixture_svg_bezier(), 0);
}

#[test]
fn svg_ring_even_odd() {
    check("svg_ring_even_odd", fixture_svg_ring_even_odd(), 0);
}

#[test]
fn svg_strokes() {
    check("svg_strokes", fixture_svg_strokes(), 0);
}

#[test]
fn svg_group_transform() {
    check("svg_group_transform", fixture_svg_group_transform(), 0);
}

#[test]
fn svg_linear_gradient() {
    check("svg_linear_gradient", fixture_svg_linear_gradient(), 0);
}

#[test]
fn svg_radial_gradient() {
    check("svg_radial_gradient", fixture_svg_radial_gradient(), 0);
}

#[test]
fn svg_clip_path() {
    check("svg_clip_path", fixture_svg_clip_path(), 0);
}

#[test]
fn svg_embedded_image() {
    check("svg_embedded_image", fixture_svg_embedded_image(), 0);
}

#[test]
fn svg_mask_fallback() {
    check("svg_mask_fallback", fixture_svg_mask_fallback(), 0);
}

#[test]
fn svg_mask_fallback_gpu_source() {
    check_with_strategy(
        "svg_mask_fallback_gpu_source",
        fixture_svg_mask_fallback(),
        0,
        typst_gpu_canvas::ImageStrategy::Auto,
    );
}

#[test]
fn svg_gpu_source_reuses_texture_across_zoom() {
    let (_, stats) = common::capture_gpu_steps_with_strategy_stats(
        fixture_svg_mask_fallback(),
        &[(egui::Pos2::ZERO, 1.0), (egui::Pos2::ZERO, 2.0)],
        typst_gpu_canvas::ImageStrategy::Gpu,
    );
    assert_eq!(stats.sync_conversions + stats.async_jobs, 1);
    assert_eq!(stats.completed, 1);
    assert_eq!(stats.cache_entries, 1);
}

#[test]
fn svg_radial_object_bbox() {
    check(
        "svg_radial_object_bbox",
        fixture_svg_radial_object_bbox(),
        0,
    );
}

#[test]
fn svg_clip_object_bbox() {
    check("svg_clip_object_bbox", fixture_svg_clip_object_bbox(), 0);
}

#[test]
fn svg_clip_transformed_element() {
    check(
        "svg_clip_transformed_element",
        fixture_svg_clip_transformed_element(),
        0,
    );
}

#[test]
fn svg_nested_clip() {
    check("svg_nested_clip", fixture_svg_nested_clip(), 0);
}

#[test]
fn svg_spread_repeat() {
    check("svg_spread_repeat", fixture_svg_spread_repeat(), 0);
}

#[test]
fn svg_opacity_group() {
    check("svg_opacity_group", fixture_svg_opacity_group(), 0);
}

#[test]
fn svg_opacity_image() {
    check("svg_opacity_image", fixture_svg_opacity_image(), 0);
}

#[test]
fn svg_quad_after_close() {
    check("svg_quad_after_close", fixture_svg_quad_after_close(), 0);
}

#[test]
fn svg_zero_linear() {
    check("svg_zero_linear", fixture_svg_zero_linear(), 0);
}

#[test]
fn svg_zero_radial() {
    check("svg_zero_radial", fixture_svg_zero_radial(), 0);
}

#[test]
fn svg_opacity_fill_stroke() {
    check(
        "svg_opacity_fill_stroke",
        fixture_svg_opacity_fill_stroke(),
        0,
    );
}

#[test]
fn svg_radial_focal_radius() {
    check(
        "svg_radial_focal_radius",
        fixture_svg_radial_focal_radius(),
        0,
    );
}

#[test]
fn stroke_curve() {
    check("stroke_curve", fixture_stroke_curve(), 0);
}

#[test]
fn stroke_dashed() {
    check("stroke_dashed", fixture_stroke_dashed(), 0);
}

#[test]
fn stroke_gradient() {
    // Three edge pixels where the stroke outline meets the gradient's steep
    // region; the same sub-pixel class as `gradient_conic`.
    check("stroke_gradient", fixture_stroke_gradient(), 3);
}

#[test]
fn text_plain() {
    check("text_plain", fixture_text_plain(), 0);
}

#[test]
fn text_small() {
    // At 24 ppem `typst-render` rasterizes glyphs with pixglyph while this
    // renderer tessellates outlines, so the bound is the observed count against
    // a different rasterizer, not a widened threshold.
    check("text_small", fixture_text_small(), 125);
}

#[test]
fn text_cjk() {
    let Some(frame) = fixture_text_cjk() else {
        eprintln!("gpu_canvas_fidelity: no system CJK font found; skipping text_cjk");
        return;
    };
    check("text_cjk", frame, 0);
}

#[test]
fn group_clip_rect() {
    check("group_clip_rect", fixture_group_clip_rect(), 0);
}

#[test]
fn group_clip_curved() {
    check("group_clip_curved", fixture_group_clip_curved(), 0);
}

#[test]
fn nested_clips() {
    check("nested_clips", fixture_nested_clips(), 0);
}

#[test]
fn curved_gradient() {
    check("curved_gradient", fixture_curved_gradient(), 0);
}

/// Replays one recording across a pan and compares the final frame against a
/// fresh recording at the same position.
///
/// Both frames paint at the same scale, so the second frame is a recording-cache
/// hit: the recorded geometry is reused and the GPU transform performs a pure
/// translation. With the integer-aligned [`fixture_solid_rects`] the translated
/// replay is expected to be pixel-identical to a cold recording at the final
/// position, which is what distinguishes a correct transform from a re-record.
#[test]
fn pan_replay_matches_fresh_recording() {
    let frame = fixture_solid_rects();
    let final_pos = egui::pos2(32.0, 0.0);
    let fresh = common::over_white(common::capture_gpu_at(frame.clone(), SCALE, final_pos));
    let replayed = common::over_white(common::capture_gpu_positions(
        frame,
        SCALE,
        &[egui::pos2(0.0, 0.0), final_pos],
    ));
    common::diff_and_assert_at(
        &common::output_dir("gpu-canvas-pan"),
        "pan_replay",
        &fresh,
        &replayed,
        0,
    );
}

/// Replays one recording across an in-bucket zoom and compares the final frame
/// against a fresh recording at the same scale.
///
/// The two steps (scale `s`, then `s * 1.05`) share a scale bucket, so the
/// second frame is a recording-cache hit. Both the hit and a cold recording are
/// made at the bucket center and then transformed by the same ratio, so the
/// final frames are expected to be pixel-identical (bound 0).
#[test]
fn zoom_replay_matches_fresh_recording() {
    let frame = fixture_solid_rects();
    let base = SCALE;
    let zoomed = base * 1.05;
    let origin = egui::pos2(0.0, 0.0);
    let fresh = common::over_white(common::capture_gpu_at(frame.clone(), zoomed, origin));
    let replayed = common::over_white(common::capture_gpu_steps(
        frame,
        &[(origin, base), (origin, zoomed)],
    ));
    common::diff_and_assert_at(
        &common::output_dir("gpu-canvas-zoom"),
        "zoom_replay",
        &fresh,
        &replayed,
        0,
    );
}

/// The local adapter advertises what `GpuCanvas::new` checks — 4x MSAA on the
/// harness color format and a `Stencil8` render attachment. Every other test in
/// this file already requires the same adapter.
#[test]
fn adapter_supports_the_fidelity_render_pass() {
    let render_state = common::harness_render_state();
    let config = typst_gpu_canvas::GpuCanvasConfig {
        msaa_samples: common::MSAA_SAMPLES,
        target_format: render_state.target_format,
        loading_label: "",
        image_strategy: typst_gpu_canvas::ImageStrategy::Cpu,
        image_max_side: 4096,
    };
    assert!(
        typst_gpu_canvas::is_supported(&render_state, &config),
        "the fidelity harness adapter must support 4x MSAA and a Stencil8 attachment"
    );
}
