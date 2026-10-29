//! A searchable single-select dropdown.
//!
//! The trigger shows the current value; the popup starts with a search field
//! and filters the option list as the user types. Font family pickers use it so
//! the separate "filter" row disappears.

use egui::{RichText, TextEdit, Ui};

use crate::list::option_row;

/// Shows `current` as a searchable dropdown over `options`.
///
/// Returns `true` when the selection changed. `placeholder` is the search
/// field hint, `empty_text` is shown when no option matches, and `width` is the
/// popup width in points.
pub fn search_combo(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    current: &mut String,
    options: &[String],
    placeholder: &str,
    empty_text: &str,
    width: f32,
) -> bool {
    let base = ui.id().with(id_salt);
    let query_id = base.with("query");
    let mut changed = false;
    let popup_width = width.max(160.0);

    egui::ComboBox::from_id_salt(base)
        .selected_text(current.clone())
        .width(popup_width)
        .show_ui(ui, |ui| {
            let mut query = ui
                .data(|data| data.get_temp::<String>(query_id))
                .unwrap_or_default();
            let search = ui.add(
                TextEdit::singleline(&mut query)
                    .hint_text(placeholder)
                    .desired_width(popup_width - 16.0),
            );
            if !search.has_focus() {
                search.request_focus();
            }
            if search.changed() {
                ui.data_mut(|data| data.insert_temp(query_id, query.clone()));
            }

            let needle = query.trim().to_lowercase();
            let mut visible = 0usize;
            egui::ScrollArea::vertical()
                .id_salt(base.with("results"))
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for option in options {
                        if !needle.is_empty() && !option.to_lowercase().contains(&needle) {
                            continue;
                        }
                        visible += 1;
                        let selected = option.as_str() == current.as_str();
                        if option_row(ui, selected, option.as_str()).clicked() {
                            if !selected {
                                *current = option.clone();
                                changed = true;
                            }
                            ui.close();
                        }
                    }
                });
            if visible == 0 {
                ui.label(RichText::new(empty_text).weak());
            }
        });
    changed
}
