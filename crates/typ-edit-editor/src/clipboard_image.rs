//! Platform clipboard reads for bitmap pasting (arboard).
//!
//! This module deliberately stops at the read. The editor reports a pasted
//! bitmap through `EditorOutput::paste_image`; the application owns encoding
//! it to PNG when (and where) it writes the file.

use std::borrow::Cow;

/// A bitmap read from the system clipboard, always RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Reads a bitmap from the clipboard for pasting.
///
/// Returns `Some` only when the clipboard carries no meaningful text: copied
/// rich content (a web page or a Word selection) also exposes an image flavor,
/// and such pastes must keep pasting text. A screenshot has no text and
/// therefore qualifies.
pub fn read_for_paste() -> Option<ClipboardImage> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if clipboard
        .get_text()
        .is_ok_and(|text| !text.trim().is_empty())
    {
        return None;
    }
    let image = clipboard.get_image().ok()?;
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba: Vec<u8> = match image.bytes {
        Cow::Borrowed(bytes) => bytes.to_vec(),
        Cow::Owned(bytes) => bytes,
    };
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return None;
    }
    rgba.truncate(expected);
    Some(ClipboardImage {
        width,
        height,
        rgba,
    })
}
