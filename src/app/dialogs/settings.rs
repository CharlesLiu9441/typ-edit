use crate::app::state::{GalleryState, UiState};
use crate::settings::Settings;
use crate::shell::settings_window::packages::{PackagePane, PackageRequest};
use typ_edit_engine::fonts::FontCatalog;

/// App-only effects produced by rendering the settings window, applied by the
/// caller after the render returns.
#[derive(Default)]
pub(in crate::app) struct SettingsOutcome {
    /// Package operations the 包管理 pane requested, in order.
    pub(in crate::app) requests: Vec<PackageRequest>,
    /// Whether the user changed any setting this frame.
    pub(in crate::app) changed: bool,
    /// Whether `changed` also flipped `git_enabled` (caller refreshes git).
    pub(in crate::app) git_enabled_changed: bool,
}

impl UiState {
    /// Renders the settings window and returns its changes. Package actions the
    /// 包管理 pane requested are handed back for the caller to execute through
    /// the gallery's worker plumbing.
    pub(in crate::app) fn settings_window_ui(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        fonts: &FontCatalog,
        gallery: &mut GalleryState,
    ) -> SettingsOutcome {
        if !self.settings_open {
            return SettingsOutcome::default();
        }
        let ctx = ui.ctx().clone();
        let mut requests: Vec<PackageRequest> = Vec::new();
        let git_before = settings.git.git_enabled;
        let changed = {
            let mut pane = PackagePane {
                groups: &gallery.groups,
                installed: &gallery.installed,
                selected_versions: &mut gallery.selected_versions,
                query: &mut gallery.query,
                installed_only: &mut gallery.installed_only,
                loaded: gallery.index.is_some(),
                loading: gallery.loading,
                op_in_flight: gallery.op_in_flight,
                error: gallery.error.as_deref(),
                status: gallery.status.as_ref().map(|(message, _)| message.as_str()),
            };
            crate::shell::settings_window::show(
                &ctx,
                &mut self.settings_open,
                settings,
                fonts,
                &mut pane,
                &mut requests,
            )
        };
        SettingsOutcome {
            requests,
            changed,
            git_enabled_changed: changed && settings.git.git_enabled != git_before,
        }
    }
}
