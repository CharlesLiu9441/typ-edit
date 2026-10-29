//! Nested vertical sub-pages inside a settings category.
//!
//! The rail width follows its longest label, and its accent thickness matches
//! the category rail. The rail/content split uses an explicit content width so
//! the page grid fills the remaining column instead of shrink-wrapping to its
//! intrinsic size.

use egui::Ui;
use typ_edit_i18n::{Lang, t, t_in};

/// Draws a vertical sub-tab rail and the selected page, returning whether the
/// page changed any setting.
///
/// `label_keys` are translation keys: the rail reserves the widest rendering
/// across both shipped languages, so switching language cannot move the
/// content column or leave a stripe gap.
pub(super) fn sub_tabs(
    ui: &mut Ui,
    id_salt: &str,
    label_keys: &[&str],
    mut content: impl FnMut(&mut Ui, usize) -> bool,
) -> bool {
    let labels: Vec<String> = label_keys.iter().map(|key| t(key).into_owned()).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let id = egui::Id::new(("typ_edit_settings_subtabs", id_salt));
    let mut selected: usize = ui.ctx().data(|data| data.get_temp(id)).unwrap_or(0);
    if selected >= labels.len() {
        selected = 0;
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut widest = 0.0_f32;
    for key in label_keys {
        for lang in [Lang::ZhCn, Lang::EnUs] {
            let galley = ui.painter().layout_no_wrap(
                t_in(lang, key).into_owned(),
                font.clone(),
                egui::Color32::PLACEHOLDER,
            );
            widest = widest.max(galley.size().x);
        }
    }
    let rail_w = (widest + 28.0).clamp(88.0, 240.0);
    let mut changed = false;
    ui.horizontal_top(|ui| {
        let rail_id = egui::Id::new(("typ_edit_settings_subtabs_rail", id_salt));
        ui.vertical(|ui| {
            ui.set_width(rail_w);
            if let Some(index) = typ_edit_widgets::tab_bar::vertical_tabs_thin(
                ui,
                ("settings-subtabs", id_salt),
                &label_refs,
                selected,
            ) {
                selected = index;
            }
            typ_edit_widgets::layout_probe::record(ui, rail_id, false);
        });
        ui.add_space(8.0);
        let content_w = ui.available_width();
        let content_id = egui::Id::new(("typ_edit_settings_subtabs_content", id_salt));
        ui.allocate_ui_with_layout(
            egui::vec2(content_w, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_width(content_w);
                changed = content(ui, selected);
                typ_edit_widgets::layout_probe::record(ui, content_id, true);
            },
        );
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, selected));
    changed
}
