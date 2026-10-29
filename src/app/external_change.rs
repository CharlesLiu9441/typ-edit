//! Tab-level prompt for a file changed outside the editor.
//!
//! The disk copy is deliberately not applied while this modal is open: the
//! preview and compile pipeline continue to read the in-memory buffer until
//! the user explicitly chooses the disk version.

use super::TypEditApp;
use super::state::ExternalChangeChoice;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::separator::separator;

/// Width of the external-change prompt.
const DIALOG_WIDTH: f32 = 420.0;

impl TypEditApp {
    /// Renders the conflict owned by the active tab, if any.
    pub(super) fn external_change_dialog(&mut self, ctx: &egui::Context) {
        let active_id = self.session.tabs[self.session.active_tab].id;
        let Some(change) = self.external_changes.get(&active_id) else {
            return;
        };
        let path = change.path.clone();
        let tab_id = change.tab_id;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut choice = None;
        let opacity = self.settings.appearance.overlay_opacity.clamp(0.0, 1.0);
        let backdrop = egui::Color32::from_black_alpha((opacity * 255.0).round() as u8);

        egui::Modal::new(egui::Id::new("external-change-dialog"))
            .backdrop_color(backdrop)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(DIALOG_WIDTH);
                        ui.label(
                            egui::RichText::new(t("dialog.external_change.title"))
                                .size(15.0)
                                .strong(),
                        );
                        separator(ui);
                        ui.label(t_with("dialog.external_change.body", &[("name", &name)]));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(t("dialog.external_change.hint"))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let keep = button(
                                ui,
                                &t("dialog.external_change.keep"),
                                ButtonVariant::Secondary,
                                true,
                            );
                            #[cfg(test)]
                            crate::app::ui_probe::record("external_change.keep", keep.rect);
                            if keep.clicked() {
                                choice = Some(ExternalChangeChoice::KeepMemory);
                            }
                            let use_disk = button(
                                ui,
                                &t("dialog.external_change.use_disk"),
                                ButtonVariant::Primary,
                                true,
                            );
                            #[cfg(test)]
                            crate::app::ui_probe::record("external_change.use_disk", use_disk.rect);
                            if use_disk.clicked() {
                                choice = Some(ExternalChangeChoice::UseDisk);
                            }
                        });
                    });
            });

        if let Some(choice) = choice {
            self.resolve_external_change(tab_id, choice);
        }
    }

    /// Applies the user's decision for `tab_id`.
    pub(super) fn resolve_external_change(&mut self, tab_id: u64, choice: ExternalChangeChoice) {
        let Some(change) = self.external_changes.remove(&tab_id) else {
            return;
        };
        let Some(index) = self.session.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        match choice {
            ExternalChangeChoice::KeepMemory => {
                self.session.tabs[index].dirty = true;
                self.recovery.note_change();
                self.trigger_compile();
            }
            ExternalChangeChoice::UseDisk => {
                self.session.tabs[index].editor.set_text(change.disk_text);
                self.session.tabs[index].dirty = false;
                if index == self.session.active_tab {
                    self.on_tab_switched();
                }
                self.trigger_compile();
            }
        }
    }
}
