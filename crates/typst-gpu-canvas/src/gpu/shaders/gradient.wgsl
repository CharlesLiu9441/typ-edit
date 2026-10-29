// Gradient fills: the vertex shader passes gradient-space coordinates through,
// the fragment shader computes the gradient parameter t with the exact
// formulas of typst 0.15.1's `Gradient::sample_at`
// (crates/typst-library/src/visualize/gradient.rs, Apache-2.0/MIT) and looks
// the color up in a CPU-baked 1D LUT.

struct Screen {
    /// xy = screen size in physical pixels, z = pixels per point.
    size: vec4<f32>,
};

// Four scalars keep the struct exactly 16 bytes (a `vec2`'s 8-byte alignment
// would make it 24 and mismatch the uniform buffer).
struct Transform {
    /// Uniform zoom ratio between the live scale and the recording's scale.
    ratio: f32,
    /// Horizontal pan offset in screen points.
    offset_x: f32,
    /// Vertical pan offset in screen points.
    offset_y: f32,
    /// Padding to the 16-byte uniform size.
    pad: f32,
};

struct GradientParams {
    box_min: vec2<f32>,
    box_size: vec2<f32>,
    kind: u32,
    aspect: f32,
    lin_scale: vec2<f32>,
    lin_offset: f32,
    lin_factor: f32,
    center: vec2<f32>,
    focal: vec2<f32>,
    radius: f32,
    focal_radius: f32,
    conic_angle: f32,
    pad0: f32,
    corner_offset: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Screen;
@group(0) @binding(1) var<uniform> xf: Transform;
@group(1) @binding(0) var<uniform> p: GradientParams;
@group(1) @binding(1) var lut: texture_2d<f32>;
@group(1) @binding(2) var lut_sampler: sampler;

const PI: f32 = 3.141592653589793;
const TAU: f32 = 6.283185307179586;

struct VertexIn {
    @location(0) pos: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let sp = (in.pos * xf.ratio + vec2<f32>(xf.offset_x, xf.offset_y)) * u.size.z;
    let ndc = vec2<f32>(
        (sp.x / max(u.size.x, 1.0)) * 2.0 - 1.0,
        1.0 - (sp.y / max(u.size.y, 1.0)) * 2.0,
    );
    var out: VertexOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.local = in.local;
    out.color = in.color;
    return out;
}

fn gradient_t(nrm: vec2<f32>) -> f32 {
    if (p.kind == 0u) {
        return (dot(nrm, p.lin_scale) + p.lin_offset) / p.lin_factor;
    }
    if (p.kind == 1u) {
        let d = length(nrm - p.focal);
        if (d < p.focal_radius) {
            return 0.0;
        }
        if (length(nrm - p.center) > p.radius) {
            return 1.0;
        }
        let v = nrm - p.focal;
        let uz = v / max(d, 1e-6);
        let az = dot(p.focal - p.center, uz);
        let delta = p.center - p.focal;
        let rho = p.radius * p.radius - dot(delta, delta);
        let bz = sqrt(max(az * az + rho, 0.0)) - az;
        return (d - p.focal_radius) / (bz - p.focal_radius);
    }
    let d = nrm - p.center;
    let sample_angle = atan2(d.y / p.aspect, d.x);
    var a = PI + sample_angle - p.conic_angle;
    a = a - TAU * floor(a / TAU);
    return a / TAU;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let nrm = (in.local - p.corner_offset - p.box_min) / max(p.box_size, vec2<f32>(1e-6));
    let t = clamp(gradient_t(nrm), 0.0, 1.0);
    let width = f32(textureDimensions(lut, 0).x);
    let uv = vec2<f32>((t * (width - 1.0) + 0.5) / width, 0.5);
    return in.color * textureSample(lut, lut_sampler, uv);
}
