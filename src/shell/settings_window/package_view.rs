//! The package view layer: the [`PackageGroup`] value type, its grouping and
//! filtering helpers, and the shared package-list rendering.
//!
//! The helpers are pure (no UI I/O beyond egui) so the settings pane renders
//! packages without depending on the app's gallery worker; `app/gallery` keeps
//! only the worker plumbing and reuses [`package_groups`] when a load lands.

use std::collections::BTreeMap;

use super::packages::{PackagePane, PackageRequest};
use typ_edit_engine::registry::RegistryEntry;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::list::option_row;
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::tui::{
    self, TuiBuilderLogic,
    taffy::{AlignItems, JustifyContent, LengthPercentage, Size, Style},
};

/// One package name with every published version (newest first).
pub(crate) struct PackageGroup {
    pub(crate) name: String,
    pub(crate) entries: Vec<RegistryEntry>,
}

impl PackageGroup {
    /// The newest published version of the package.
    pub(crate) fn latest(&self) -> &RegistryEntry {
        &self.entries[0]
    }
}

/// Groups the flat index by package name, newest version first within a group.
pub(crate) fn package_groups(entries: &[RegistryEntry]) -> Vec<PackageGroup> {
    let mut by_name: BTreeMap<String, Vec<RegistryEntry>> = BTreeMap::new();
    for entry in entries {
        by_name
            .entry(entry.name.clone())
            .or_default()
            .push(entry.clone());
    }
    by_name
        .into_iter()
        .map(|(name, mut versions)| {
            versions.sort_by(|a, b| compare_versions(&b.version, &a.version));
            PackageGroup {
                name,
                entries: versions,
            }
        })
        .collect()
}

/// Filters grouped packages by name or latest-version description/authors, and
/// hides every group that has no locally installed version when `installed_only`.
pub(crate) fn filter_groups<'a>(
    groups: &'a [PackageGroup],
    query: &str,
    installed_only: bool,
    installed: &BTreeMap<String, Vec<String>>,
) -> Vec<&'a PackageGroup> {
    let needle = query.trim().to_lowercase();
    groups
        .iter()
        .filter(|group| {
            needle.is_empty()
                || group.name.to_lowercase().contains(&needle)
                || entry_matches(group.latest(), &needle)
        })
        .filter(|group| {
            !installed_only
                || installed
                    .get(&group.name)
                    .is_some_and(|versions| !versions.is_empty())
        })
        .collect()
}

/// Whether a registry entry's name/description/authors/categories contain
/// `needle` (already lowercased).
pub(crate) fn entry_matches(entry: &RegistryEntry, needle: &str) -> bool {
    entry.name.to_lowercase().contains(needle)
        || entry
            .description
            .as_deref()
            .is_some_and(|text| text.to_lowercase().contains(needle))
        || entry
            .authors
            .iter()
            .any(|author| author.to_lowercase().contains(needle))
        || entry
            .categories
            .iter()
            .any(|category| category.to_lowercase().contains(needle))
}

