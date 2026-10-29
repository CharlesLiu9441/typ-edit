//! Vector SVG rendering for the GPU canvas renderer.
//!
//! Instead of rasterizing an SVG, this walker maps usvg nodes onto the crate's
//! existing [`Page`] draw commands (fills, strokes, gradients, stencil clips),
//! so a vectorizable SVG stays resolution-independent and never forces the
//! host's whole-document raster fallback. SVGs that use features we cannot draw
//! faithfully (masks, filters, non-normal blend modes, patterns, non-Pad
//! gradient spread, multi-leaf translucent groups, radial gradients with a
//! degenerate radius or a non-zero focal radius, and nested or non-path clip
//! children) are detected by [`is_vectorizable`] and handled by the per-image
//! resvg raster path instead (see [`super::loader`]).
//!
//! The traversal mirrors vello_svg's `render_group`: usvg's `abs_transform` is
//! root-absolute, so recursion passes the same base transform and every node's
//! transform is `node.abs_transform() * base`.

use typst_library::foundations::Bytes;
use typst_library::layout::{Abs, Ratio, Size};
use typst_library::visualize::{
    DashPattern, ExchangeFormat, FillRule, FixedStroke, Image, ImageKind, LineCap, LineJoin,
    Paint as TypstPaint, RasterImage, SvgImage,
};

use crate::affine::Affine2;
use crate::error::RenderError;
use crate::paint::PaintCtx;
use crate::tess::{self, Path2, TessCtx, TessMesh};

use super::canvas::Page;
use super::gradient::{GradientPlan, SvgStop};
use super::paint::{ImageOptions, paint_image};
use super::resources::CanvasVertex;
use super::traverse::unsupported;

/// tiny-skia's gradient degenerate threshold (`1 / 2^15`): geometry whose
/// magnitude is at most this is treated as zero-sized.
const DEGENERATE_THRESHOLD: f32 = 1.0 / (1 << 15) as f32;

/// Whether a `(dx, dy)` vector is degenerately short by tiny-skia's threshold.
fn is_degenerate_length(dx: f32, dy: f32) -> bool {
    (dx * dx + dy * dy).sqrt() <= DEGENERATE_THRESHOLD
}

/// Whether every node in `tree` can be drawn faithfully as vector content.
///
/// Masks, filters, non-normal blend modes, patterns, non-Pad gradient spread,
/// multi-leaf translucent groups, radial gradients with a degenerate radius or a
/// non-zero focal radius, and clip paths with non-path children all force the
/// per-image raster fallback.
pub(crate) fn is_vectorizable(tree: &usvg::Tree) -> bool {
    group_is_vectorizable(tree.root())
}

/// Whether a group and its subtree are vectorizable.
fn group_is_vectorizable(group: &usvg::Group) -> bool {
    if group.mask().is_some()
        || !group.filters().is_empty()
        || group.blend_mode() != usvg::BlendMode::Normal
    {
        return false;
    }
    if let Some(clip) = group.clip_path()
        && !clip_is_vectorizable(clip)
    {
        return false;
    }
    // resvg composites a translucent group as a layer; distributing its alpha
    // across children only matches for a single paint leaf.
    if group.opacity().get() < 1.0 && count_paint_leaves(group) > 1 {
        return false;
    }
    for node in group.children() {
        match node {
            usvg::Node::Group(child) => {
                if !group_is_vectorizable(child) {
                    return false;
                }
            }
            usvg::Node::Path(path) => {
                if let Some(fill) = path.fill()
                    && !paint_is_vectorizable(fill.paint())
                {
                    return false;
                }
                if let Some(stroke) = path.stroke()
                    && !paint_is_vectorizable(stroke.paint())
                {
                    return false;
                }
            }
            usvg::Node::Image(img) => {
                if let usvg::ImageKind::SVG(nested) = img.kind()
                    && !group_is_vectorizable(nested.root())
                {
                    return false;
                }
            }
            usvg::Node::Text(text) => {
                if !group_is_vectorizable(text.flattened()) {
                    return false;
                }
            }
        }
    }
    true
}

