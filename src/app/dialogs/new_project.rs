//! The new-project dialog: scaffolds a blank project (folder + `main.typ` +
//! `.typ-edit.json`) and opens it. The template field is always blank today;
//! P1-6c extends the selector with gallery entries.

use crate::app::NewProjectDialog;
use crate::shell::platform_ops;
use crate::theme;
use crossbeam_channel::TryRecvError;
use std::path::{Path, PathBuf};
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::list::option_row;
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::striped_grid::tui_grid;
use typ_edit_widgets::text_input::TextInput;
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Fixed content width of the new-project dialog.
const DIALOG_WIDTH: f32 = 420.0;

/// Why a requested project target cannot be created.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NewProjectError {
    /// The name failed [`platform_ops::validate_name`]; the string is its display message.
    InvalidName(String),
    /// A file or a non-empty folder already occupies `parent/name`.
    TargetNotEmpty,
}

/// Resolves the directory a new project named `name` would live at.
///
/// Rejects an invalid name, a file at the target, and a non-empty existing
/// folder; a fresh path or an empty existing folder is accepted.
pub(super) fn new_project_target(parent: &Path, name: &str) -> Result<PathBuf, NewProjectError> {
    let name = name.trim();
    platform_ops::validate_name(name).map_err(NewProjectError::InvalidName)?;
    let target = parent.join(name);
    if target.exists() && !is_empty_dir(&target) {
        return Err(NewProjectError::TargetNotEmpty);
    }
    Ok(target)
}

