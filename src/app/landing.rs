//! The landing page shown while no workspace is open: the app title over three
//! rounded-square buttons (open, new project, settings).
//!
//! The page is a child of the window's single tui tree (its central band is a
//! flexible node), so it adds its children to the tree it is handed instead of
//! opening a nested `tui()` (which would reproduce the 0×0 host bug).

use super::state::UiState;
use crate::branding;
use crate::settings::Settings;
use typ_edit_i18n::t;
use typ_edit_widgets::button::ButtonVariant;
use typ_edit_widgets::menu::{menu_begin_sized, menu_item, menu_separator};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{AlignItems, Dimension, Size, Style},
};

mod recent;

use self::recent::RecentOutcome;

/// Side of one landing button (the rounded squares on the no-workspace page).
const LANDING_BTN: f32 = 60.0;
/// Side of the icon inside a landing button.
const LANDING_BTN_ICON: f32 = 26.0;
/// Gap between the two landing buttons.
const LANDING_BTN_GAP: f32 = 18.0;
/// Side of the hero app icon above the landing-page title, in logical pixels.
const LANDING_ICON: f32 = 112.0;
/// Gap between the hero app icon and the landing-page title.
const LANDING_ICON_GAP: f32 = 20.0;

/// The landing-page buttons. `BackToProject` is only shown while a workspace is
/// open behind the landing page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LandingButton {
    BackToProject,
    Open,
    NewProject,
    Settings,
}

/// App-only effects produced by rendering the landing page, applied by the
/// caller after the render returns.
#[derive(Default)]
pub(in crate::app) struct LandingOutcome {
    /// The 打开文件 menu row was chosen.
    pub(in crate::app) open_file: bool,
    /// The 打开文件夹 menu row was chosen.
    pub(in crate::app) open_folder: bool,
    /// The new-project button was chosen.
    pub(in crate::app) new_project: bool,
    /// The 加入局域网协作 row was chosen.
    pub(in crate::app) join_collab: bool,
    /// Effects of the 最近项目 card.
    pub(in crate::app) recent: RecentOutcome,
}

/// An exactly-sized row leaf (one landing button or the hero icon).
fn fixed_slot(side: f32) -> Style {
    let mut style = tui::fixed(side);
    style.size = Size {
        width: Dimension::length(side),
        height: Dimension::length(side),
    };
    style
}