/// Counts the visible paint leaves (fills/strokes, images, flattened text
/// glyphs) in `group`'s subtree.
fn count_paint_leaves(group: &usvg::Group) -> usize {
    let mut count = 0;
    for node in group.children() {
        match node {
            usvg::Node::Group(child) => count += count_paint_leaves(child),
            usvg::Node::Path(path) => {
                if path.is_visible() {
                    // Fill and stroke are separate paint ops, so a translucent
                    // group around a filled-and-stroked path is not a single
                    // composited leaf.
                    count += path.fill().is_some() as usize + path.stroke().is_some() as usize;
                }
            }
            usvg::Node::Image(image) => {
                if image.is_visible() {
                    count += 1;
                }
            }
            usvg::Node::Text(text) => count += count_paint_leaves(text.flattened()),
        }
    }
    count
}

/// Whether a clip path can be drawn as a union of stencil-mask paths.
///
/// A clip path that is itself clipped is rejected: the walker only draws the
/// outer union, so a nested `clip-path` would be silently dropped.
fn clip_is_vectorizable(clip: &usvg::ClipPath) -> bool {
    if clip.clip_path().is_some() {
        return false;
    }
    clip.root()
        .children()
        .iter()
        .all(|node| matches!(node, usvg::Node::Path(_)))
}

/// Whether a paint is drawable (patterns, non-Pad spread methods, and singular
/// gradient transforms are not).
fn paint_is_vectorizable(paint: &usvg::Paint) -> bool {
    match paint {
        usvg::Paint::Color(_) => true,
        usvg::Paint::Pattern(_) => false,
        usvg::Paint::LinearGradient(gradient) => gradient_is_vectorizable(gradient),
        // A degenerately small radius makes tiny-skia's radial shader `None`
        // (resvg paints nothing), and a non-zero focal radius diverges from our
        // focal-radial formula, so both take the raster fallback.
        usvg::Paint::RadialGradient(gradient) => {
            gradient_is_vectorizable(gradient)
                && gradient.r().get() > DEGENERATE_THRESHOLD
                && gradient.fr().get() <= DEGENERATE_THRESHOLD
        }
    }
}

/// Whether a gradient can be drawn (Pad spread, non-singular transform).
fn gradient_is_vectorizable(gradient: &usvg::BaseGradient) -> bool {
    gradient.spread_method() == usvg::SpreadMethod::Pad
        && Affine2::from_usvg(gradient.transform()).invert().is_some()
}

/// Records a vectorizable SVG's tree as page draw commands.
///
/// `alpha` is the inherited group alpha (1.0 for a top-level image).
pub(crate) fn paint_svg(
    svg: &SvgImage,
    size: Size,
    ctx: &PaintCtx,
    alpha: f32,
    options: ImageOptions,
    page: &mut Page,
) -> Result<(), RenderError> {
    let tree = svg.tree();
    let tree_size = tree.size();
    let (tree_w, tree_h) = (f64::from(tree_size.width()), f64::from(tree_size.height()));
    if !(tree_w.is_finite() && tree_h.is_finite()) || tree_w <= 0.0 || tree_h <= 0.0 {
        return Err(unsupported("svg with zero size"));
    }
    let svg_to_screen = Affine2 {
        sx: size.x.to_pt() / tree_w,
        ky: 0.0,
        kx: 0.0,
        sy: size.y.to_pt() / tree_h,
        tx: 0.0,
        ty: 0.0,
    }
    .then(ctx.to_screen);
    paint_group(
        tree.root(),
        svg_to_screen,
        alpha,
        ctx.pixels_per_point,
        options,
        page,
    )
}

