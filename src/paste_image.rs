//! PNG encoding for pasted clipboard bitmaps.
//!
//! The editor only reads the clipboard bitmap
//! (`typ_edit_editor::clipboard_image`); the application owns encoding it,
//! because the application is what writes the file.

use typ_edit_editor::clipboard_image::ClipboardImage;

/// Encodes `image` as a PNG byte stream, or `None` on malformed input.
pub(super) fn encode_png(image: &ClipboardImage) -> Option<Vec<u8>> {
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)?
        .checked_mul(4)?;
    if image.width == 0 || image.height == 0 || image.rgba.len() < expected {
        return None;
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.rgba[..expected]).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::encode_png;
    use typ_edit_editor::clipboard_image::ClipboardImage;

    /// Decodes `bytes` back to `(width, height, rgba)`; fails unless RGBA8.
    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("PNG info");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("output size")];
        let frame = reader.next_frame(&mut buf).expect("frame");
        assert_eq!(frame.color_type, png::ColorType::Rgba, "color type");
        assert_eq!(frame.bit_depth, png::BitDepth::Eight, "bit depth");
        (
            frame.width,
            frame.height,
            buf[..frame.buffer_size()].to_vec(),
        )
    }

    fn image(width: u32, height: u32, rgba: Vec<u8>) -> ClipboardImage {
        ClipboardImage {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn png_round_trip_preserves_pixels() {
        let source = image(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 10, 20, 30, 40,
            ],
        );
        let bytes = encode_png(&source).expect("encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
        let (width, height, rgba) = decode_png(&bytes);
        assert_eq!((width, height), (2, 2));
        assert_eq!(rgba, source.rgba);
    }

    #[test]
    fn encode_rejects_empty_dimensions() {
        assert_eq!(encode_png(&image(0, 0, Vec::new())), None);
        assert_eq!(encode_png(&image(0, 4, vec![0; 16])), None);
        assert_eq!(encode_png(&image(4, 0, vec![0; 16])), None);
    }

    #[test]
    fn encode_rejects_short_buffer() {
        assert_eq!(encode_png(&image(2, 2, vec![0; 15])), None);
    }
}