impl UiState {
    /// The main interface shown while no workspace is open (or while the home
    /// button has parked an open workspace): the app title over rounded-square
    /// buttons (open, new project, settings, plus a back-to-project button when
    /// a workspace is open behind the page).
    ///
    /// `tree` is the window tree's flexible central band: the page adds one
    /// full-size, board-painted column to it and lays every element out as tui
    /// children (no nested `tui()`).
    pub(super) fn landing_pane(
        &mut self,
        tree: &mut Tui,
        has_root: bool,
        settings: &Settings,
    ) -> LandingOutcome {
        let mut out = LandingOutcome::default();
        let base = tree.current_id().with("landing_page");
        let board = crate::theme::board(tree.egui_ui());
        let mut page_style = tui::column();
        tui::fill(&mut page_style);
        page_style.align_items = Some(AlignItems::Center);
        tree.style(page_style).id(base).add_with_background_ui(
            |ui, container| {
                ui.painter().rect_filled(
                    container.full_container(),
                    egui::CornerRadius::same(0),
                    board,
                );
            },
            |tree, _| {
                let available_h = tree.egui_ui().available_height();
                // With recent projects the stack is taller than the viewport's
                // centre. Centre the estimated stack when it fits; when even
                // that is too short (large UI zoom or a very small window),
                // omit the recent card so the hero and primary actions stay
                // reachable.
                let has_recents = settings.workspace.recent_projects_enabled
                    && !settings.workspace.recent_projects.is_empty()
                    && available_h >= 565.0;
                let top_offset = if has_recents {
                    ((available_h - 549.0) * 0.5).clamp(16.0, 120.0)
                } else {
                    (available_h * 0.5 - 120.0 - (LANDING_ICON + LANDING_ICON_GAP) * 0.5).max(24.0)
                };
                tree.style(tui::fixed(top_offset))
                    .id(base.with("top_gap"))
                    .add_empty();

                tree.style(fixed_slot(LANDING_ICON))
                    .id(base.with("hero_icon"))
                    .ui(|ui| {
                        let texture = branding::icon_texture(
                            ui.ctx(),
                            &mut self.icon_large,
                            branding::ICON_256,
                        );
                        ui.add(
                            egui::Image::new(&texture)
                                .fit_to_exact_size(egui::Vec2::splat(LANDING_ICON)),
                        );
                    });
                tree.style(tui::fixed(LANDING_ICON_GAP))
                    .id(base.with("icon_gap"))
                    .add_empty();
                tree.style(tui::label_leaf())
                    .id(base.with("title"))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .label(egui::RichText::new("Typst Editor").size(30.0).strong());
                tree.style(tui::fixed(26.0))
                    .id(base.with("title_gap"))
                    .add_empty();

                let mut row_style = tui::row_gap(LANDING_BTN_GAP);
                row_style.align_self = Some(AlignItems::Center);
                tree.style(row_style).id(base.with("buttons")).add(|row| {
                    let with_project = has_root;
                    if with_project
                        && landing_button(row, LandingButton::BackToProject)
                            .on_hover_text(t("landing.back_to_project"))
                            .clicked()
                    {
                        self.landing = false;
                    }
                    let open = landing_button(row, LandingButton::Open)
                        .on_hover_text(t("landing.open_hint"));
                    #[cfg(test)]
                    super::ui_probe::record("landing.open", open.rect);
                    egui::Popup::menu(&open).show(|ui| {
                        let open_file = t("menu.file.open_file");
                        let open_folder = t("menu.file.open_folder");
                        let join_collab = t("collab.command.join");
                        menu_begin_sized(
                            ui,
                            &[
                                (open_folder.as_ref(), None),
                                (open_file.as_ref(), None),
                                (join_collab.as_ref(), None),
                            ],
                            false,
                        );
                        if menu_item(ui, &open_folder, None, true).clicked() {
                            ui.close();
                            out.open_folder = true;
                        }
                        if menu_item(ui, &open_file, None, true).clicked() {
                            ui.close();
                            out.open_file = true;
                        }
                        menu_separator(ui);
                        let join_item = menu_item(ui, &join_collab, None, true);
                        #[cfg(test)]
                        super::ui_probe::record("landing.join", join_item.rect);
                        if join_item.clicked() {
                            ui.close();
                            out.join_collab = true;
                        }
                    });
                    if landing_button(row, LandingButton::NewProject)
                        .on_hover_text(t("menu.file.new_project"))
                        .clicked()
                    {
                        out.new_project = true;
                    }
                    let settings = landing_button(row, LandingButton::Settings)
                        .on_hover_text(t("landing.settings"));
                    #[cfg(test)]
                    super::ui_probe::record("landing.settings", settings.rect);
                    if settings.clicked() {
                        self.settings_open = true;
                    }
                });
                tree.style(tui::fixed(10.0))
                    .id(base.with("shortcuts_gap"))
                    .add_empty();
                tree.style(tui::row_gap(10.0))
                    .id(base.with("help_links"))
                    .add(|tree| {
                        tree.id(base.with("typst_help")).ui(|ui| {
                            let response = typ_edit_widgets::button::button(
                                ui,
                                &t("landing.typst_help"),
                                ButtonVariant::Quiet,
                                true,
                            );
                            #[cfg(test)]
                            super::ui_probe::record("landing.typst_help", response.rect);
                            if response.clicked() {
                                self.typst_help_open = true;
                            }
                        });
                        tree.id(base.with("shortcuts")).ui(|ui| {
                            let response = typ_edit_widgets::button::button(
                                ui,
                                &t("landing.shortcuts"),
                                ButtonVariant::Quiet,
                                true,
                            );
                            #[cfg(test)]
                            super::ui_probe::record("landing.shortcuts", response.rect);
                            if response.clicked() {
                                self.shortcuts_open = true;
                            }
                        });
                    });

                if has_recents {
                    out.recent = self.recent_projects_section(tree, settings);
                }
            },
        );
        out
    }
}

