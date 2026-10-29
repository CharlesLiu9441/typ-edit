//! Page rasterization request and finished raster.

/// Request: rasterize a page (or a region of it) of `generation`.
#[derive(Clone, Debug)]
pub struct RasterRequest {
    pub generation: u64,
    pub page: usize,
    /// Page-space points `(x, y, w, h)`; `None` = whole page.
    pub region: Option<(f32, f32, f32, f32)>,
    pub pixel_per_pt: f32,
}

/// A finished raster: premultiplied RGBA8, row-major, top-to-bottom.
#[derive(Clone, Debug)]
pub struct RasterResult {
    pub generation: u64,
    pub page: usize,
    pub region: (f32, f32, f32, f32),
    pub pixel_per_pt: f32,
    pub size_px: (u32, u32),
    /// Premultiplied RGBA.
    pub rgba: Vec<u8>,
}
