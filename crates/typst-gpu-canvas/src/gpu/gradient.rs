//! CPU-side gradient parameterization and LUT baking for the GPU canvas.
//!
//! The gradient pipeline computes the gradient parameter `t` in the fragment
//! shader from a vertex's gradient-space position, using the exact formulas of
//! typst 0.15.1's `Gradient::sample_at`
//! (`crates/typst-library/src/visualize/gradient.rs`, Apache-2.0/MIT), and
//! looks the color up in a 1D LUT baked with `Gradient::sample`. Baking with
//! typst's own sampler means every stop interpolation color space (including
//! HSV/CMYK/D65Gray) and focal radial gradients are exact, instead of being
//! re-implemented here.
//!
//! SVG gradients are built directly in gradient space
//! ([`GradientPlan::svg_linear`] / [`GradientPlan::svg_radial`]): `from_local`
//! is the inverse of the usvg gradient transform (which folds in
//! `objectBoundingBox` and `gradientTransform`), the geometry is raw, and the
//! LUT is baked by sRGB-interpolating the usvg stops (SVG's default
//! interpolation), matching resvg.

use std::f64::consts::{FRAC_PI_2, PI};

use typst_library::layout::{Angle, Ratio};
use typst_library::visualize::{Gradient, RatioOrAngle, RelativeTo};

use crate::affine::Affine2;
use crate::bounds::Extents;
use crate::paint::PaintCtx;

/// LUT texels per gradient.
pub(crate) const LUT_SIZE: u32 = 512;

/// Shader kind discriminants.
const KIND_LINEAR: u32 = 0;
const KIND_RADIAL: u32 = 1;
const KIND_CONIC: u32 = 2;

/// Cache key for one gradient's GPU resources.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum GradientKey {
    /// A typst gradient plus its gradient box size in points (which determines
    /// the aspect-ratio correction).
    Typst(Gradient, u64, u64),
    /// An SVG gradient, keyed by its resolved geometry and stops.
    Svg(SvgGradientKey),
}

/// Content key for an SVG gradient: the local-to-gradient transform, the raw
/// gradient-space geometry, and the effective stops.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum SvgGradientKey {
    /// Linear gradient geometry, plus stops.
    Linear {
        /// Local-to-gradient inverse transform, as `f64` bits.
        from_local: [u64; 6],
        /// Start point in gradient space.
        p0: [u32; 2],
        /// End point in gradient space.
        p1: [u32; 2],
        /// Effective stops.
        stops: Vec<SvgStopKey>,
    },
    /// Radial gradient geometry, plus stops.
    Radial {
        /// Local-to-gradient inverse transform, as `f64` bits.
        from_local: [u64; 6],
        /// Main circle center.
        center: [u32; 2],
        /// Focal circle center.
        focal: [u32; 2],
        /// Main circle radius.
        radius: u32,
        /// Focal circle radius.
        focal_radius: u32,
        /// Effective stops.
        stops: Vec<SvgStopKey>,
    },
}

/// Serializes an affine transform to hashable bits.
fn affine_bits(m: Affine2) -> [u64; 6] {
    [
        m.sx.to_bits(),
        m.ky.to_bits(),
        m.kx.to_bits(),
        m.sy.to_bits(),
        m.tx.to_bits(),
        m.ty.to_bits(),
    ]
}

/// One SVG gradient stop as hashable bits.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SvgStopKey {
    offset: u32,
    color: [u8; 3],
    alpha: u32,
}

/// One SVG gradient stop after fill opacity and inherited group alpha are
/// folded into its alpha.
#[derive(Clone, Copy)]
pub(crate) struct SvgStop {
    /// Stop offset in `[0, 1]`.
    pub(crate) offset: f32,
    /// Non-premultiplied sRGB color.
    pub(crate) color: [u8; 3],
    /// Effective alpha (`stop-opacity * fill-opacity * inherited alpha`).
    pub(crate) alpha: f32,
}

/// Everything the GPU needs to draw one gradient mesh.
pub(crate) struct GradientPlan {
    /// Cache key of the plan.
    pub(crate) key: GradientKey,
    /// Maps a mesh vertex's path-space `local` into gradient space.
    pub(crate) from_local: Affine2,
    /// Uniform contents for the gradient pipeline.
    pub(crate) params: GradientParams,
    /// Premultiplied sRGB LUT sampled over `t in [0, 1]`, kept in f32 so the
    /// upload to `Rgba16Float` loses no byte-level rounding.
    pub(crate) lut: Vec<[f32; 4]>,
}

