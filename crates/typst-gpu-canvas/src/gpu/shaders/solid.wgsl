// Vertex positions are logical screen points from the recording; the transform
// uniform maps them onto the live page position and zoom, then the screen
// uniform converts them to physical pixels and NDC (the convention of the
// renderer this was extracted from). Vertex colors are premultiplied sRGB (the
// epaint convention), written directly to the gamma-space target like egui's
// own `fs_main_gamma_framebuffer`.

struct Screen {
    /// xy = screen size in physical pixels, z = pixels per point.
    size: vec4<f32>,
};

// `ratio`/`offset_x`/`offset_y` are four scalars rather than a `vec2<f32>` so
// the struct stays exactly 16 bytes: a `vec2`'s 8-byte alignment would push it
// to 24 and mismatch the uniform buffer.
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

struct VertexIn {
    @location(0) pos: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let p = (in.pos * xf.ratio + vec2<f32>(xf.offset_x, xf.offset_y)) * u.size.z;
    let ndc = vec2<f32>(
        (p.x / max(u.size.x, 1.0)) * 2.0 - 1.0,
        1.0 - (p.y / max(u.size.y, 1.0)) * 2.0,
    );
    var out: VertexOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
