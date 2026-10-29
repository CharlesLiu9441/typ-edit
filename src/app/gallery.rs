//! The template gallery window and the package-manager worker plumbing.
//!
//! The gallery window itself only lists templates (see [`render`]). The package
//! list is rendered by the settings window's package pane, which hands back
//! [`PackageRequest`]s that are executed here using the same worker channels:
//! all network and package work runs on worker threads, and the UI only ever
//! reads the snapshot the worker sent back.

use super::state::{GalleryLoad, GalleryState, PackageOp, PackageOpResult, ToastMsg};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use typ_edit_engine::registry::RegistryEntry;
use typ_edit_i18n::{t, t_with};

mod model;
mod render;

#[cfg(test)]
mod tests;

use crate::shell::settings_window::package_view::package_groups;
use crate::shell::settings_window::packages::PackageRequest;

/// The index cache is trusted for one day before a background re-fetch.
const INDEX_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A user interaction collected while rendering the gallery window, applied
/// after the modal is dismissed so the render closures never need the whole app.
pub(in crate::app) enum GalleryAction {
    Close,
    Refresh,
    UseTemplate { name: String, version: String },
}

impl GalleryState {
    /// Opens the gallery (from the command/menu or the new-project dialog) and
    /// starts the first load if needed.
    pub(in crate::app) fn open_window(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.ensure_loaded(ctx);
    }

    fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if self.index.is_none() && !self.loading {
            self.spawn_index_load(ctx);
        }
    }

    /// Fetches the `@preview` index on a worker thread, together with the local
    /// installed versions, and reports the result through the index channel.
    /// [`GalleryState::force_refresh`] bypasses the on-disk cache once, then
    /// clears itself.
    fn spawn_index_load(&mut self, ctx: &egui::Context) {
        if self.loading {
            return;
        }
        let Some(cache) = crate::settings::cache_file("registry-index.json") else {
            self.error = Some(t("gallery.offline").into_owned());
            return;
        };
        let force = std::mem::take(&mut self.force_refresh);
        self.loading = true;
        self.error = None;
        let tx = self.index_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = typ_edit_engine::registry::load_index(&cache, INDEX_MAX_AGE, force)
                .map(build_gallery_load);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Runs an install/uninstall on a worker thread. Only one operation runs at
    /// a time; `op_in_flight` disables the row buttons until it reports back.
    fn spawn_package_op(
        &mut self,
        name: String,
        version: String,
        kind: PackageOp,
        ctx: &egui::Context,
    ) {
        if self.op_in_flight {
            return;
        }
        self.op_in_flight = true;
        let tx = self.op_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let spec = format!("@preview/{name}:{version}");
            let result = match kind {
                PackageOp::Install => typ_edit_engine::registry::install_package(&spec),
                PackageOp::Uninstall => {
                    typ_edit_engine::registry::uninstall_package(&name, &version)
                }
            };
            let installed = typ_edit_engine::registry::installed_versions(&name);
            let _ = tx.send(PackageOpResult {
                name,
                kind,
                result,
                installed,
            });
            ctx.request_repaint();
        });
    }

    /// Drains the gallery's worker channels. Called from the frame logic step,
    /// so a finished load is applied before the window renders that frame.
    /// Returns the toasts the caller must raise, in order.
    pub(in crate::app) fn pump(&mut self, ctx: &egui::Context) -> Vec<ToastMsg> {
        let mut toasts = Vec::new();
        while let Ok(result) = self.index_rx.try_recv() {
            self.loading = false;
            match result {
                Ok(load) => {
                    self.groups = package_groups(&load.entries);
                    self.index = Some(load.entries);
                    self.installed = load.installed;
                    self.error = None;
                }
                Err(message) => self.error = Some(message),
            }
        }

        while let Ok(op) = self.op_rx.try_recv() {
            self.op_in_flight = false;
            self.installed.insert(op.name.clone(), op.installed);
            let message = match &op.result {
                Ok(()) => match op.kind {
                    PackageOp::Install => {
                        ToastMsg::success(t_with("toast.package_installed", &[("name", &op.name)]))
                    }
                    PackageOp::Uninstall => {
                        ToastMsg::success(t_with("toast.package_removed", &[("name", &op.name)]))
                    }
                },
                Err(err) => {
                    self.error = Some(err.clone());
                    ToastMsg::error(t_with("toast.package_op_failed", &[("err", err)]))
                }
            };
            self.status = Some((message.message.clone(), Instant::now()));
            toasts.push(message);
        }

        if self.loading || self.op_in_flight {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        toasts
    }

    /// Applies one collected action. Close/Refresh are handled here; a template
    /// choice is returned for the caller to preset the new-project dialog with.
    pub(in crate::app) fn apply_action(
        &mut self,
        action: GalleryAction,
        ctx: &egui::Context,
    ) -> Option<GalleryAction> {
        match action {
            GalleryAction::Close => {
                self.open = false;
                None
            }
            GalleryAction::Refresh => {
                self.force_refresh = true;
                self.spawn_index_load(ctx);
                None
            }
            template @ GalleryAction::UseTemplate { .. } => {
                self.open = false;
                Some(template)
            }
        }
    }

    /// Executes one request emitted by the settings package pane.
    pub(in crate::app) fn apply_package_request(
        &mut self,
        request: PackageRequest,
        ctx: &egui::Context,
    ) {
        match request {
            PackageRequest::EnsureLoaded => self.ensure_loaded(ctx),
            PackageRequest::Refresh => {
                self.force_refresh = true;
                self.spawn_index_load(ctx);
            }
            PackageRequest::Install { name, version } => {
                self.spawn_package_op(name, version, PackageOp::Install, ctx);
            }
            PackageRequest::Uninstall { name, version } => {
                self.spawn_package_op(name, version, PackageOp::Uninstall, ctx);
            }
        }
    }
}

/// Rebuilds the locally installed map for every unique name in the index.
fn build_gallery_load(entries: Vec<RegistryEntry>) -> GalleryLoad {
    let mut installed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in &entries {
        if seen.insert(entry.name.clone()) {
            let versions = typ_edit_engine::registry::installed_versions(&entry.name);
            installed.insert(entry.name.clone(), versions);
        }
    }
    GalleryLoad { entries, installed }
}
