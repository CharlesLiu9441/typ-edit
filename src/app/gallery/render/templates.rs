//! The template list: a card per registry entry that ships a template, with a
//! tag chip row for single-select category filtering.
//!
//! The tag row and every card are top-level tui trees opened on the gallery's
//! plain modal/scroll `Ui` (same pattern as the settings package list).

use super::super::{GalleryAction, model};
use crate::app::state::GalleryState;
use crate::shell::settings_window::package_view::format_updated;
use typ_edit_engine::registry::RegistryEntry;
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::list::toggle_chip;
use typ_edit_widgets::tui::{self, Tui, TuiBuilderLogic, taffy::AlignItems};

pub(super) fn body(
    ui: &mut egui::Ui,
    gallery: &mut GalleryState,
    actions: &mut Vec<GalleryAction>,
) {
    let index = gallery.index.as_deref().unwrap_or(&[]);
    let tags = model::available_tags(index);
    if !tags.is_empty() {
        let width = ui.available_width().max(120.0);
        let base = ui.id().with("typ_edit_gallery_tags");
        let mut style = tui::row_wrap(ui.spacing().item_spacing.x);
        tui::fill_width(&mut style);
        tui::tui(ui, base)
            .reserve_width(width)
            .style(style)
            .show(|tree| {
                if tree
                    .id(base.with("all"))
                    .ui(|ui| toggle_chip(ui, gallery.tag.is_none(), &t("gallery.filter_all")))
                    .clicked()
                {
                    gallery.tag = None;
                }
                for tag in &tags {
                    let selected = gallery.tag.as_deref() == Some(tag.as_str());
                    if tree
                        .id(base.with(("tag", tag)))
                        .ui(|ui| toggle_chip(ui, selected, tag))
                        .clicked()
                    {
                        gallery.tag = if selected { None } else { Some(tag.clone()) };
                    }
                }
            });
    }
    let templates: Vec<&RegistryEntry> =
        model::filter_entries(index, &gallery.query, gallery.tag.as_deref())
            .into_iter()
            .filter(|entry| entry.template.is_some())
            .collect();
    if templates.is_empty() {
        ui.label(egui::RichText::new(t("gallery.no_results")).weak());
        return;
    }
    // tui-whitelist: ScrollArea 容器开在顶层 Ui 上;每张卡片各自开顶层
    // tui(不嵌套在 tui 叶内),卡片内部不再有 egui 多子排布。
    egui::ScrollArea::vertical()
        .id_salt("gallery-templates-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for entry in templates {
                let installed = gallery
                    .installed
                    .get(&entry.name)
                    .is_some_and(|versions| !versions.is_empty());
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    card(ui, entry, installed, actions);
                });
            }
        });
}

/// One template card: a top-level tui column opened inside the group frame.
fn card(
    ui: &mut egui::Ui,
    entry: &RegistryEntry,
    installed: bool,
    actions: &mut Vec<GalleryAction>,
) {
    let width = ui.available_width().max(120.0);
    let base = egui::Id::new(("typ_edit_gallery_card", &entry.name, &entry.version));
    let gap = ui.spacing().item_spacing.y.max(2.0);
    let mut style = tui::column_gap(gap);
    tui::fill_width(&mut style);
    let mut use_template = false;
    tui::tui(ui, base)
        .reserve_width(width)
        .style(style)
        .show(|tree| {
            let mut header = tui::row_gap(tree.egui_ui().spacing().item_spacing.x);
            header.align_items = Some(AlignItems::Center);
            tree.style(header).id(base.with("header")).add(|row| {
                row.style(tui::label_leaf())
                    .id(base.with("name"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .label(
                        egui::RichText::new(format!("{} {}", entry.name, entry.version)).strong(),
                    );
                if installed {
                    row.style(tui::label_leaf())
                        .id(base.with("installed"))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .label(egui::RichText::new(t("gallery.installed")).weak().small());
                }
            });
            if !entry.authors.is_empty() {
                wrapped_label(
                    tree,
                    base.with("authors"),
                    egui::RichText::new(format!(
                        "{}: {}",
                        t("gallery.authors"),
                        entry.authors.join(", ")
                    ))
                    .weak()
                    .small(),
                );
            }
            if let Some(description) = &entry.description {
                wrapped_label(
                    tree,
                    base.with("description"),
                    egui::RichText::new(description),
                );
            }
            let mut footer = tui::row_gap(tree.egui_ui().spacing().item_spacing.x);
            footer.align_items = Some(AlignItems::Center);
            tree.style(footer).id(base.with("footer")).add(|row| {
                for (index, category) in entry.categories.iter().enumerate() {
                    row.id(base.with(("category", index))).ui(|ui| {
                        toggle_chip(ui, false, category);
                    });
                }
                let updated = format_updated(entry.updated_at);
                if !updated.is_empty() {
                    row.style(tui::label_leaf())
                        .id(base.with("updated"))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .label(
                            egui::RichText::new(format!("{} {}", t("gallery.updated"), updated))
                                .weak()
                                .small(),
                        );
                }
                row.style(tui::flexible())
                    .id(base.with("footer_spring"))
                    .add_empty();
                if row
                    .id(base.with("use"))
                    .ui(|ui| button(ui, &t("gallery.use_template"), ButtonVariant::Primary, true))
                    .clicked()
                {
                    use_template = true;
                }
            });
        });
    if use_template {
        actions.push(GalleryAction::UseTemplate {
            name: entry.name.clone(),
            version: entry.version.clone(),
        });
    }
}

/// A full-width leaf whose text wraps at the card width.
fn wrapped_label(tree: &mut Tui, id: egui::Id, text: egui::RichText) {
    tree.id(id).ui(|ui| {
        ui.add(egui::Label::new(text).wrap());
    });
}