impl GradientPlan {
    /// Builds the plan for `gradient` over the path-space box `bbox`.
    pub(crate) fn new(gradient: &Gradient, bbox: Extents, ctx: &PaintCtx) -> Self {
        let relative = resolve_relative(gradient, ctx.on_text);
        let (size, offset) = geometry(bbox, ctx, relative);
        let from_local = match relative {
            RelativeTo::Self_ => Affine2::translate(-offset.0, -offset.1),
            RelativeTo::Parent => match ctx.container_to_screen.invert() {
                Some(inverse) => ctx.to_screen.then(inverse),
                None => Affine2::translate(-offset.0, -offset.1),
            },
        };
        // typst-render bakes gradients into a device-pixel pixmap evaluated at
        // its pixel indices and samples it with nearest filtering, so a
        // destination pixel shows the gradient at its top-left corner. The
        // vertex `local` carries the gradient position at the fragment center,
        // so shift by half a device pixel converted into gradient space.
        let screen_to_gradient = match ctx.to_screen.invert() {
            Some(inverse) => inverse.then(from_local),
            None => from_local,
        };
        let mut params = params(gradient, size);
        params.corner_offset = half_pixel_offset(&screen_to_gradient, ctx.pixels_per_point);
        let lut = bake(gradient);
        Self {
            key: GradientKey::Typst(gradient.clone(), size.0.to_bits(), size.1.to_bits()),
            from_local,
            params,
            lut,
        }
    }

    /// Builds a plan for an SVG linear gradient.
    ///
    /// `from_local` maps a mesh vertex's path-space `local` into gradient space
    /// (the inverse of usvg's `gradient.transform()`), so `p0`/`p1` are the raw
    /// gradient-space endpoints and any `gradientTransform` or
    /// `objectBoundingBox` mapping is applied by the shader exactly. The
    /// gradient box is the whole gradient space, so `box_size` is `[1, 1]` and
    /// the shader's projection `t = (P - P1) · d / |d|²` uses `p0`/`p1`
    /// directly.
    pub(crate) fn svg_linear(
        from_local: Affine2,
        p0: (f32, f32),
        p1: (f32, f32),
        stops: &[SvgStop],
    ) -> Self {
        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
        let len2 = dx * dx + dy * dy;
        let inv = if len2 > 0.0 { 1.0 / len2 } else { 0.0 };
        let params = GradientParams {
            box_min: [0.0, 0.0],
            box_size: [1.0, 1.0],
            kind: KIND_LINEAR,
            aspect: 1.0,
            lin_scale: [dx * inv, dy * inv],
            lin_offset: -(p0.0 * dx + p0.1 * dy) * inv,
            lin_factor: 1.0,
            center: [0.0, 0.0],
            focal: [0.0, 0.0],
            radius: 0.0,
            focal_radius: 0.0,
            conic_angle: 0.0,
            pad: 0.0,
            corner_offset: [0.0, 0.0],
        };
        let key = GradientKey::Svg(SvgGradientKey::Linear {
            from_local: affine_bits(from_local),
            p0: [p0.0.to_bits(), p0.1.to_bits()],
            p1: [p1.0.to_bits(), p1.1.to_bits()],
            stops: stop_keys(stops),
        });
        Self {
            key,
            from_local,
            params,
            lut: svg_lut(stops),
        }
    }