/// Formats a Unix timestamp as `YYYY-MM-DD` (UTC); `None` yields an empty string.
pub(crate) fn format_updated(timestamp: Option<u64>) -> String {
    let Some(seconds) = timestamp else {
        return String::new();
    };
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Orders two `major.minor.patch` version strings newest-first. Strings that do
/// not parse fall back to a plain descending lexical comparison.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Days since 1970-01-01 to a proleptic-Gregorian `(year, month, day)`, using
/// Howard Hinnant's inverse `days_from_civil` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Horizontal, vertically centred tui row used for the package list's own rows.
fn row_style(item_spacing: f32) -> Style {
    let mut style = tui::row();
    style.align_items = Some(AlignItems::Center);
    style.gap = Size {
        width: LengthPercentage::length(item_spacing),
        height: LengthPercentage::length(0.0),
    };
    style
}

/// Like [`row_style`], with the last item pushed to the row's right edge.
fn split_row_style(item_spacing: f32) -> Style {
    let mut style = row_style(item_spacing);
    style.justify_content = Some(JustifyContent::SpaceBetween);
    style
}

/// The shared package list: one group per package with its version selector,
/// install button, and locally installed versions with per-version uninstall.
/// It is rendered inside the settings package pane and emits [`PackageRequest`]s
/// rather than touching any worker directly.
pub(crate) fn package_list(
    ui: &mut egui::Ui,
    pane: &mut PackagePane<'_>,
    requests: &mut Vec<PackageRequest>,
) {
    let filtered = filter_groups(
        pane.groups,
        pane.query,
        *pane.installed_only,
        pane.installed,
    );
    if filtered.is_empty() {
        ui.label(egui::RichText::new(t("gallery.no_results")).weak());
        return;
    }
    let busy = pane.op_in_flight;
    for group in filtered {
        let name = group.name.clone();
        let latest = group.latest();
        let installed = pane.installed.get(&name).cloned().unwrap_or_default();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let width = ui.available_width();
            let item_spacing = ui.spacing().item_spacing.x;
            tui::tui(ui, egui::Id::new(("typ_edit_package_title", &name)))
                .reserve_width(width)
                .style(row_style(item_spacing))
                .show(|tree| {
                    tree.wrap_mode(egui::TextWrapMode::Extend)
                        .label(egui::RichText::new(&name).strong());
                    tree.wrap_mode(egui::TextWrapMode::Extend).label(
                        egui::RichText::new(format!("{} {}", t("gallery.version"), latest.version))
                            .weak()
                            .small(),
                    );
                    if !installed.is_empty() {
                        tree.wrap_mode(egui::TextWrapMode::Extend).label(
                            egui::RichText::new(format!(
                                "{}: {}",
                                t("gallery.installed"),
                                installed.join(", ")
                            ))
                            .weak()
                            .small(),
                        );
                    }
                });
            if let Some(description) = &latest.description {
                ui.label(description);
            }
            let updated = format_updated(latest.updated_at);
            if !updated.is_empty() {
                ui.label(
                    egui::RichText::new(format!("{} {}", t("gallery.updated"), updated))
                        .weak()
                        .small(),
                );
            }

            let mut install = false;
            let width = ui.available_width();
            let item_spacing = ui.spacing().item_spacing.x;
            tui::tui(ui, egui::Id::new(("typ_edit_package_install", &name)))
                .reserve_width(width)
                .style(row_style(item_spacing))
                .show(|tree| {
                    tree.ui(|ui| {
                        let selected = pane
                            .selected_versions
                            .get(&name)
                            .copied()
                            .unwrap_or(0)
                            .min(group.entries.len() - 1);
                        crate::theme::combo_box_scope(ui, |ui| {
                            egui::ComboBox::from_id_salt(("settings-package-version", &name))
                                .selected_text(group.entries[selected].version.clone())
                                .show_ui(ui, |ui| {
                                    for (index, entry) in group.entries.iter().enumerate() {
                                        if option_row(ui, index == selected, &entry.version)
                                            .clicked()
                                        {
                                            pane.selected_versions.insert(name.clone(), index);
                                            ui.close();
                                        }
                                    }
                                });
                        });
                    });
                    tree.ui(|ui| {
                        if button(ui, &t("gallery.install"), ButtonVariant::Secondary, !busy)
                            .clicked()
                        {
                            install = true;
                        }
                    });
                });
            if install {
                let selected = pane
                    .selected_versions
                    .get(&name)
                    .copied()
                    .unwrap_or(0)
                    .min(group.entries.len() - 1);
                requests.push(PackageRequest::Install {
                    name: name.clone(),
                    version: group.entries[selected].version.clone(),
                });
            }

            if !installed.is_empty() {
                separator(ui);
                for version in installed {
                    let width = ui.available_width();
                    let item_spacing = ui.spacing().item_spacing.x;
                    tui::tui(
                        ui,
                        egui::Id::new(("typ_edit_package_uninstall", &name, &version)),
                    )
                    .reserve_width(width)
                    .style(split_row_style(item_spacing))
                    .show(|tree| {
                        tree.wrap_mode(egui::TextWrapMode::Extend)
                            .label(version.clone());
                        tree.ui(|ui| {
                            if button(ui, &t("gallery.uninstall"), ButtonVariant::Danger, !busy)
                                .clicked()
                            {
                                requests.push(PackageRequest::Uninstall {
                                    name: name.clone(),
                                    version: version.clone(),
                                });
                            }
                        });
                    });
                }
            }
        });
    }
}