/// Adds and paints one landing button: a rounded square with a Material icon.
fn landing_button(tree: &mut Tui, button: LandingButton) -> egui::Response {
    let id = tree.current_id().with(("landing_button", button as u8));
    tree.style(fixed_slot(LANDING_BTN))
        .id(id)
        .ui(|ui| paint_landing_button(ui, button))
}

fn paint_landing_button(ui: &mut egui::Ui, button: LandingButton) -> egui::Response {
    let theme = Theme::from_ui(ui);
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(LANDING_BTN), egui::Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let fill = if pressed {
        theme.colors.widget_active_fill
    } else if hovered {
        theme.colors.widget_hover_fill
    } else {
        theme.colors.card
    };
    // Hover is grayscale only; accent belongs to focus/active.
    let color = if hovered || pressed {
        theme.colors.text_strong
    } else {
        theme.colors.text
    };
    let stroke = if hovered {
        theme.colors.widget_hover_stroke
    } else {
        theme.colors.card_stroke
    };
    let radius = egui::CornerRadius::same(theme.radii.card);
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(theme.strokes.card, stroke),
        egui::StrokeKind::Inside,
    );
    if response.has_focus() || pressed {
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let icon = egui::Rect::from_center_size(
        rect.center() - egui::vec2(0.0, 5.0),
        egui::Vec2::splat(LANDING_BTN_ICON),
    );
    let label = match button {
        LandingButton::BackToProject => t("landing.back"),
        LandingButton::Open => t("landing.open"),
        LandingButton::NewProject => t("landing.new_project"),
        LandingButton::Settings => t("landing.settings_short"),
    };
    let label_color = if hovered || pressed {
        theme.colors.text_strong
    } else {
        theme.colors.text_muted
    };
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 5.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(11.0),
        label_color,
    );
    match button {
        LandingButton::BackToProject => typ_edit_widgets::icons::arrow_back(ui, icon, color),
        LandingButton::Open => {
            typ_edit_widgets::icons::folder(ui, icon, true, false, color);
            chevron_badge(ui, icon, fill, color);
        }
        LandingButton::NewProject => {
            typ_edit_widgets::icons::folder(ui, icon, false, false, color);
            plus_badge(ui, icon, fill, color);
        }
        LandingButton::Settings => typ_edit_widgets::icons::settings(ui, icon, color),
    }
    response
}

fn plus_badge(ui: &egui::Ui, icon: egui::Rect, fill: egui::Color32, color: egui::Color32) {
    let center = icon.right_top() + egui::vec2(3.0, -3.0);
    let radius = 6.5;
    ui.painter().circle_filled(center, radius, fill);
    let arm = radius * 0.5;
    let stroke = egui::Stroke::new(1.6, color);
    ui.painter().line_segment(
        [center - egui::vec2(arm, 0.0), center + egui::vec2(arm, 0.0)],
        stroke,
    );
    ui.painter().line_segment(
        [center - egui::vec2(0.0, arm), center + egui::vec2(0.0, arm)],
        stroke,
    );
}

/// A small down chevron in the same corner badge language as the plus badge:
/// it tells the user that the Open square opens a menu rather than a picker.
fn chevron_badge(ui: &egui::Ui, icon: egui::Rect, fill: egui::Color32, color: egui::Color32) {
    let center = icon.right_top() + egui::vec2(3.0, -3.0);
    let radius = 6.5;
    ui.painter().circle_filled(center, radius, fill);
    let arm = radius * 0.42;
    let stroke = egui::Stroke::new(1.6, color);
    ui.painter().line_segment(
        [
            center + egui::vec2(-arm, -arm * 0.35),
            center + egui::vec2(0.0, arm * 0.55),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(0.0, arm * 0.55),
            center + egui::vec2(arm, -arm * 0.35),
        ],
        stroke,
    );
}
