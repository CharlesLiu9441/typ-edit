//! OS drag-and-drop image import: the directory/naming/snippet helpers shared
//! with clipboard paste, the drop-batch handler, and the hover drop-hint
//! overlay. The insertion-position classifier lives in the [`insert`] submodule;
//! the pure helpers ([`is_supported_image`], [`unique_image_path`]) and the
//! classifier are unit-tested in [`super::tests`]; the [`TypEditApp`] methods are
//! the thin IO glue over them.

pub(super) mod insert;

use super::TypEditApp;
use super::files::relative_snippet_path;
use std::path::{Path, PathBuf};
use typ_edit_editor::clipboard_image::ClipboardImage;
use typ_edit_i18n::{t, t_with};

use self::insert::{ImageForm, InsertDecision, decide_insertion, image_blocked_ranges};

pub(crate) use crate::media::is_supported_image;

/// Alpha (out of 255) of the translucent veil behind the hover drop hint.
const DROP_HINT_VEIL_ALPHA: u8 = 72;

/// Upper bound on the `<stem>-N` probe: a directory holding this many same-named
/// files is pathological, and an unbounded counter would eventually overflow or
/// spin.
const MAX_UNIQUE_ATTEMPTS: u32 = 10_000;

/// First `<stem>-N.<ext>` path that does not collide inside `dir`, starting at
/// `1`. `None` when `stem` or `ext` is empty, or when every candidate up to
/// [`MAX_UNIQUE_ATTEMPTS`] is taken.
pub(super) fn unique_image_path(dir: &Path, stem: &str, ext: &str) -> Option<PathBuf> {
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    for index in 1..=MAX_UNIQUE_ATTEMPTS {
        let path = dir.join(format!("{stem}-{index}.{ext}"));
        if !path.exists() {
            return Some(path);
        }
    }
    None
}

/// Whether `source` already lives inside the workspace `root`, so an import can
/// reference it in place instead of copying it into the image directory.
pub(super) fn references_in_place(source: &Path, root: Option<&Path>) -> bool {
    root.is_some_and(|root| source.starts_with(root))
}

/// Joins the collected `#image` snippets for one batch into the single text
/// payload inserted at the drop position.
pub(super) fn combined_snippets(snippets: &[String]) -> String {
    snippets.join("\n")
}

/// Paints the centered drop hint while `count` files hover over the window.
///
/// The full-window translucent veil is painted by the area's own painter (its
/// clip rect is the whole content rect), so the veil and the label share one
/// non-interactable foreground layer with a stable z-order.
pub(super) fn show_drop_hint(ctx: &egui::Context, count: usize) {
    let n = count.to_string();
    egui::Area::new(egui::Id::new("drop_hint"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .interactable(false)
        .show(ctx, |ui| {
            ui.painter().rect_filled(
                ui.ctx().content_rect(),
                egui::CornerRadius::same(0),
                egui::Color32::from_black_alpha(DROP_HINT_VEIL_ALPHA),
            );
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new(t_with("overlay.drop_hint", &[("n", &n)])).strong());
            });
        });
}

impl TypEditApp {
    /// Directory imported images are written to: the workspace root or the
    /// active file's parent, joined with `settings.workspace.image_dir`. `None` when
    /// neither is known (no workspace and an unnamed scratch buffer).
    pub(super) fn image_target_dir(&self) -> Option<PathBuf> {
        let image_dir = self.settings.workspace.image_dir.as_str();
        self.tree
            .root
            .as_ref()
            .map(|root| root.join(image_dir))
            .or_else(|| {
                self.editor()
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(|parent| parent.join(image_dir))
            })
    }

    /// The image source for `path`, relative to the active file's directory
    /// (falling back to the bare file name when no root is shared), wrapped in
    /// the `#image("...")` markup or `image("...")` code form.
    fn image_snippet(&self, path: &Path, form: ImageForm) -> String {
        let name = file_name(path);
        let snippet = self
            .editor()
            .path
            .as_deref()
            .and_then(Path::parent)
            .and_then(|from| relative_snippet_path(from, path))
            .unwrap_or(name);
        match form {
            ImageForm::Markup => format!("#image(\"{snippet}\")"),
            ImageForm::Code => format!("image(\"{snippet}\")"),
        }
    }

    /// The snippet form for an image inserted at `byte`, or `None` (after a
    /// refusal toast) when the position would corrupt the source.
    pub(super) fn image_form_at(&mut self, byte: usize) -> Option<ImageForm> {
        let decision = {
            let parse = &self.compile.parse;
            let blocked = image_blocked_ranges(parse);
            let code: Vec<std::ops::Range<usize>> = parse
                .layout
                .code_ranges
                .iter()
                .map(|range| range.start.0..range.end.0)
                .collect();
            let math: Vec<std::ops::Range<usize>> = parse
                .layout
                .math_ranges
                .iter()
                .map(|range| range.start.0..range.end.0)
                .collect();
            decide_insertion(
                self.editor().shared_text().as_str(),
                byte,
                &code,
                &math,
                &blocked,
            )
        };
        match decision {
            InsertDecision::Insert(form) => Some(form),
            InsertDecision::Reject(_) => {
                self.toast_warning(t("toast.image_reject"));
                None
            }
        }
    }

