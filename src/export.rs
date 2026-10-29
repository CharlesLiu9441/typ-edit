//! Document export: PDF / PNG / SVG.
//!
//! [`spawn_export`] is the driver; the per-format writers live in [`pages`],
//! working from the finished [`PagedDocument`].

mod pages;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use typ_edit_i18n::t_with;
use typst_layout::PagedDocument;

/// The output formats supported by [`spawn_export`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// A single, multi-page PDF file.
    Pdf,
    /// One PNG image per page.
    Png,
    /// One SVG image per page.
    Svg,
}

impl ExportFormat {
    /// The canonical file extension, without the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Svg => "svg",
        }
    }

    /// A short human-readable label for menus and file dialogs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "PDF",
            Self::Png => "PNG",
            Self::Svg => "SVG",
        }
    }
}

/// One completed export, tagged with its success state so the UI can pick the
/// matching semantic toast family.
pub struct ExportReport {
    pub message: String,
    pub ok: bool,
}

/// Spawns a background export.
///
/// Everything — including the blocking native file dialogs — runs on a
/// dedicated thread, so the UI thread never stalls. Exactly one message is
/// sent through `report`:
///
/// * `Some(report)` on success, and on failure with a `"导出失败：…"` message,
/// * `None` when the user cancels a dialog.
///
/// `dpi` only affects PNG rasterization (typst-cli uses 144).
///
/// Afterwards `ctx.request_repaint()` wakes the UI so it can surface the
/// result.
pub fn spawn_export(
    document: Arc<PagedDocument>,
    stem: String,
    format: ExportFormat,
    dpi: f32,
    default_dir: Option<PathBuf>,
    ctx: egui::Context,
    report: crossbeam_channel::Sender<Option<ExportReport>>,
) {
    std::thread::spawn(move || {
        let payload = match run_export(&document, &stem, format, dpi, default_dir.as_deref()) {
            Ok(Some(message)) => Some(ExportReport { message, ok: true }),
            Ok(None) => None,
            Err(reason) => {
                log::error!("导出失败：{reason}");
                Some(ExportReport {
                    message: t_with("export.failed", &[("reason", reason.as_str())]),
                    ok: false,
                })
            }
        };

        let _ = report.send(payload);
        ctx.request_repaint();
    });
}

/// Shared export driver. Runs on the export thread.
///
/// Returns `Ok(Some(message))` on success, `Ok(None)` when a dialog was
/// cancelled, and `Err(reason)` if the document could not be written.
fn run_export(
    document: &PagedDocument,
    stem: &str,
    format: ExportFormat,
    dpi: f32,
    default_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    let stem = sanitize_stem(stem);
    match format {
        ExportFormat::Pdf => pages::export_pdf(document, &stem, default_dir),
        ExportFormat::Png | ExportFormat::Svg => {
            pages::export_images(document, &stem, format, dpi, default_dir)
        }
    }
}

/// Writes raw bytes to `path`, mapping I/O failures to a reportable reason.
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::platform::fs_ops::write_atomic(path, bytes).map_err(|error| {
        let path_display = path.display().to_string();
        t_with(
            "export.write_failed",
            &[("path", &path_display), ("error", &error.to_string())],
        )
    })
}

/// Turns an arbitrary document stem into a safe file name.
///
/// Strips any directory components, replaces path separators and reserved
/// characters, and falls back to `"document"` when nothing usable remains.
fn sanitize_stem(stem: &str) -> String {
    let base = stem.rsplit(['/', '\\']).next().unwrap_or(stem);

    let mut cleaned = String::with_capacity(base.len());
    for c in base.trim().chars() {
        if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            cleaned.push('_');
        } else {
            cleaned.push(c);
        }
    }

    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c == ' ');
    if cleaned.is_empty() {
        "document".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_expose_extension_and_label() {
        for (format, extension, label) in [
            (ExportFormat::Pdf, "pdf", "PDF"),
            (ExportFormat::Png, "png", "PNG"),
            (ExportFormat::Svg, "svg", "SVG"),
        ] {
            assert_eq!(format.extension(), extension);
            assert_eq!(format.label(), label);
        }
    }
}