    /// Builds a plan for an SVG radial gradient.
    ///
    /// `from_local` is the inverse of usvg's `gradient.transform()` (see
    /// [`Self::svg_linear`]); `center`/`focal` and the radii are raw
    /// gradient-space values, so focal radii and non-uniform
    /// `objectBoundingBox` scaling become exact ellipses.
    pub(crate) fn svg_radial(
        from_local: Affine2,
        center: (f32, f32),
        focal: (f32, f32),
        radius: f32,
        focal_radius: f32,
        stops: &[SvgStop],
    ) -> Self {
        let params = GradientParams {
            box_min: [0.0, 0.0],
            box_size: [1.0, 1.0],
            kind: KIND_RADIAL,
            aspect: 1.0,
            lin_scale: [0.0, 0.0],
            lin_offset: 0.0,
            lin_factor: 1.0,
            center: [center.0, center.1],
            focal: [focal.0, focal.1],
            radius,
            focal_radius,
            conic_angle: 0.0,
            pad: 0.0,
            corner_offset: [0.0, 0.0],
        };
        let key = GradientKey::Svg(SvgGradientKey::Radial {
            from_local: affine_bits(from_local),
            center: [center.0.to_bits(), center.1.to_bits()],
            focal: [focal.0.to_bits(), focal.1.to_bits()],
            radius: radius.to_bits(),
            focal_radius: focal_radius.to_bits(),
            stops: stop_keys(stops),
        });
        Self {
            key,
            from_local,
            params,
            lut: svg_lut(stops),
        }
    }
}

/// Builds the hashable bit form of each SVG stop.
fn stop_keys(stops: &[SvgStop]) -> Vec<SvgStopKey> {
    stops
        .iter()
        .map(|stop| SvgStopKey {
            offset: stop.offset.to_bits(),
            color: stop.color,
            alpha: stop.alpha.to_bits(),
        })
        .collect()
}

/// Bakes the premultiplied sRGB LUT for an SVG gradient by interpolating its
/// stops in sRGB space (the SVG default), matching resvg's `tiny-skia` output.
fn svg_lut(stops: &[SvgStop]) -> Vec<[f32; 4]> {
    (0..LUT_SIZE)
        .map(|i| {
            let t = i as f32 / (LUT_SIZE - 1) as f32;
            sample_svg_stops(stops, t)
        })
        .collect()
}

/// Samples an SVG gradient's stops at `t`, premultiplying the result.
fn sample_svg_stops(stops: &[SvgStop], t: f32) -> [f32; 4] {
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };
    let last = stops.last().unwrap_or(first);
    if t <= first.offset {
        return premultiplied(*first);
    }
    if t >= last.offset {
        return premultiplied(*last);
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t >= a.offset && t <= b.offset {
            let span = b.offset - a.offset;
            let f = if span > 0.0 {
                (t - a.offset) / span
            } else {
                0.0
            };
            let channel = |i: usize| {
                let c0 = a.color[i] as f32 / 255.0;
                let c1 = b.color[i] as f32 / 255.0;
                c0 + (c1 - c0) * f
            };
            let alpha = a.alpha + (b.alpha - a.alpha) * f;
            return [
                channel(0) * alpha,
                channel(1) * alpha,
                channel(2) * alpha,
                alpha,
            ];
        }
    }
    premultiplied(*last)
}

/// Converts one SVG stop to premultiplied sRGB floats.
fn premultiplied(stop: SvgStop) -> [f32; 4] {
    let channel = |i: usize| stop.color[i] as f32 / 255.0;
    let a = stop.alpha;
    [channel(0) * a, channel(1) * a, channel(2) * a, a]
}

/// The uniform block consumed by `shaders/gradient.wgsl`.
///
/// Field order and padding match the WGSL struct exactly (80 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct GradientParams {
    /// Gradient-space origin (zero; `from_local` already normalized it).
    pub(crate) box_min: [f32; 2],
    /// Gradient box size in points.
    pub(crate) box_size: [f32; 2],
    /// [`KIND_LINEAR`] / [`KIND_RADIAL`] / [`KIND_CONIC`].
    pub(crate) kind: u32,
    /// Gradient box aspect ratio (`width / height`).
    pub(crate) aspect: f32,
    /// Linear: per-axis `t` weights (`+/-|cos|`, `+/-|sin|`).
    pub(crate) lin_scale: [f32; 2],
    /// Linear: constant added to the projection.
    pub(crate) lin_offset: f32,
    /// Linear: `|sin| + |cos|` normalizer.
    pub(crate) lin_factor: f32,
    /// Radial: main circle center (ratios).
    pub(crate) center: [f32; 2],
    /// Radial: focal circle center (ratios).
    pub(crate) focal: [f32; 2],
    /// Radial: main circle radius (ratio).
    pub(crate) radius: f32,
    /// Radial: focal circle radius (ratio).
    pub(crate) focal_radius: f32,
    /// Conic: rotation angle in radians.
    pub(crate) conic_angle: f32,
    /// Padding to the WGSL struct alignment.
    pub(crate) pad: f32,
    /// Half a device pixel in gradient space (corner sampling, see
    /// [`GradientPlan::new`]).
    pub(crate) corner_offset: [f32; 2],
}