    /// Saves a clipboard bitmap as a PNG under the configured image directory
    /// and inserts a Typst image snippet at the caret, using the `#image("...")`
    /// markup form in markup context and the `image("...")` form in code. A
    /// caret where insertion would corrupt the source is refused before any
    /// file is written.
    pub(super) fn paste_image(&mut self, image: ClipboardImage) {
        // Collab mode: upload the PNG and let the server's `ImageAdded` drive the
        // snippet insertion (never a guessed filename).
        if self.collab.is_active() {
            let Some(bytes) = crate::paste_image::encode_png(&image) else {
                self.toast_error(t("toast.image_encode_failed"));
                return;
            };
            if self.collab.is_client() {
                self.collab.upload_image("pasted.png", bytes);
            } else {
                self.collab.publish_image("pasted.png", bytes);
            }
            return;
        }
        let Some(form) = self.image_form_at(self.compile.cursor_byte) else {
            return;
        };
        let Some(target_dir) = self.image_target_dir() else {
            self.toast_warning(t("toast.image_needs_folder"));
            return;
        };
        if let Err(err) = crate::platform::fs_ops::create_dir_all(&target_dir) {
            self.toast_error(t_with(
                "toast.image_save_failed",
                &[("err", &err.to_string())],
            ));
            return;
        }
        // Unreachable: the stem and extension passed here are non-empty constants.
        let Some(path) = unique_image_path(&target_dir, "pasted", "png") else {
            return;
        };
        let Some(bytes) = crate::paste_image::encode_png(&image) else {
            self.toast_error(t("toast.image_encode_failed"));
            return;
        };
        if let Err(err) = crate::platform::fs_ops::write_atomic(&path, &bytes) {
            self.toast_error(t_with(
                "toast.image_save_failed",
                &[("err", &err.to_string())],
            ));
            return;
        }
        self.insert_image_snippet(&path, form);
    }

    /// Queues the snippet for one imported image at the caret and reports it.
    pub(super) fn insert_image_snippet(&mut self, path: &Path, form: ImageForm) {
        let snippet = self.image_snippet(path, form);
        self.editor_mut().queue_insert_text(snippet);
        let name = file_name(path);
        self.toast_success(t_with("toast.image_pasted", &[("name", &name)]));
    }

    /// Imports the current frame's OS drop batch at the pointer byte captured
    /// while the pointer was over the editor (the caret when `None`).
    pub(super) fn import_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let at_byte = self.compile.pointer_byte;
        self.import_image_sources(&dropped, at_byte);
    }

    /// Imports `sources` — external drops and project-tree drags share this one
    /// path — as a single `#image` block inserted at `at_byte` (the caret when
    /// `None`).
    ///
    /// A source already inside the workspace is referenced in place; anything
    /// else is copied into the image directory under a collision-checked
    /// `dropped-N.ext` name. A batch with no supported image shows the
    /// unsupported-type toast; a copy-only batch without a resolvable target
    /// directory shows the open-a-folder toast and imports nothing. A target
    /// byte where insertion would corrupt the source is refused before any copy.
    pub(super) fn import_image_sources(&mut self, sources: &[PathBuf], at_byte: Option<usize>) {
        let images: Vec<&PathBuf> = sources
            .iter()
            .filter(|path| path.is_file() && is_supported_image(path))
            .collect();
        let skipped = sources.len() - images.len();
        if images.is_empty() {
            if skipped > 0 {
                self.toast_warning(t("toast.drop_unsupported"));
            }
            return;
        }
        let target_byte = at_byte.unwrap_or(self.compile.cursor_byte);
        let Some(form) = self.image_form_at(target_byte) else {
            return;
        };
        let root = self.tree.root.clone();
        let needs_copy = images
            .iter()
            .any(|path| !references_in_place(path, root.as_deref()));
        // In-place references reuse the existing file, so only a batch that
        // copies anything needs (and resolves) the image directory.
        let target_dir = if needs_copy {
            let Some(dir) = self.image_target_dir() else {
                self.toast_warning(t("toast.drop_needs_folder"));
                return;
            };
            if let Err(err) = crate::platform::fs_ops::create_dir_all(&dir) {
                self.toast_error(t_with(
                    "toast.image_save_failed",
                    &[("err", &err.to_string())],
                ));
                return;
            }
            Some(dir)
        } else {
            None
        };
        // `queue_insert_text` replaces the single pending insert, so every
        // destination is resolved and every snippet collected before the batch
        // is joined into exactly one call.
        let mut snippets = Vec::with_capacity(images.len());
        for source in images {
            let imported = if references_in_place(source, root.as_deref()) {
                Some(source.clone())
            } else {
                copy_import(source, target_dir.as_deref())
            };
            if let Some(path) = imported {
                snippets.push(self.image_snippet(&path, form));
            }
        }
        if snippets.is_empty() {
            if skipped > 0 {
                self.toast_warning(t("toast.drop_unsupported"));
            }
            return;
        }
        if let Some(byte) = at_byte {
            self.editor_mut().jump_to_byte(byte);
        }
        let n = snippets.len().to_string();
        self.editor_mut()
            .queue_insert_text(combined_snippets(&snippets));
        self.toast_success(t_with("toast.dropped_images", &[("n", &n)]));
    }
}

/// Copies `source` into `target_dir` under the next free `dropped-N.<ext>`
/// name (extension preserved); `None` when the extension is unreadable, no name
/// is free, or the copy fails.
fn copy_import(source: &Path, target_dir: Option<&Path>) -> Option<PathBuf> {
    let ext = source.extension().and_then(|ext| ext.to_str())?;
    let dest = unique_image_path(target_dir?, "dropped", ext)?;
    match crate::platform::fs_ops::copy(source, &dest) {
        Ok(_) => Some(dest),
        Err(err) => {
            log::warn!("cannot copy {}: {err}", source.display());
            None
        }
    }
}

/// Lossy file name of `path`, empty when it has none.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