/// Records one group with its accumulated alpha and clip.
fn paint_group(
    group: &usvg::Group,
    base: Affine2,
    alpha: f32,
    ppp: f32,
    options: ImageOptions,
    page: &mut Page,
) -> Result<(), RenderError> {
    let element = Affine2::from_usvg(group.abs_transform()).then(base);
    let group_alpha = alpha * group.opacity().get();
    let clipped = match group.clip_path() {
        Some(clip) => match clip_mask(clip, element) {
            Some((vertices, indices)) => {
                page.push_clip(vertices, indices);
                true
            }
            None => false,
        },
        None => false,
    };

    for node in group.children() {
        let transform = Affine2::from_usvg(node.abs_transform()).then(base);
        match node {
            usvg::Node::Group(child) => {
                paint_group(child, base, group_alpha, ppp, options, page)?;
            }
            usvg::Node::Path(path) => {
                paint_path(path, transform, group_alpha, page)?;
            }
            usvg::Node::Image(image) => {
                paint_svg_image(image, transform, group_alpha, ppp, options, page)?;
            }
            usvg::Node::Text(text) => {
                // `flattened` children carry transforms relative to the text,
                // so the text node's transform is the new base (as in vello_svg).
                paint_group(text.flattened(), transform, group_alpha, ppp, options, page)?;
            }
        }
    }

    if clipped {
        page.pop_clip();
    }
    Ok(())
}

/// Records one path's fill and stroke in SVG paint order.
fn paint_path(
    path: &usvg::Path,
    transform: Affine2,
    alpha: f32,
    page: &mut Page,
) -> Result<(), RenderError> {
    if !path.is_visible() {
        return Ok(());
    }
    let path2 = Path2::from_tiny_skia(path.data());
    let tess_ctx = TessCtx::new(transform);
    let fill = |page: &mut Page| match path.fill() {
        Some(fill) => {
            let rule = match fill.rule() {
                usvg::FillRule::NonZero => FillRule::NonZero,
                usvg::FillRule::EvenOdd => FillRule::EvenOdd,
            };
            let mut mesh = TessMesh::default();
            tess::fill(&path2, rule, &tess_ctx, false, &mut mesh);
            push_svg_paint(&mesh, fill.paint(), fill.opacity().get(), alpha, page)
        }
        None => Ok(()),
    };
    let stroke = |page: &mut Page| match path.stroke() {
        Some(stroke) => {
            let mut mesh = TessMesh::default();
            tess::stroke_with(&path2, &to_typst_stroke(stroke), &tess_ctx, None, &mut mesh);
            push_svg_paint(&mesh, stroke.paint(), stroke.opacity().get(), alpha, page)
        }
        None => Ok(()),
    };
    match path.paint_order() {
        usvg::PaintOrder::FillAndStroke => {
            fill(page)?;
            stroke(page)?;
        }
        usvg::PaintOrder::StrokeAndFill => {
            stroke(page)?;
            fill(page)?;
        }
    }
    Ok(())
}

/// Builds a typst stroke from a usvg stroke; the paint is filled in separately.
///
/// `stroke_with` ignores `FixedStroke::paint`, so a placeholder color is used
/// and the actual usvg paint is pushed afterwards through [`push_svg_paint`].
fn to_typst_stroke(stroke: &usvg::Stroke) -> FixedStroke {
    let cap = match stroke.linecap() {
        usvg::LineCap::Butt => LineCap::Butt,
        usvg::LineCap::Round => LineCap::Round,
        usvg::LineCap::Square => LineCap::Square,
    };
    let join = match stroke.linejoin() {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
        usvg::LineJoin::Round => LineJoin::Round,
        usvg::LineJoin::Bevel => LineJoin::Bevel,
    };
    let dash = stroke.dasharray().map(|array| DashPattern {
        array: array
            .iter()
            .map(|&length| Abs::pt(f64::from(length)))
            .collect(),
        phase: Abs::pt(f64::from(stroke.dashoffset())),
    });
    FixedStroke {
        paint: TypstPaint::Solid(typst_library::visualize::Color::BLACK),
        thickness: Abs::pt(f64::from(stroke.width().get())),
        cap,
        join,
        dash,
        miter_limit: Ratio::new(f64::from(stroke.miterlimit().get())),
    }
}

