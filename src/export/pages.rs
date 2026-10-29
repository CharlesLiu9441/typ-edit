//! Page-based export: PDF, PNG and SVG.

use super::{ExportFormat, write_bytes};
use std::path::{Path, PathBuf};
use typ_edit_i18n::{t, t_with};
use typst::utils::Scalar;
use typst_layout::{Page, PagedDocument};

/// Exports every page into a single PDF file via a save dialog.
pub(super) fn export_pdf(
    document: &PagedDocument,
    stem: &str,
    default_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    let mut dialog = rfd::FileDialog::new()
        .set_file_name(format!("{stem}.pdf"))
        .add_filter(ExportFormat::Pdf.label(), &["pdf"]);
    if let Some(dir) = default_dir {
        dialog = dialog.set_directory(dir);
    }
    let Some(path) = dialog.save_file() else {
        return Ok(None);
    };

    let bytes = typst_pdf::pdf(document, &typst_pdf::PdfOptions::default()).map_err(|errors| {
        errors
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    write_bytes(&path, &bytes)?;
    let path_display = path.display().to_string();
    Ok(Some(t_with("export.pdf_ok", &[("path", &path_display)])))
}

/// Exports PNG or SVG. Multi-page documents ask for a folder and write one
/// file per page (`"{stem}-{n}.{ext}"`, 1-based); single-page documents ask
/// for a file.
pub(super) fn export_images(
    document: &PagedDocument,
    stem: &str,
    format: ExportFormat,
    dpi: f32,
    default_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    let pages = document.pages();
    if pages.is_empty() {
        return Err(t("export.no_pages").into_owned());
    }

    let note = if format == ExportFormat::Png {
        t_with(
            "export.note_dpi",
            &[("dpi", &(dpi.round() as i64).to_string())],
        )
    } else {
        String::new()
    };

    if pages.len() > 1 {
        let mut dialog = rfd::FileDialog::new();
        if let Some(default_dir) = default_dir {
            dialog = dialog.set_directory(default_dir);
        }
        let Some(dir) = dialog.pick_folder() else {
            return Ok(None);
        };

        let targets = image_targets(&dir, stem, pages.len(), format.extension());
        let existing = existing_count(&targets);
        if existing > 0 {
            let answer = rfd::MessageDialog::new()
                .set_title(t("export.exists.title"))
                .set_description(t_with(
                    "export.exists.body",
                    &[("count", &existing.to_string())],
                ))
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show();
            if !matches!(answer, rfd::MessageDialogResult::Ok) {
                return Ok(None);
            }
        }

        for (index, page) in pages.iter().enumerate() {
            write_page(page, format, dpi, &targets[index])?;
        }

        let dir_display = dir.display().to_string();
        Ok(Some(t_with(
            "export.pages_ok",
            &[
                ("count", &pages.len().to_string()),
                ("dir", &dir_display),
                ("note", &note),
            ],
        )))
    } else {
        let file_name = format!("{stem}.{}", format.extension());
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(file_name)
            .add_filter(format.label(), &[format.extension()]);
        if let Some(default_dir) = default_dir {
            dialog = dialog.set_directory(default_dir);
        }
        let Some(path) = dialog.save_file() else {
            return Ok(None);
        };

        write_page(&pages[0], format, dpi, &path)?;
        let path_display = path.display().to_string();
        Ok(Some(t_with(
            "export.file_ok",
            &[("path", &path_display), ("note", &note)],
        )))
    }
}

/// Encodes and writes a single page in the given image format.
fn write_page(page: &Page, format: ExportFormat, dpi: f32, path: &Path) -> Result<(), String> {
    match format {
        ExportFormat::Png => {
            let options = typst_render::RenderOptions {
                pixel_per_pt: Scalar::new(dpi.clamp(1.0, 4800.0) as f64 / 72.0),
                render_bleed: false,
            };
            let pixmap = typst_render::render(page, &options);
            let bytes = pixmap.encode_png().map_err(|error| {
                t_with("export.png_encode_failed", &[("error", &error.to_string())])
            })?;
            write_bytes(path, &bytes)
        }
        ExportFormat::Svg => {
            let options = typst_svg::SvgOptions {
                render_bleed: false,
                pretty: false,
            };
            let svg = typst_svg::svg(page, &options);
            write_bytes(path, svg.as_bytes())
        }
        ExportFormat::Pdf => Err(t("export.page_unsupported").into_owned()),
    }
}

/// Target file paths of a multi-page image export: `"{stem}-{n}.{ext}"`, 1-based.
fn image_targets(dir: &Path, stem: &str, count: usize, ext: &str) -> Vec<PathBuf> {
    (0..count)
        .map(|index| dir.join(format!("{stem}-{}.{ext}", index + 1)))
        .collect()
}

/// Number of `targets` that already exist on disk.
fn existing_count(targets: &[PathBuf]) -> usize {
    targets.iter().filter(|path| path.exists()).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-export-{}-{id}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn image_targets_are_one_based_and_ordered() {
        let targets = image_targets(Path::new("/out"), "doc", 3, "png");
        assert_eq!(
            targets,
            [
                PathBuf::from("/out/doc-1.png"),
                PathBuf::from("/out/doc-2.png"),
                PathBuf::from("/out/doc-3.png"),
            ]
        );
    }

    #[test]
    fn existing_count_counts_only_present_targets() {
        let dir = scratch_dir();
        std::fs::write(dir.join("doc-1.png"), b"x").expect("write first");
        std::fs::write(dir.join("doc-3.png"), b"x").expect("write third");
        let targets = image_targets(&dir, "doc", 4, "png");
        assert_eq!(existing_count(&targets), 2);
        assert_eq!(existing_count(&[]), 0);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
