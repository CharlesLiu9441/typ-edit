@group(0) @binding(0) var<storage, read_write> pixels: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= arrayLength(&pixels)) {
        return;
    }
    let packed = pixels[index];
    let r = packed & 0xffu;
    let g = (packed >> 8u) & 0xffu;
    let b = (packed >> 16u) & 0xffu;
    let a = (packed >> 24u) & 0xffu;
    let rp = mul_frac_round(r, a);
    let gp = mul_frac_round(g, a);
    let bp = mul_frac_round(b, a);
    pixels[index] = rp | (gp << 8u) | (bp << 16u) | (a << 24u);
}

fn mul_frac_round(value: u32, alpha: u32) -> u32 {
    let p = value * alpha + 128u;
    return (p + (p >> 8u)) >> 8u;
}
