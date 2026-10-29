use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use egui::{ProgressBar, RichText};
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DownloadStatus {
    Downloading,
    NeedsConfirm,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
pub(super) struct DownloadTask {
    pub(super) path: String,
    pub(super) received: u64,
    pub(super) total: u64,
    reported: u64,
    last_pct: f32,
    pub(super) status: DownloadStatus,
    pub(super) size_hint: u64,
    temp: Option<PathBuf>,
}

impl DownloadTask {
    fn new(path: String) -> Self {
        Self {
            path,
            received: 0,
            total: 0,
            reported: 0,
            last_pct: 0.0,
            status: DownloadStatus::Downloading,
            size_hint: 0,
            temp: None,
        }
    }

    fn fraction(&self) -> f32 {
        if self.status == DownloadStatus::Done {
            1.0
        } else if self.total == 0 {
            0.0
        } else {
            (self.reported as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

#[derive(Default)]
pub(crate) struct DownloadState {
    tasks: Vec<DownloadTask>,
    requested: BTreeSet<String>,
    pub(super) panel_open: bool,
    prompt: Option<String>,
}

pub(super) enum DownloadAction {
    None,
    Confirm(String),
    Cancel,
}

impl DownloadState {
    /// Whether the background-task panel or a too-large confirmation is
    /// currently covering the workspace.
    pub(super) fn modal_open(&self) -> bool {
        self.panel_open || self.prompt.is_some()
    }

    pub(super) fn begin(&mut self, path: &str) -> bool {
        if path.is_empty() || self.requested.contains(path) {
            return false;
        }
        self.requested.insert(path.to_owned());
        self.tasks.push(DownloadTask::new(path.to_owned()));
        true
    }

    pub(super) fn progress(
        &mut self,
        path: &str,
        offset: u64,
        total: u64,
        bytes: &[u8],
        mirror: &Path,
    ) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) else {
            return false;
        };
        let Some(target) = super::collab::join_rel(mirror, path) else {
            task.status = DownloadStatus::Failed;
            return false;
        };
        let temp = task.temp.get_or_insert_with(|| temp_path(&target));
        if offset == 0 {
            if let Some(parent) = temp.parent()
                && let Err(err) = fs::create_dir_all(parent)
            {
                log::warn!("cannot create download dir {}: {err}", parent.display());
                task.status = DownloadStatus::Failed;
                return false;
            }
            if let Err(err) = fs::write(&*temp, []) {
                log::warn!("cannot create download temp {}: {err}", temp.display());
                task.status = DownloadStatus::Failed;
                return false;
            }
        } else if let Some(parent) = temp.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            log::warn!("cannot create download dir {}: {err}", parent.display());
            task.status = DownloadStatus::Failed;
            return false;
        }
        let mut file = match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&*temp)
        {
            Ok(file) => file,
            Err(err) => {
                log::warn!("cannot open download temp {}: {err}", temp.display());
                task.status = DownloadStatus::Failed;
                return false;
            }
        };
        if let Err(err) = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(bytes))
        {
            log::warn!("cannot write download chunk {}: {err}", temp.display());
            task.status = DownloadStatus::Failed;
            return false;
        }
        task.received = task.received.max(offset + bytes.len() as u64);
        task.total = total;
        let complete = total == 0 || task.received >= total;
        let pct = if total == 0 {
            100.0
        } else {
            task.received as f32 * 100.0 / total as f32
        };
        if complete || pct - task.last_pct >= 0.5 {
            task.reported = task.received;
            task.last_pct = pct;
        }
        if complete {
            if let Err(err) = fs::rename(&*temp, &target) {
                log::warn!("cannot finalize download {}: {err}", target.display());
                task.status = DownloadStatus::Failed;
                return false;
            }
            task.status = DownloadStatus::Done;
            task.reported = task.received;
            task.temp = None;
            self.requested.remove(path);
            return true;
        }
        false
    }

    pub(super) fn too_large(&mut self, path: &str, size: u64) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) {
            task.status = DownloadStatus::NeedsConfirm;
            task.size_hint = size;
        }
        self.refresh_prompt();
    }

    fn refresh_prompt(&mut self) {
        if self.prompt.is_none() {
            self.prompt = self
                .tasks
                .iter()
                .find(|task| task.status == DownloadStatus::NeedsConfirm)
                .map(|task| task.path.clone());
        }
    }

    pub(super) fn unavailable(&mut self, path: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) {
            task.status = DownloadStatus::Failed;
        }
        if self.prompt.as_deref() == Some(path) {
            self.prompt = None;
        }
        self.refresh_prompt();
    }

    pub(super) fn complete(&mut self, path: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) {
            task.status = DownloadStatus::Done;
            task.received = task.total.max(task.received);
            task.reported = task.received;
            task.total = task.total.max(task.received);
            if let Some(temp) = task.temp.take() {
                let _ = fs::remove_file(temp);
            }
        }
        self.requested.remove(path);
    }

    pub(super) fn confirm(&mut self, path: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) {
            task.status = DownloadStatus::Downloading;
            task.received = 0;
            task.total = 0;
            task.reported = 0;
            task.last_pct = 0.0;
            task.temp = None;
        }
        self.prompt = None;
        self.refresh_prompt();
    }

    pub(super) fn cancel(&mut self, path: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.path == path) {
            task.status = DownloadStatus::Failed;
        }
        if self.prompt.as_deref() == Some(path) {
            self.prompt = None;
        }
        self.refresh_prompt();
    }

    pub(super) fn active_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    DownloadStatus::Downloading | DownloadStatus::NeedsConfirm
                )
            })
            .count()
    }

    pub(super) fn fetching(&self) -> bool {
        self.active_count() > 0
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context) -> DownloadAction {
        if let Some(path) = self.prompt.clone() {
            let size = self
                .tasks
                .iter()
                .find(|task| task.path == path)
                .map_or(0, |task| task.size_hint);
            let mut confirm = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::new("download-too-large")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading(t("downloads.large.title"));
                ui.label(t_with(
                    "downloads.large.body",
                    &[("path", &path), ("size", &format_bytes(size))],
                ));
                ui.horizontal(|ui| {
                    if button(
                        ui,
                        &t("downloads.large.fetch"),
                        ButtonVariant::Primary,
                        true,
                    )
                    .clicked()
                    {
                        confirm = true;
                    }
                    if button(ui, &t("downloads.cancel"), ButtonVariant::Secondary, true).clicked()
                    {
                        cancel = true;
                    }
                });
            });
            if confirm {
                self.confirm(&path);
                return DownloadAction::Confirm(path);
            }
            if cancel {
                self.cancel(&path);
                return DownloadAction::Cancel;
            }
            return DownloadAction::None;
        }

        if !self.panel_open {
            return DownloadAction::None;
        }
        let tasks = self.tasks.clone();
        let mut close = false;
        egui::Modal::new(egui::Id::new("download-panel")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading(t("downloads.title"));
            egui::ScrollArea::vertical()
                .max_height(340.0)
                .show(ui, |ui| {
                    if tasks.is_empty() {
                        ui.label(t("downloads.empty"));
                    }
                    for task in &tasks {
                        ui.label(RichText::new(&task.path).strong());
                        let (status, fraction) = match task.status {
                            DownloadStatus::Downloading => {
                                (t("downloads.downloading"), task.fraction())
                            }
                            DownloadStatus::NeedsConfirm => (t("downloads.waiting"), 0.0),
                            DownloadStatus::Done => (t("downloads.done"), 1.0),
                            DownloadStatus::Failed => (t("downloads.failed"), 0.0),
                        };
                        ui.add(ProgressBar::new(fraction).show_percentage().text(format!(
                            "{status} · {} / {}",
                            format_bytes(task.reported),
                            if task.total == 0 {
                                "?".to_owned()
                            } else {
                                format_bytes(task.total)
                            }
                        )));
                        ui.add_space(4.0);
                    }
                });
            if button(ui, &t("downloads.close"), ButtonVariant::Secondary, true).clicked() {
                close = true;
            }
        });
        if close {
            self.panel_open = false;
        }
        DownloadAction::None
    }
}

impl super::TypEditApp {
    pub(super) fn request_missing_assets(&mut self, missing: &[String]) {
        if !self.collab.is_client() {
            return;
        }
        for path in missing {
            if self.downloads.begin(path) {
                self.collab.get_chunked_file(path, false);
            }
        }
    }
}

fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    target.with_file_name(format!("{name}.part"))
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_written_and_finalized() {
        let root = std::env::temp_dir().join(format!("typ-edit-downloads-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut state = DownloadState::default();
        assert!(state.begin("image/a.bin"));
        state.too_large("image/a.bin", 5);
        assert!(state.fetching());
        state.confirm("image/a.bin");
        assert!(!state.progress("image/a.bin", 0, 5, b"abc", &root));
        assert!(state.progress("image/a.bin", 3, 5, b"de", &root));
        assert_eq!(fs::read(root.join("image/a.bin")).expect("file"), b"abcde");
        assert!(!state.fetching());
        assert_eq!(state.active_count(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_requests_are_coalesced() {
        let mut state = DownloadState::default();
        assert!(state.begin("a.png"));
        assert!(!state.begin("a.png"));
        state.complete("a.png");
        assert!(state.begin("a.png"));
    }
}
