// Image quads: the vertex shader maps the recorded position through the
// transform and passes UVs (`local`) through, the fragment shader multiplies
// the uploaded premultiplied sRGB texture by the vertex color, matching egui's
// gamma-space fragment path.

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

@group(0) @binding(0) var<uniform> u: Screen;
@group(0) @binding(1) var<uniform> xf: Transform;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct VertexIn {
    @location(0) pos: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
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
    out.uv = in.local;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color * textureSample(image, image_sampler, in.uv);
}