/// Whether `path` is a directory with no entries. An unreadable directory is
/// treated as non-empty, so it is never overwritten.
fn is_empty_dir(path: &Path) -> bool {
    path.is_dir()
        && crate::platform::fs_ops::read_dir(path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
}

/// The starting point of a new project: the user's Documents folder, else their
/// home, else the process working directory.
fn default_project_parent() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new() {
        if let Some(docs) = dirs.document_dir() {
            return docs.to_path_buf();
        }
        return dirs.home_dir().to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The minimal Typst document written into a blank project's `main.typ`.
pub(super) fn main_typ() -> String {
    t("dialog.new_project.default_content").into_owned()
}

/// Creates the project: the folder chain, `main.typ`, and the workspace marker
/// pointing at it. The caller has already proved the target is fresh or empty.
fn create_project(target: &Path) -> std::io::Result<()> {
    crate::platform::fs_ops::create_dir_all(target)?;
    let main = target.join("main.typ");
    crate::platform::fs_ops::write_atomic(&main, main_typ().as_bytes())?;
    crate::workspace::set_main_file(target, Some(&main))
}

/// App-only effects produced by the new-project dialog: a freshly created
/// project to open (with its one-shot toast) and the "从模板" request.
#[derive(Default)]
pub(in crate::app) struct NewProjectOutcome {
    pub(in crate::app) created_root: Option<PathBuf>,
    pub(in crate::app) toast: Option<String>,
    pub(in crate::app) open_gallery: bool,
}

impl NewProjectDialog {
    /// Opens the dialog with fresh fields, defaulting the parent to the user's
    /// Documents folder.
    pub(in crate::app) fn open_fresh(&mut self) {
        *self = NewProjectDialog {
            open: true,
            name: String::new(),
            parent: Some(default_project_parent()),
            template: None,
            error: None,
            preparing: false,
            init_rx: None,
        };
    }

    /// Presets the dialog from a gallery entry: pre-fills the name with the
    /// package name and selects `spec` as the template. Opens the dialog only
    /// when it is not already on screen, so a parent folder already chosen in
    /// the dialog is preserved.
    pub(in crate::app) fn preset(&mut self, name: &str, spec: String) {
        if !self.open {
            self.open_fresh();
        }
        self.name = name.to_owned();
        self.template = Some(spec);
        self.error = None;
    }

    /// Applies a finished template-instantiation worker: on success the
    /// workspace marker points at the entrypoint and the created root is
    /// returned for the caller to open; on failure the dialog shows the reason
    /// inline.
    fn pump_init(&mut self) -> Option<PathBuf> {
        let result = {
            let rx = self.init_rx.as_ref()?;
            match rx.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Err(String::new()),
            }
        };
        self.init_rx = None;
        self.preparing = false;
        match result {
            Ok(init) => {
                let dest = init.root.clone();
                let entry = dest.join(&init.entrypoint);
                if let Err(err) = crate::workspace::set_main_file(&dest, Some(&entry)) {
                    log::warn!("cannot write workspace marker: {err}");
                }
                *self = NewProjectDialog::default();
                Some(dest)
            }
            Err(message) if message.is_empty() => {
                self.error = Some(t("dialog.new_project.template_worker_lost").into_owned());
                None
            }
            Err(message) => {
                self.error = Some(t_with(
                    "dialog.new_project.template_failed",
                    &[("err", &message)],
                ));
                None
            }
        }
    }

    /// Renders the new-project dialog and, on 创建, scaffolds the project. The
    /// caller opens the created root (and raises the toast) or opens the gallery
    /// per the returned outcome. Validation failures set the dialog's inline
    /// error.
    pub(in crate::app) fn ui(&mut self, ui: &mut egui::Ui) -> NewProjectOutcome {
        let mut out = NewProjectOutcome::default();
        if !self.open {
            return out;
        }
        if let Some(root) = self.pump_init() {
            out.created_root = Some(root);
            out.toast = Some(t("toast.template_created").into_owned());
            return out;
        }
        if !self.open {
            return out;
        }
        let ctx = ui.ctx().clone();
        let mut dialog = std::mem::take(self);
        let mut confirm = false;
        let mut cancel = false;
        let mut from_gallery = false;
        egui::Modal::new(egui::Id::new("new-project-dialog")).show(&ctx, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(DIALOG_WIDTH);
                    ui.label(
                        egui::RichText::new(t("dialog.new_project.title"))
                            .size(15.0)
                            .strong(),
                    );
                    separator(ui);

                    let actionable = !dialog.preparing;
                    let mut name_response: Option<egui::Response> = None;
                    tui_grid(ui, "new-project-grid", |table| {
                        table.row(&t("dialog.new_project.name"), |ui| {
                            let has_error = dialog.error.is_some();
                            name_response = Some(
                                TextInput::form(
                                    &mut dialog.name,
                                    &t("dialog.new_project.name_hint"),
                                    240.0,
                                )
                                .error(has_error)
                                .show(ui),
                            );
                        });

                        table.row_tui(&t("dialog.new_project.location"), |tree| {
                            let location = dialog
                                .parent
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_default();
                            tree.style(tui::flexible()).ui(|ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(location).weak())
                                        .truncate(),
                                );
                            });
                            if tree
                                .ui(|ui| {
                                    button(
                                        ui,
                                        &t("dialog.new_project.browse"),
                                        ButtonVariant::Secondary,
                                        actionable,
                                    )
                                })
                                .clicked()
                            {
                                let start =
                                    dialog.parent.clone().unwrap_or_else(default_project_parent);
                                if let Some(dir) =
                                    rfd::FileDialog::new().set_directory(&start).pick_folder()
                                {
                                    dialog.parent = Some(dir);
                                }
                            }
                        });

                        table.row_tui(&t("dialog.new_project.template"), |tree| {
                            let selected_text = dialog.template.clone().unwrap_or_else(|| {
                                t("dialog.new_project.template_blank").into_owned()
                            });
                            tree.ui(|ui| {
                                crate::theme::combo_box_scope(ui, |ui| {
                                    egui::ComboBox::from_id_salt("new-project-template")
                                        .selected_text(selected_text)
                                        .show_ui(ui, |ui| {
                                            let blank = t("dialog.new_project.template_blank");
                                            if option_row(ui, dialog.template.is_none(), &blank)
                                                .clicked()
                                            {
                                                dialog.template = None;
                                                ui.close();
                                            }
                                            if let Some(spec) = dialog.template.clone()
                                                && option_row(ui, true, &spec).clicked()
                                            {
                                                ui.close();
                                            }
                                        });
                                });
                            });
                            if tree
                                .ui(|ui| {
                                    button(
                                        ui,
                                        &t("dialog.new_project.from_gallery"),
                                        ButtonVariant::Quiet,
                                        actionable,
                                    )
                                })
                                .clicked()
                            {
                                from_gallery = true;
                            }
                        });
                    });
                    if actionable
                        && let Some(name_response) = &name_response
                        && ui.ctx().memory(|memory| memory.focused().is_none())
                    {
                        name_response.request_focus();
                    }

                    if dialog.preparing {
                        ui.label(egui::RichText::new(t("dialog.new_project.preparing")).weak());
                    }
                    if let Some(message) = &dialog.error {
                        ui.colored_label(theme::error(ui), message);
                    }
                    if actionable && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                    if actionable
                        && name_response.as_ref().is_some_and(|r| r.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        confirm = true;
                    }
                    let base = ui.id().with(("typ_edit_new_project", "actions"));
                    let mut actions_style = tui::row_reverse_gap(ui.spacing().item_spacing.x);
                    tui::fill_width(&mut actions_style);
                    let actions_width = ui.available_width();
                    tui::tui(ui, base)
                        .reserve_width(actions_width)
                        .style(actions_style)
                        .show(|actions| {
                            if actions
                                .id(base.with("create"))
                                .ui(|ui| {
                                    button(
                                        ui,
                                        &t("dialog.new_project.create"),
                                        ButtonVariant::Primary,
                                        actionable,
                                    )
                                })
                                .clicked()
                            {
                                confirm = true;
                            }
                            if actions
                                .id(base.with("cancel"))
                                .ui(|ui| {
                                    button(
                                        ui,
                                        &t("dialog.new_project.cancel"),
                                        ButtonVariant::Secondary,
                                        actionable,
                                    )
                                })
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                });
        });

        if cancel {
            return out;
        }
        let mut keep_open = true;
        if confirm && !dialog.preparing {
            let parent = dialog.parent.clone().unwrap_or_else(default_project_parent);
            dialog.parent = Some(parent.clone());
            match new_project_target(&parent, &dialog.name) {
                Err(NewProjectError::InvalidName(message)) => dialog.error = Some(message),
                Err(NewProjectError::TargetNotEmpty) => {
                    dialog.error = Some(t("dialog.new_project.exists").into_owned());
                }
                Ok(target) => match dialog.template.clone() {
                    Some(spec) => {
                        dialog.error = None;
                        dialog.preparing = true;
                        // Init channel: bounded(1) — the short-lived worker sends
                        // exactly one result, read once by `pump_init`. Its
                        // blocking `send` cannot block the UI (worker thread only).
                        let (tx, rx) = crossbeam_channel::bounded(1);
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let result = typ_edit_engine::template::init_project(&spec, &target);
                            let _ = tx.send(result);
                            ctx.request_repaint();
                        });
                        dialog.init_rx = Some(rx);
                    }
                    None => match create_project(&target) {
                        Ok(()) => {
                            keep_open = false;
                            out.created_root = Some(target);
                            out.toast = Some(t("toast.project_created").into_owned());
                        }
                        Err(err) => {
                            dialog.error =
                                Some(t_with("error.create_failed", &[("err", &err.to_string())]));
                        }
                    },
                },
            }
        }
        if keep_open {
            dialog.open = true;
            *self = dialog;
        }
        if from_gallery && !confirm {
            out.open_gallery = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// Unique, self-cleaning scratch directory under the OS temp dir.
    fn scratch_dir(tag: &str) -> PathBuf {
        let seq = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-new-project-{}-{seq}-{nanos}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn target_accepts_a_fresh_name() {
        let dir = scratch_dir("fresh");
        assert_eq!(new_project_target(&dir, "book"), Ok(dir.join("book")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn target_rejects_an_invalid_name() {
        let dir = scratch_dir("invalid");
        assert!(matches!(
            new_project_target(&dir, ""),
            Err(NewProjectError::InvalidName(_))
        ));
        assert!(matches!(
            new_project_target(&dir, "a/b"),
            Err(NewProjectError::InvalidName(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn target_rejects_a_non_empty_existing_dir() {
        let dir = scratch_dir("nonempty");
        let existing = dir.join("book");
        std::fs::create_dir_all(&existing).expect("create existing");
        std::fs::write(existing.join("main.typ"), "x").expect("write file");
        assert_eq!(
            new_project_target(&dir, "book"),
            Err(NewProjectError::TargetNotEmpty)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn target_allows_an_existing_empty_dir() {
        let dir = scratch_dir("empty");
        let existing = dir.join("book");
        std::fs::create_dir_all(&existing).expect("create existing");
        assert_eq!(new_project_target(&dir, "book"), Ok(existing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn target_rejects_a_file_with_the_same_name() {
        let dir = scratch_dir("file");
        std::fs::write(dir.join("book"), "x").expect("write file");
        assert_eq!(
            new_project_target(&dir, "book"),
            Err(NewProjectError::TargetNotEmpty)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn target_allows_a_missing_nested_parent() {
        let dir = scratch_dir("nested");
        let parent = dir.join("a").join("b");
        let target = dir.join("a").join("b").join("book");
        assert_eq!(new_project_target(&parent, "book"), Ok(target.clone()));
        create_project(&target).expect("create nested project");
        assert!(target.join("main.typ").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_project_writes_main_and_the_workspace_marker() {
        let dir = scratch_dir("create");
        let target = dir.join("book");
        create_project(&target).expect("create project");
        assert!(target.join("main.typ").is_file());
        assert!(target.join(crate::workspace::MAIN_MARKER).is_file());
        assert_eq!(
            crate::workspace::main_file(&target),
            Some(target.join("main.typ"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn main_typ_is_a_non_empty_document_with_a_heading() {
        let text = main_typ();
        assert!(!text.trim().is_empty());
        assert!(
            text.lines().any(|line| line.trim_start().starts_with("= ")),
            "the template must contain a title heading"
        );
    }
}
