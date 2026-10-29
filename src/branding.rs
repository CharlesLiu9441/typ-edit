//! Application branding: the embedded app icon and its egui textures.
//!
//! The finalized "code + page" artwork ships as straight-alpha RGBA PNGs. Each
//! in-app texture is decoded once per app instance and reused on later frames;
//! the window icon is decoded directly into the viewport builder.

use std::io::Cursor;

/// One embedded app-icon PNG plus the name its texture is registered under.
#[derive(Clone, Copy)]
pub(crate) struct IconPng {
    name: &'static str,
    bytes: &'static [u8],
}

/// 64 px icon, drawn small in the custom title strip.
pub(crate) const ICON_64: IconPng = IconPng {
    name: "app-icon-64",
    bytes: include_bytes!("../assets/app-icon/typ-edit-icon-64.png"),
};

/// 256 px icon, used as the OS window icon and the landing-page hero.
pub(crate) const ICON_256: IconPng = IconPng {
    name: "app-icon-256",
    bytes: include_bytes!("../assets/app-icon/typ-edit-icon-256.png"),
};

/// The 256 px icon in the form [`egui::ViewportBuilder::with_icon`] expects.
pub(crate) fn window_icon() -> egui::IconData {
    let (width, height, rgba) = decode_png(ICON_256.bytes);
    egui::IconData {
        rgba,
        width,
        height,
    }
}

/// Returns the texture for `icon`, decoding and uploading it on first use and
/// reusing the handle cached in `cache` on every later frame.
///
/// The cache is per app instance (a field on the caller), never global: a
/// texture handle is owned by the [`egui::Context`] that created it, and tests
/// construct several contexts in one process.
pub(crate) fn icon_texture(
    ctx: &egui::Context,
    cache: &mut Option<egui::TextureHandle>,
    icon: IconPng,
) -> egui::TextureHandle {
    if let Some(handle) = cache {
        return handle.clone();
    }
    let (width, height, rgba) = decode_png(icon.bytes);
    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
    let handle = ctx.load_texture(icon.name, image, egui::TextureOptions::LINEAR);
    *cache = Some(handle.clone());
    handle
}

/// Decodes `bytes` into `(width, height, straight-alpha RGBA)`, panicking on a
/// malformed stream or anything other than 8-bit RGBA.
fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("app icon: invalid PNG header");
    let size = reader
        .output_buffer_size()
        .expect("app icon: PNG output size overflow");
    let mut buf = vec![0u8; size];
    let frame = reader
        .next_frame(&mut buf)
        .expect("app icon: invalid PNG data");
    assert_eq!(
        frame.color_type,
        png::ColorType::Rgba,
        "app icon must be RGBA8"
    );
    assert_eq!(
        frame.bit_depth,
        png::BitDepth::Eight,
        "app icon must be RGBA8"
    );
    (
        frame.width,
        frame.height,
        buf[..frame.buffer_size()].to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::{ICON_64, ICON_256, decode_png};

    #[test]
    fn embedded_icons_decode_as_rgba8() {
        let (w, h, rgba) = decode_png(ICON_256.bytes);
        assert_eq!((w, h), (256, 256), "256 px icon dimensions");
        assert_eq!(rgba.len(), 256 * 256 * 4, "256 px icon buffer");

        let (w, h, rgba) = decode_png(ICON_64.bytes);
        assert_eq!((w, h), (64, 64), "64 px icon dimensions");
        assert_eq!(rgba.len(), 64 * 64 * 4, "64 px icon buffer");
    }

    #[test]
    #[should_panic(expected = "app icon")]
    fn malformed_png_panics() {
        decode_png(b"not a png");
    }
}