/// Records a tessellated mesh with a usvg paint, folding in opacity and alpha.
///
/// Gradient geometry stays in gradient space; the inverse of usvg's gradient
/// transform (which folds in `objectBoundingBox` and `gradientTransform`) is
/// passed as `from_local`, so scaled/skewed gradients render exactly. A
/// near-zero-length linear gradient paints its last stop (tiny-skia's Pad
/// behavior); a near-zero-radius radial gradient is rejected by
/// [`is_vectorizable`] before reaching here.
fn push_svg_paint(
    mesh: &TessMesh,
    paint: &usvg::Paint,
    opacity: f32,
    alpha: f32,
    page: &mut Page,
) -> Result<(), RenderError> {
    if mesh.is_empty() {
        return Ok(());
    }
    match paint {
        usvg::Paint::Color(color) => {
            let a = (opacity * alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            let color =
                egui::Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, a);
            page.push_solid(mesh, color);
            Ok(())
        }
        usvg::Paint::LinearGradient(gradient) => {
            let dx = gradient.x2() - gradient.x1();
            let dy = gradient.y2() - gradient.y1();
            if is_degenerate_length(dx, dy) {
                // tiny-skia paints a Pad linear gradient's last stop when its
                // endpoints nearly coincide.
                page.push_solid(mesh, last_stop_color(gradient, opacity, alpha));
                return Ok(());
            }
            let from_local = gradient_inverse(gradient)?;
            let stops = svg_stops(gradient, opacity, alpha);
            let p0 = (gradient.x1(), gradient.y1());
            let p1 = (gradient.x2(), gradient.y2());
            page.push_gradient(mesh, GradientPlan::svg_linear(from_local, p0, p1, &stops));
            Ok(())
        }
        usvg::Paint::RadialGradient(gradient) => {
            let radius = gradient.r().get();
            if radius <= 0.0 {
                // The scan rejects degenerately small radii; this is a backstop
                // so a wrong color is never painted.
                return Err(unsupported("svg zero-radius radial"));
            }
            let from_local = gradient_inverse(gradient)?;
            let stops = svg_stops(gradient, opacity, alpha);
            let center = (gradient.cx(), gradient.cy());
            let focal = (gradient.fx(), gradient.fy());
            let focal_radius = gradient.fr().get();
            page.push_gradient(
                mesh,
                GradientPlan::svg_radial(from_local, center, focal, radius, focal_radius, &stops),
            );
            Ok(())
        }
        usvg::Paint::Pattern(_) => Err(unsupported("svg pattern")),
    }
}

/// The local-to-gradient inverse of a usvg gradient transform.
///
/// [`is_vectorizable`] rejects singular gradient transforms, so this only fails
/// if the walker is reached in violation of the scan.
fn gradient_inverse(gradient: &usvg::BaseGradient) -> Result<Affine2, RenderError> {
    Affine2::from_usvg(gradient.transform())
        .invert()
        .ok_or_else(|| unsupported("svg gradient transform"))
}

/// The effective color of a gradient's last stop, for degenerate gradients.
fn last_stop_color(gradient: &usvg::BaseGradient, opacity: f32, alpha: f32) -> egui::Color32 {
    match gradient.stops().last() {
        Some(stop) => {
            let color = stop.color();
            let a = (stop.opacity().get() * opacity * alpha * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            egui::Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, a)
        }
        None => egui::Color32::TRANSPARENT,
    }
}

/// Resolves a usvg gradient's stops with the effective alpha folded in.
fn svg_stops(gradient: &usvg::BaseGradient, opacity: f32, alpha: f32) -> Vec<SvgStop> {
    gradient
        .stops()
        .iter()
        .map(|stop| {
            let color = stop.color();
            SvgStop {
                offset: stop.offset().get(),
                color: [color.red, color.green, color.blue],
                alpha: stop.opacity().get() * opacity * alpha,
            }
        })
        .collect()
}