impl GradientParams {
    /// Serializes the uniform to little-endian bytes.
    ///
    /// `kind` is a `u32` in the WGSL struct, so it is written as a bit pattern
    /// rather than through the float channel conversion.
    pub(crate) fn to_bytes(self) -> [u8; 80] {
        let floats = [
            self.box_min[0],
            self.box_min[1],
            self.box_size[0],
            self.box_size[1],
            self.aspect,
            self.lin_scale[0],
            self.lin_scale[1],
            self.lin_offset,
            self.lin_factor,
            self.center[0],
            self.center[1],
            self.focal[0],
            self.focal[1],
            self.radius,
            self.focal_radius,
            self.conic_angle,
            self.pad,
            self.corner_offset[0],
            self.corner_offset[1],
        ];
        let mut out = [0_u8; 80];
        let mut float_index = 0;
        for (word, slot) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let bits = if word == 4 {
                self.kind
            } else {
                let value = floats[float_index];
                float_index += 1;
                value.to_bits()
            };
            slot.copy_from_slice(&bits.to_le_bytes());
        }
        out
    }
}

/// Half a device pixel expressed in gradient space, for
/// [`GradientParams::corner_offset`].
fn half_pixel_offset(screen_to_gradient: &Affine2, pixels_per_point: f32) -> [f32; 2] {
    let origin = screen_to_gradient.map(0.0, 0.0);
    let x_axis = screen_to_gradient.map(1.0, 0.0);
    let y_axis = screen_to_gradient.map(0.0, 1.0);
    let dx = (x_axis.0 - origin.0) + (y_axis.0 - origin.0);
    let dy = (x_axis.1 - origin.1) + (y_axis.1 - origin.1);
    let scale = 0.5 / f64::from(pixels_per_point.max(0.1));
    [(dx * scale) as f32, (dy * scale) as f32]
}

/// Resolves the gradient's coordinate system, forcing text gradients to parent
/// (mirrors the CPU gradient path).
fn resolve_relative(gradient: &Gradient, on_text: bool) -> RelativeTo {
    let relative = gradient.unwrap_relative(on_text);
    if on_text && relative == RelativeTo::Self_ {
        RelativeTo::Parent
    } else {
        relative
    }
}

/// Returns the gradient box size and origin (mirrors the CPU gradient path).
fn geometry(bbox: Extents, ctx: &PaintCtx, relative: RelativeTo) -> ((f64, f64), (f64, f64)) {
    let (size, offset) = match relative {
        RelativeTo::Self_ => ((bbox.x1 - bbox.x0, bbox.y1 - bbox.y0), (bbox.x0, bbox.y0)),
        RelativeTo::Parent => (ctx.container_size, (0.0, 0.0)),
    };
    ((size.0.abs().max(1.0), size.1.abs().max(1.0)), offset)
}

