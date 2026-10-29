//! Cache management page: list and clear app-owned temp/cache directories.

use egui::RichText;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::theme::Theme;

use crate::cache::{CacheEntry, CacheKind};

fn entries_id() -> egui::Id {
    egui::Id::new("typ_edit_cache_inventory")
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Keep long cache paths from widening the settings modal; the full path is
/// still available in the row's hover text.
fn shortened(text: &str) -> String {
    const MAX: usize = 72;
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(44).collect();
    out.push('…');
    out.extend(text.chars().skip(text.chars().count() - 25));
    out
}

/// Cache management page: inventory, per-entry clear and clear-all.
pub(super) fn cache_ui(ui: &mut egui::Ui) -> bool {
    let page_w = ui.available_width();
    ui.set_width(page_w);
    ui.set_max_width(page_w);
    let mut entries: Vec<CacheEntry> = ui
        .ctx()
        .data(|data| data.get_temp(entries_id()))
        .unwrap_or_default();
    let mut refresh: bool = ui
        .ctx()
        .data(|data| data.get_temp(entries_id().with("refresh")))
        .unwrap_or(true);
    if refresh {
        entries = crate::cache::entries();
        refresh = false;
        ui.ctx().data_mut(|data| {
            data.insert_temp(entries_id(), entries.clone());
            data.insert_temp(entries_id().with("refresh"), refresh);
        });
    }

    let mut error: Option<String> = None;
    let theme = Theme::from_ui(ui);
    let header_w = ui.available_width();
    let refresh_w = 80.0;
    ui.horizontal(|ui| {
        let desc_w = (header_w - refresh_w - 8.0).max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(desc_w, ui.spacing().interact_size.y * 2.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(desc_w);
                ui.set_max_width(desc_w);
                ui.add(
                    egui::Label::new(
                        RichText::new(t("settings.cache.desc"))
                            .small()
                            .color(theme.colors.text_muted),
                    )
                    .wrap(),
                );
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::vec2(refresh_w, ui.spacing().interact_size.y),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if button(ui, &t("settings.cache.refresh"), ButtonVariant::Quiet, true).clicked() {
                    refresh = true;
                }
            },
        );
    });
    separator(ui);

    if entries.is_empty() {
        ui.label(RichText::new(t("settings.cache.empty")).color(theme.colors.text_muted));
    } else {
        for entry in &entries {
            ui.horizontal(|ui| {
                let kind = match entry.kind {
                    CacheKind::Registry => t("settings.cache.kind.registry"),
                    CacheKind::Collab => t("settings.cache.kind.collab"),
                    CacheKind::Temp => t("settings.cache.kind.temp"),
                };
                let width = page_w;
                let right_w = 128.0;
                ui.allocate_ui_with_layout(
                    egui::vec2((width - right_w - 8.0).max(0.0), 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_max_width((width - right_w - 8.0).max(0.0));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(kind).small().color(theme.colors.text_muted));
                            ui.add(
                                egui::Label::new(RichText::new(shortened(&entry.label)).strong())
                                    .truncate(),
                            );
                        });
                    },
                );
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, ui.spacing().interact_size.y),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if button(ui, &t("settings.cache.clear"), ButtonVariant::Quiet, true)
                            .clicked()
                        {
                            match crate::cache::clear(entry) {
                                Ok(()) => refresh = true,
                                Err(err) => error = Some(err.to_string()),
                            }
                        }
                        ui.label(
                            RichText::new(format_bytes(entry.bytes))
                                .small()
                                .color(theme.colors.text_muted),
                        );
                    },
                );
            });
            let path = entry.path.display().to_string();
            ui.add_sized(
                [page_w, ui.spacing().interact_size.y],
                egui::Label::new(
                    RichText::new(shortened(&path))
                        .small()
                        .color(theme.colors.text_muted),
                )
                .truncate(),
            )
            .on_hover_text(path);
            separator(ui);
        }

        let confirm_id = entries_id().with("confirm");
        let mut confirming: bool = ui
            .ctx()
            .data(|data| data.get_temp(confirm_id))
            .unwrap_or(false);
        if confirming {
            ui.label(RichText::new(t("settings.cache.confirm")).color(theme.colors.warn_text));
            ui.horizontal(|ui| {
                if button(ui, &t("settings.ok"), ButtonVariant::Primary, true).clicked() {
                    for entry in &entries {
                        if let Err(err) = crate::cache::clear(entry) {
                            error = Some(err.to_string());
                        }
                    }
                    refresh = true;
                    confirming = false;
                }
                if button(ui, &t("settings.cancel"), ButtonVariant::Secondary, true).clicked() {
                    confirming = false;
                }
            });
        } else if button(
            ui,
            &t("settings.cache.clear_all"),
            ButtonVariant::Quiet,
            true,
        )
        .clicked()
        {
            confirming = true;
        }
        ui.ctx().data_mut(|data| {
            data.insert_temp(confirm_id, confirming);
        });
    }

    if let Some(error) = error {
        ui.colored_label(theme.colors.error_text, error);
    }
    ui.ctx().data_mut(|data| {
        data.insert_temp(entries_id(), entries);
        data.insert_temp(entries_id().with("refresh"), refresh);
    });
    false
}