/// Records a usvg image node: embedded rasters go through [`paint_image`], and
/// nested SVG trees recurse into the walker.
fn paint_svg_image(
    image: &usvg::Image,
    transform: Affine2,
    alpha: f32,
    ppp: f32,
    options: ImageOptions,
    page: &mut Page,
) -> Result<(), RenderError> {
    if !image.is_visible() {
        return Ok(());
    }
    let size = image.size();
    let (width_pt, height_pt) = (f64::from(size.width()), f64::from(size.height()));
    match image.kind() {
        usvg::ImageKind::PNG(data)
        | usvg::ImageKind::JPEG(data)
        | usvg::ImageKind::GIF(data)
        | usvg::ImageKind::WEBP(data) => {
            let format = match image.kind() {
                usvg::ImageKind::JPEG(_) => ExchangeFormat::Jpg,
                usvg::ImageKind::PNG(_) => ExchangeFormat::Png,
                usvg::ImageKind::GIF(_) => ExchangeFormat::Gif,
                _ => ExchangeFormat::Webp,
            };
            let raster = RasterImage::plain(Bytes::new(data.to_vec()), format)
                .map_err(|_| unsupported("svg embedded image"))?;
            let image = Image::plain(ImageKind::Raster(raster));
            let size = Size::new(Abs::pt(width_pt), Abs::pt(height_pt));
            let ctx = PaintCtx {
                to_screen: transform,
                container_to_screen: transform,
                container_size: (width_pt, height_pt),
                pixels_per_point: ppp,
                on_text: false,
            };
            paint_image(&image, size, &ctx, alpha, options, page)
        }
        usvg::ImageKind::SVG(tree) => {
            let nested = tree.size();
            let (nested_w, nested_h) = (f64::from(nested.width()), f64::from(nested.height()));
            let base = if nested_w > 0.0 && nested_h > 0.0 {
                Affine2 {
                    sx: width_pt / nested_w,
                    ky: 0.0,
                    kx: 0.0,
                    sy: height_pt / nested_h,
                    tx: 0.0,
                    ty: 0.0,
                }
                .then(transform)
            } else {
                transform
            };
            paint_group(tree.root(), base, alpha, ppp, options, page)
        }
    }
}

/// Tessellates an SVG clip path into stencil-mask canvas vertices.
///
/// All children are paths (guaranteed by [`is_vectorizable`]); each child is
/// mapped to screen space through its own transform, then the clip path's own
/// `transform()` (which includes `clipPathUnits="objectBoundingBox"`'s bbox
/// transform), then the clipped element's transform. The concatenated union is
/// filled with the NonZero rule; since SVG's clip union assumes identically
/// wound children, a shape wound the other way would cancel its overlap instead
/// of unioning.
fn clip_mask(clip: &usvg::ClipPath, element: Affine2) -> Option<(Vec<CanvasVertex>, Vec<u32>)> {
    let clip_transform = Affine2::from_usvg(clip.transform());
    let mut path = Path2::default();
    for child in clip.root().children() {
        let usvg::Node::Path(child) = child else {
            continue;
        };
        if !child.is_visible() {
            continue;
        }
        let transform = Affine2::from_usvg(child.abs_transform())
            .then(clip_transform)
            .then(element);
        path.0
            .extend(Path2::from_tiny_skia(child.data()).map(&transform).0);
    }
    if path.is_empty() {
        return None;
    }
    let mut mesh = TessMesh::default();
    tess::fill(
        &path,
        FillRule::NonZero,
        &TessCtx::new(Affine2::IDENTITY),
        false,
        &mut mesh,
    );
    if mesh.vertices.is_empty() || mesh.indices.is_empty() {
        return None;
    }
    let vertices = mesh
        .vertices
        .iter()
        .map(|vertex| CanvasVertex {
            pos: [vertex.pos.x, vertex.pos.y],
            local: [vertex.local.x, vertex.local.y],
            color: egui::Color32::WHITE.to_array(),
        })
        .collect();
    Some((vertices, mesh.indices.clone()))
}
