//! Semantic colors for the app chrome, backed by the extracted design system
//! in [`typ_edit_widgets::theme`].
//!
//! Every function here reads its value from [`ColorTheme`]; only the dark/light
//! mode flag comes from egui. [`install_card_style`] additionally installs the
//! card surfaces and widget state language into [`egui::Visuals`], so stock
//! egui controls share the same focus/active/hover tokens as hand-drawn widgets.

use egui::{Color32, Ui};

pub use typ_edit_widgets::theme::SemanticKind;
use typ_edit_widgets::theme::{Theme, install_widget_visuals};

/// The active theme of `ui`.
fn theme(ui: &Ui) -> &'static Theme {
    Theme::from_ui(ui)
}

/// Error / destructive-action text color.
pub fn error(ui: &Ui) -> Color32 {
    theme(ui).colors.error
}

/// Enabled secondary text (status values, pane titles, hints).
pub fn text_muted(ui: &Ui) -> Color32 {
    theme(ui).colors.text_muted
}

/// Error / destructive-action text color with WCAG AA contrast.
pub fn error_text(ui: &Ui) -> Color32 {
    theme(ui).colors.error_text
}

/// Warning text color.
pub fn warn(ui: &Ui) -> Color32 {
    theme(ui).colors.warn
}

/// Foreground/outline color of one semantic family.
pub fn semantic(ui: &Ui, kind: SemanticKind) -> Color32 {
    kind.fg(&theme(ui).colors)
}

/// High-transparency surface tint of one semantic family.
pub fn semantic_bg(ui: &Ui, kind: SemanticKind) -> Color32 {
    kind.bg(&theme(ui).colors)
}

/// Opaque semantic surface for floating feedback containers such as toasts.
pub fn semantic_surface(ui: &Ui, kind: SemanticKind) -> Color32 {
    kind.surface(&theme(ui).colors)
}

/// The default 1px control border.
pub fn border_default(ui: &Ui) -> Color32 {
    theme(ui).colors.widget_stroke
}

/// Gives a stock ComboBox trigger the input-style resting state:
/// `fill-neutral` plus `border-default`. Hover/active/open states are already
/// installed by [`install_card_style`] and therefore stay grayscale/accent.
fn style_combo_box(ui: &mut Ui) {
    let theme = theme(ui);
    let inactive = &mut ui.visuals_mut().widgets.inactive;
    inactive.bg_fill = theme.colors.widget_inactive_fill;
    inactive.weak_bg_fill = theme.colors.widget_inactive_fill;
    inactive.bg_stroke = egui::Stroke::new(theme.strokes.card, theme.colors.widget_stroke);
}

/// Runs a stock ComboBox inside a style scope so only the ComboBox receives
/// the input-style resting state.
pub fn combo_box_scope<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        style_combo_box(ui);
        add(ui)
    })
    .inner
}

/// The raised/floating surface (menus, popups, dialogs).
pub fn raised(ui: &Ui) -> Color32 {
    theme(ui).colors.raised
}

/// Fill for a preview page whose raster has not arrived yet: a neutral surface
/// clearly distinct from the card surface in both themes.
pub fn placeholder(ui: &Ui) -> Color32 {
    theme(ui).colors.placeholder
}

/// Color of the fading "reveal" ring in the preview; the app accent.
pub fn reveal(ui: &Ui) -> Color32 {
    theme(ui).colors.accent
}

/// Shared backdrop behind the cards: window background / panel fill.
pub fn board(ui: &Ui) -> Color32 {
    theme(ui).colors.board
}

/// Surface of the three column cards and the status-bar card.
pub fn card(ui: &Ui) -> Color32 {
    theme(ui).colors.card
}

/// 1px outline of a card, weaker than [`hairline`].
pub fn card_stroke(ui: &Ui) -> Color32 {
    theme(ui).colors.card_stroke
}

/// Surface of the custom title/menu strip: the same card surface as the
/// panels so the strip reads as part of the workspace chrome.
pub fn title_bar(ui: &Ui) -> Color32 {
    theme(ui).colors.card
}

/// Structural line inside a card (gutter, tab underline, error-bar top).
pub fn hairline(ui: &Ui) -> Color32 {
    theme(ui).colors.hairline
}

/// Installs the card-style surfaces, radii and strokes into an [`egui::Visuals`].
///
/// Call it for both the dark and the light style at startup
/// (`ctx.all_styles_mut(...)`); `set_theme` only switches which installed style is
/// active, so a later theme change keeps the patched values.
pub fn install_card_style(visuals: &mut egui::Visuals) {
    let theme = Theme::of(visuals.dark_mode);

    visuals.panel_fill = theme.colors.board;
    visuals.window_fill = theme.colors.raised;

    visuals.window_corner_radius = egui::CornerRadius::same(theme.radii.window);
    visuals.menu_corner_radius = egui::CornerRadius::same(theme.radii.menu);

    install_widget_visuals(visuals, theme);
}

/// Installs the UI text scale into an [`egui::Style`].
///
/// `ui_font_size` is the body size in points; button/body share it, small text
/// follows three points below (clamped at 9) and headings four points above.
/// Fixed-size chrome (the 28 pt title strip, tab row) is deliberately not
/// scaled here: that is what the separate UI scale setting is for.
pub fn install_text_styles(style: &mut egui::Style, ui_font_size: f32) {
    let body = ui_font_size.clamp(8.0, 24.0);
    let small = (body - 3.0).max(9.0);
    let mono = (body - 1.0).max(9.0);
    let heading = body + 4.0;
    style.text_styles = [
        (egui::TextStyle::Small, egui::FontId::proportional(small)),
        (egui::TextStyle::Body, egui::FontId::proportional(body)),
        (egui::TextStyle::Button, egui::FontId::proportional(body)),
        (
            egui::TextStyle::Heading,
            egui::FontId::proportional(heading),
        ),
        (egui::TextStyle::Monospace, egui::FontId::monospace(mono)),
    ]
    .into();
}

/// Maps a persisted theme preference to egui's [`egui::ThemePreference`].
pub fn preference(pref: crate::settings::ThemePref) -> egui::ThemePreference {
    match pref {
        crate::settings::ThemePref::System => egui::ThemePreference::System,
        crate::settings::ThemePref::Light => egui::ThemePreference::Light,
        crate::settings::ThemePref::Dark => egui::ThemePreference::Dark,
    }
}
