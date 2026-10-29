//! A numeric field with a preset dropdown.
//!
//! The user can type any value in the range or pick a common preset from the
//! arrow menu. It replaces `DragValue` for UI/editor font sizes, where dragging
//! is imprecise and awkward.

use egui::{TextEdit, Ui};

use crate::button::icon_button_painted;
use crate::icons;
use crate::list::option_row;

/// Shows `value` as an editable numeric field with a preset dropdown.
///
/// Returns `true` when the value changed. `width` includes the text field, the
/// suffix and the arrow button.
pub fn editable_number(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    value: &mut f32,
    presets: &[f32],
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    width: f32,
) -> bool {
    let base = ui.id().with(id_salt);
    let draft_id = base.with("draft");
    let mut changed = false;

    let mut draft = ui
        .data(|data| data.get_temp::<String>(draft_id))
        .unwrap_or_else(|| format_value(*value));
    ui.horizontal(|ui| {
        let text_width = (width - 42.0).max(36.0);
        let response = ui.add(TextEdit::singleline(&mut draft).desired_width(text_width));
        if response.changed()
            && let Ok(parsed) = draft.trim().parse::<f32>()
        {
            let clamped = parsed.clamp(*range.start(), *range.end());
            if (clamped - *value).abs() > f32::EPSILON {
                *value = clamped;
                changed = true;
            }
        }
        if response.lost_focus() || !response.has_focus() {
            // Normalise the visible text to the stored value; this also repairs
            // an invalid/out-of-range draft when focus leaves the field.
            draft = format_value(*value);
            ui.data_mut(|data| data.insert_temp(draft_id, draft.clone()));
        } else {
            ui.data_mut(|data| data.insert_temp(draft_id, draft.clone()));
        }
        ui.label(suffix);
        let arrow = icon_button_painted(ui, 18.0, true, |ui, rect, color| {
            icons::chevron(ui, rect, true, color)
        });
        egui::Popup::menu(&arrow).show(|ui| {
            for preset in presets {
                let selected = (*preset - *value).abs() < f32::EPSILON;
                if option_row(ui, selected, &format!("{preset:.0} {suffix}")).clicked() {
                    *value = *preset;
                    draft = format_value(*preset);
                    changed = true;
                    ui.close();
                }
            }
        });
    });
    changed
}

fn format_value(value: f32) -> String {
    if (value.fract()).abs() < 0.01 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}