/// Builds the shader parameters for `gradient` (typst `sample_at` formulas).
fn params(gradient: &Gradient, size: (f64, f64)) -> GradientParams {
    let aspect = (size.0 as f32) / (size.1 as f32);
    let mut out = GradientParams {
        box_min: [0.0, 0.0],
        box_size: [size.0 as f32, size.1 as f32],
        kind: KIND_LINEAR,
        aspect,
        lin_scale: [0.0, 0.0],
        lin_offset: 0.0,
        lin_factor: 1.0,
        center: [0.0, 0.0],
        focal: [0.0, 0.0],
        radius: 0.0,
        focal_radius: 0.0,
        conic_angle: 0.0,
        pad: 0.0,
        corner_offset: [0.0, 0.0],
    };
    match gradient {
        Gradient::Linear(linear) => {
            let angle = correct_aspect_ratio(linear.angle, Ratio::new(f64::from(aspect)));
            let (sin, cos) = (angle.sin(), angle.cos());
            let rad = angle.to_rad();
            let flip_x = rad > FRAC_PI_2 && rad < 3.0 * FRAC_PI_2;
            let flip_y = rad > PI;
            let (sx, ox) = if flip_x {
                (-cos.abs(), cos.abs())
            } else {
                (cos.abs(), 0.0)
            };
            let (sy, oy) = if flip_y {
                (-sin.abs(), sin.abs())
            } else {
                (sin.abs(), 0.0)
            };
            out.kind = KIND_LINEAR;
            out.lin_scale = [sx as f32, sy as f32];
            out.lin_offset = (ox + oy) as f32;
            out.lin_factor = (sin.abs() + cos.abs()) as f32;
        }
        Gradient::Radial(radial) => {
            out.kind = KIND_RADIAL;
            out.center = [radial.center.x.get() as f32, radial.center.y.get() as f32];
            out.focal = [
                radial.focal_center.x.get() as f32,
                radial.focal_center.y.get() as f32,
            ];
            out.radius = radial.radius.get() as f32;
            out.focal_radius = radial.focal_radius.get() as f32;
        }
        Gradient::Conic(conic) => {
            out.kind = KIND_CONIC;
            out.center = [conic.center.x.get() as f32, conic.center.y.get() as f32];
            out.conic_angle = conic.angle.to_rad() as f32;
        }
    }
    out
}

/// typst's `Gradient::correct_aspect_ratio`.
fn correct_aspect_ratio(angle: Angle, aspect: Ratio) -> Angle {
    Angle::atan2(angle.sin() / aspect.get().abs(), angle.cos()).normalized()
}

/// Bakes the premultiplied sRGB LUT with typst's own stop sampler.
fn bake(gradient: &Gradient) -> Vec<[f32; 4]> {
    (0..LUT_SIZE)
        .map(|i| {
            let t = f64::from(i) / f64::from(LUT_SIZE - 1);
            color_f32(&gradient.sample(RatioOrAngle::Ratio(Ratio::new(t))))
        })
        .collect()
}

