//! The 包管理 settings category: the search box, the 只显示已安装 filter, and
//! the shared package list (name, versions, install/uninstall). Every action is
//! recorded as a [`PackageRequest`]; the app layer executes them with the
//! existing gallery worker plumbing, so the pane itself never blocks.

use std::collections::BTreeMap;

use egui::Ui;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::checkbox::checkbox;
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::text_input::form_input;
use typ_edit_widgets::tui::{self, TuiBuilderLogic, taffy::AlignItems};

use crate::shell::settings_window::package_view::PackageGroup;

/// One action the package pane asks the app layer to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackageRequest {
    /// Load the registry index if nothing is loaded yet.
    EnsureLoaded,
    /// Bypass the on-disk cache and re-fetch the registry index.
    Refresh,
    Install {
        name: String,
        version: String,
    },
    Uninstall {
        name: String,
        version: String,
    },
}

/// Borrowed view of the app's package state, rendered by the settings pane.
pub(crate) struct PackagePane<'a> {
    pub(crate) groups: &'a [PackageGroup],
    pub(crate) installed: &'a BTreeMap<String, Vec<String>>,
    pub(crate) selected_versions: &'a mut BTreeMap<String, usize>,
    pub(crate) query: &'a mut String,
    pub(crate) installed_only: &'a mut bool,
    pub(crate) loaded: bool,
    pub(crate) loading: bool,
    pub(crate) op_in_flight: bool,
    pub(crate) error: Option<&'a str>,
    pub(crate) status: Option<&'a str>,
}

/// Renders the package-manager pane. Never reports a settings change: the pane
/// only mutates transient package state, which is not persisted.
pub(super) fn packages_ui(
    ui: &mut Ui,
    pane: &mut PackagePane<'_>,
    requests: &mut Vec<PackageRequest>,
) -> bool {
    if !pane.loaded && !pane.loading {
        requests.push(PackageRequest::EnsureLoaded);
    }
    let loading = pane.loading;
    let width = ui.available_width();
    let mut refresh_row = tui::row();
    refresh_row.align_items = Some(AlignItems::Center);
    tui::tui(ui, egui::Id::new("typ_edit_settings_packages_refresh"))
        .reserve_width(width)
        .style(refresh_row)
        .show(|tree| {
            tree.ui(|ui| {
                if button(ui, &t("gallery.refresh"), ButtonVariant::Quiet, !loading).clicked() {
                    requests.push(PackageRequest::Refresh);
                }
            });
        });
    let width = ui.available_width().max(120.0);
    form_input(ui, pane.query, &t("gallery.search"), width);
    let _ = checkbox(
        ui,
        pane.installed_only,
        &t("packages.filter_installed"),
        true,
    );
    if pane.loading {
        ui.label(egui::RichText::new(t("gallery.loading")).weak());
    } else if pane.op_in_flight {
        ui.label(egui::RichText::new(t("gallery.working")).weak());
    }
    if let Some(error) = pane.error {
        ui.colored_label(
            crate::theme::error(ui),
            typ_edit_i18n::t_with("gallery.error", &[("err", error)]),
        );
    }
    if let Some(status) = pane.status {
        ui.label(egui::RichText::new(status).weak().small());
    }
    separator(ui);
    super::package_view::package_list(ui, pane, requests);
    false
}
