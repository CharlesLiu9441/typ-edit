//! Pure media-path helpers shared by the app and the shell.
//!
//! [`is_supported_image`] classifies a path by extension with no I/O, so both
//! the app's import pipeline and the shell's project tree can recognize an
//! image without depending on each other.

use std::path::Path;

/// Image extensions accepted for import, lowercase, without the leading dot.
const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp"];

/// Whether `path`'s extension is one of the supported image formats
/// (case-insensitive).
pub(crate) fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}