/// Converts a typst color into premultiplied sRGB floats in `[0, 1]`
/// (`color32` without the byte rounding).
fn color_f32(color: &typst_library::visualize::Color) -> [f32; 4] {
    let (r, g, b, a) = color.to_process().to_rgb().into_components();
    [r * a, g * a, b * a, a]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use typst_library::foundations::Smart;
    use typst_library::layout::{Angle, Axes, Ratio};
    use typst_library::visualize::{
        Color, ColorSpace, ConicGradient, LinearGradient, ProcessColorSpace, RadialGradient,
    };

    use crate::paint::color32;

    /// Mirrors the `gradient_t` function of `shaders/gradient.wgsl`.
    fn shader_t(params: &GradientParams, local: (f32, f32)) -> f64 {
        let nrm = (
            (local.0 - params.corner_offset[0] - params.box_min[0]) / params.box_size[0].max(1e-6),
            (local.1 - params.corner_offset[1] - params.box_min[1]) / params.box_size[1].max(1e-6),
        );
        let t = match params.kind {
            KIND_LINEAR => {
                (f64::from(nrm.0) * f64::from(params.lin_scale[0])
                    + f64::from(nrm.1) * f64::from(params.lin_scale[1])
                    + f64::from(params.lin_offset))
                    / f64::from(params.lin_factor)
            }
            KIND_RADIAL => {
                let (zx, zy) = (f64::from(nrm.0), f64::from(nrm.1));
                let (px, py) = (f64::from(params.center[0]), f64::from(params.center[1]));
                let (qx, qy) = (f64::from(params.focal[0]), f64::from(params.focal[1]));
                let cr = f64::from(params.radius);
                let fr = f64::from(params.focal_radius);
                let d = ((zx - qx).powi(2) + (zy - qy).powi(2)).sqrt();
                if d < fr {
                    0.0
                } else if ((zx - px).powi(2) + (zy - py).powi(2)).sqrt() > cr {
                    1.0
                } else {
                    let (ux, uy) = ((zx - qx) / d.max(1e-6), (zy - qy) / d.max(1e-6));
                    let az = (qx - px) * ux + (qy - py) * uy;
                    let rho = cr * cr - ((qx - px).powi(2) + (qy - py).powi(2));
                    let bz = (az * az + rho).max(0.0).sqrt() - az;
                    (d - fr) / (bz - fr)
                }
            }
            _ => {
                let (dx, dy) = (
                    f64::from(nrm.0) - f64::from(params.center[0]),
                    f64::from(nrm.1) - f64::from(params.center[1]),
                );
                let sample_angle = (dy / f64::from(params.aspect)).atan2(dx);
                let a = (std::f64::consts::PI + sample_angle - f64::from(params.conic_angle))
                    .rem_euclid(std::f64::consts::TAU);
                a / std::f64::consts::TAU
            }
        };
        t.clamp(0.0, 1.0)
    }

    /// Samples the baked LUT the way the shader's linear filtering does.
    fn lut_sample(lut: &[[f32; 4]], t: f64) -> (f32, f32, f32) {
        let pos = t * (LUT_SIZE as f64 - 1.0);
        let i0 = pos.floor().clamp(0.0, (LUT_SIZE - 1) as f64) as usize;
        let i1 = (i0 + 1).min(LUT_SIZE as usize - 1);
        let frac = (pos - i0 as f64) as f32;
        let a = lut[i0];
        let b = lut[i1];
        let mix = |i: usize| a[i] * (1.0 - frac) + b[i] * frac;
        (mix(0), mix(1), mix(2))
    }

    fn probe(gradient: &Gradient, size: (f64, f64)) -> f64 {
        let bbox = Extents {
            x0: 0.0,
            y0: 0.0,
            x1: size.0,
            y1: size.1,
        };
        let ctx = PaintCtx {
            to_screen: Affine2::IDENTITY,
            container_to_screen: Affine2::IDENTITY,
            container_size: size,
            pixels_per_point: 1.0,
            on_text: false,
        };
        let plan = GradientPlan::new(gradient, bbox, &ctx);
        // The corner offset is a sampling-convention shift, not part of the
        // parameterization, so zero it for the formula comparison.
        let mut params = plan.params;
        params.corner_offset = [0.0, 0.0];
        let mut max_error = 0.0_f64;
        for j in 0..=16 {
            for i in 0..=16 {
                let x = size.0 * f64::from(i) / 16.0;
                let y = size.1 * f64::from(j) / 16.0;
                let (lx, ly) = plan.from_local.map(x, y);
                let t = shader_t(&params, (lx as f32, ly as f32));
                let (r, g, b) = lut_sample(&plan.lut, t);
                let expected = color32(
                    &gradient.sample_at((x as f32, y as f32), (size.0 as f32, size.1 as f32)),
                );
                let error = (r - f32::from(expected.r()) / 255.0)
                    .abs()
                    .max((g - f32::from(expected.g()) / 255.0).abs())
                    .max((b - f32::from(expected.b()) / 255.0).abs());
                max_error = max_error.max(f64::from(error));
            }
        }
        max_error
    }

    fn linear(angle: f64, space: ProcessColorSpace) -> Gradient {
        Gradient::Linear(Arc::new(LinearGradient {
            stops: vec![
                (Color::RED, Ratio::zero()),
                (Color::GREEN, Ratio::new(0.5)),
                (Color::BLUE, Ratio::one()),
            ],
            angle: Angle::rad(angle),
            space: ColorSpace::Process(space),
            relative: Smart::Auto,
            anti_alias: true,
        }))
    }

    #[test]
    fn shader_linear_matches_sample_at() {
        for (angle, space) in [
            (0.0, ProcessColorSpace::Srgb),
            (0.6, ProcessColorSpace::Srgb),
            (2.5, ProcessColorSpace::Srgb),
            (4.0, ProcessColorSpace::Srgb),
            (0.2, ProcessColorSpace::Hsv),
            (1.0, ProcessColorSpace::Cmyk),
        ] {
            let error = probe(&linear(angle, space), (112.0, 100.0));
            assert!(error < 0.02, "angle {angle}: max error {error}");
        }
    }

    #[test]
    fn shader_radial_matches_sample_at() {
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
        let error = probe(&gradient, (108.0, 108.0));
        assert!(error < 0.02, "max error {error}");
    }

    #[test]
    fn shader_conic_matches_sample_at() {
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
        let error = probe(&gradient, (112.0, 112.0));
        assert!(error < 0.02, "max error {error}");
    }
}
