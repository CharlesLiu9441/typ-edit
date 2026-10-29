//! The app-wide design system: layout metrics, corner radii, stroke widths and
//! the light/dark color palettes.
//!
//! [`ColorTheme::LIGHT`] / [`ColorTheme::DARK`] are the single source of truth
//! for app colors. The palette started from egui's defaults, but the state
//! tokens now follow the token spec intentionally (for example active fills
//! keep the hover weight and active borders are always the accent). Stock egui
//! controls are brought back onto the same language by
//! [`install_widget_visuals`]; hand-drawn widgets read [`ColorTheme`] directly.
//!
//! Layout metrics are shared by both themes; only [`ColorTheme`] varies.

use egui::{Color32, CornerRadius, Stroke};

use crate::tokens;

/// `Color32::from_white_alpha`, but `const` (egui's is not usable in constants).
const fn white_alpha(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(a, a, a, a)
}

/// An unmultiplied semantic tint, kept `const` for the palette constants.
const fn tint(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const(r, g, b, a)
}

/// The four semantic feedback families used by the app chrome.
///
/// `Success` is an extra family beyond the Info/Warning/Error trio in the
/// design spec; it is used for positive completion feedback (saved, installed,
/// connected) and is intentionally kept separate from the accent color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticKind {
    Info,
    Success,
    Warning,
    Error,
}

impl SemanticKind {
    /// Every semantic family, in a stable order (useful for token tests and
    /// exhaustive UI audits).
    pub const ALL: [Self; 4] = [Self::Info, Self::Success, Self::Warning, Self::Error];

    /// The outline/icon/text color of this family.
    pub fn fg(self, colors: &ColorTheme) -> Color32 {
        match self {
            Self::Info => colors.info,
            Self::Success => colors.success,
            Self::Warning => colors.warn,
            Self::Error => colors.error,
        }
    }

    /// The high-transparency surface tint of this family.
    pub fn bg(self, colors: &ColorTheme) -> Color32 {
        match self {
            Self::Info => colors.info_bg,
            Self::Success => colors.success_bg,
            Self::Warning => colors.warn_bg,
            Self::Error => colors.error_bg,
        }
    }

    /// The semantic tint composited over the floating surface, returned opaque.
    /// Use this for containers (toasts) that must not show content through.
    pub fn surface(self, colors: &ColorTheme) -> Color32 {
        composite_over(self.bg(colors), colors.raised)
    }
}

/// Composites a premultiplied `Color32` over an opaque base color.
fn composite_over(fg: Color32, base: Color32) -> Color32 {
    let alpha = f32::from(fg.a()) / 255.0;
    let blend = |foreground: u8, background: u8| {
        (f32::from(foreground) + f32::from(background) * (1.0 - alpha)).round() as u8
    };
    Color32::from_rgb(
        blend(fg.r(), base.r()),
        blend(fg.g(), base.g()),
        blend(fg.b(), base.b()),
    )
}

/// The complete design system of the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Layout metrics (gaps, paddings, heights).
    pub spacing: Spacing,
    /// Corner radii.
    pub radii: Radii,
    /// Border/underline stroke widths.
    pub strokes: Strokes,
    /// Color palette.
    pub colors: ColorTheme,
}

impl Theme {
    /// Light theme.
    pub const LIGHT: Self = Self {
        spacing: Spacing::DEFAULT,
        radii: Radii::DEFAULT,
        strokes: Strokes::DEFAULT,
        colors: ColorTheme::LIGHT,
    };

    /// Dark theme.
    pub const DARK: Self = Self {
        spacing: Spacing::DEFAULT,
        radii: Radii::DEFAULT,
        strokes: Strokes::DEFAULT,
        colors: ColorTheme::DARK,
    };

    /// The theme for `dark`.
    pub fn of(dark: bool) -> &'static Self {
        if dark { &Self::DARK } else { &Self::LIGHT }
    }

    /// The theme matching `ui`'s active dark/light mode.
    ///
    /// Only the mode flag is read from egui; every value comes from the
    /// constants above.
    pub fn from_ui(ui: &egui::Ui) -> &'static Self {
        Self::of(ui.visuals().dark_mode)
    }
}

/// Layout metrics shared by the app shell and its widgets, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    /// Gap between cards, cards and the window edge, panes and the status bar.
    pub gap: f32,
    /// Horizontal inner padding of a card body.
    pub card_pad_x: f32,
    /// Vertical inner padding of a card body.
    pub card_pad_y: f32,
    /// Height of a pane header row (`项目 / 编辑 / 预览`).
    pub header_h: f32,
    /// Width of a collapsed pane strip.
    pub strip_w: f32,
    /// Minimum width of an open pane.
    pub min_open_w: f32,
    /// Width of the drag hit zone between two panes.
    pub splitter_w: f32,
    /// Height of the custom title/menu strip.
    pub title_bar_h: f32,
    /// Width of one window button (minimize / maximize / close).
    pub window_btn_w: f32,
    /// Side of a title-bar icon.
    pub title_icon: f32,
    /// Gap between a title-bar icon and its neighbor.
    pub title_icon_gap: f32,
    /// Inner horizontal padding of the status-bar card.
    pub status_pad_x: f32,
    /// Inner vertical padding of the status-bar card.
    pub status_pad_y: f32,
    /// Height of the preview error bar.
    pub error_bar_h: f32,
    /// Height of one tab.
    pub tab_h: f32,
    /// Gap between tabs.
    pub tab_gap: f32,
    /// Horizontal padding inside a tab.
    pub tab_pad_x: f32,
    /// Width of a tab's close / dirty zone.
    pub tab_close_w: f32,
    /// Maximum tab width.
    pub tab_max_w: f32,
    /// Height of a segmented control.
    pub segmented_h: f32,
    /// Horizontal padding of a segmented option.
    pub segmented_pad_x: f32,
    /// Gap between segmented options.
    pub segmented_gap: f32,
    /// Inset between the segmented track and its options.
    pub segmented_track_pad: f32,
    /// Row height of a settings grid.
    pub grid_row_h: f32,
    /// Row height of a project-tree row.
    pub tree_row_h: f32,
    /// Width of a tree chevron cell.
    pub chevron_w: f32,
    /// Side of a standard vector icon ([`crate::icons::ICON_PX`]).
    pub icon: f32,
    /// Width of the fixed-size settings overlay.
    pub settings_w: f32,
    /// Height of the fixed-size settings overlay.
    pub settings_h: f32,
    /// Width of the settings category tab column.
    pub settings_tab_col_w: f32,
    /// Gap between the settings tab column and the content pane.
    pub settings_gap: f32,
    /// Height of the settings footer.
    pub settings_footer_h: f32,
    /// Side of a landing-page button.
    pub landing_btn: f32,
    /// Side of the icon inside a landing-page button.
    pub landing_btn_icon: f32,
    /// Gap between the two landing-page buttons.
    pub landing_btn_gap: f32,
    /// Side of the landing-page hero icon.
    pub landing_icon: f32,
    /// Gap between the landing hero icon and the title.
    pub landing_icon_gap: f32,
    /// Height of a standard (Primary/Secondary/Danger) button.
    pub control_h: f32,
    /// Horizontal padding of a standard button; matches the segmented option.
    pub control_pad_x: f32,
    /// Height of a quiet (toolbar/ghost) button.
    pub quiet_h: f32,
    /// Horizontal padding of a quiet button.
    pub quiet_pad_x: f32,
    /// Height of an empty-state CTA button.
    pub cta_h: f32,
    /// Horizontal padding of an empty-state CTA button.
    pub cta_pad_x: f32,
    /// Side of a square icon button.
    pub icon_btn: f32,
    /// Side of the compact pane-header icon button.
    pub icon_btn_small: f32,
    /// Row height of a menu item.
    pub menu_row_h: f32,
    /// Horizontal padding of a menu item row.
    pub menu_pad_x: f32,
    /// Horizontal inner padding of a text input.
    pub input_pad_x: f32,
    /// Vertical inner padding of a form text input.
    pub input_pad_y: f32,
    /// Vertical inner padding of a compact (find bar) text input.
    pub input_pad_y_compact: f32,
    /// Side of a checkbox box.
    pub checkbox_box: f32,
    /// Gap between a checkbox box and its label.
    pub checkbox_gap: f32,
    /// Row height of a list option / toggle chip.
    pub list_row_h: f32,
}

impl Spacing {
    /// The one and only spacing set (identical in both themes).
    pub const DEFAULT: Self = Self {
        gap: 10.0,
        card_pad_x: 6.0,
        card_pad_y: 4.0,
        header_h: 32.0,
        strip_w: 32.0,
        min_open_w: 120.0,
        splitter_w: 10.0,
        title_bar_h: 28.0,
        window_btn_w: 44.0,
        title_icon: 16.0,
        title_icon_gap: 6.0,
        status_pad_x: 10.0,
        status_pad_y: 3.0,
        error_bar_h: 160.0,
        tab_h: 28.0,
        tab_gap: 4.0,
        tab_pad_x: 10.0,
        tab_close_w: 24.0,
        tab_max_w: 180.0,
        segmented_h: 28.0,
        segmented_pad_x: 12.0,
        segmented_gap: 2.0,
        segmented_track_pad: 2.0,
        grid_row_h: 28.0,
        tree_row_h: 22.0,
        chevron_w: 12.0,
        icon: crate::icons::ICON_PX,
        settings_w: 687.0,
        settings_h: 440.0,
        settings_tab_col_w: 120.0,
        settings_gap: 10.0,
        settings_footer_h: 40.0,
        landing_btn: 60.0,
        landing_btn_icon: 26.0,
        landing_btn_gap: 18.0,
        landing_icon: 112.0,
        landing_icon_gap: 20.0,
        control_h: 28.0,
        control_pad_x: 12.0,
        quiet_h: 24.0,
        quiet_pad_x: 12.0,
        cta_h: 32.0,
        cta_pad_x: 12.0,
        icon_btn: 24.0,
        icon_btn_small: 18.0,
        menu_row_h: 26.0,
        menu_pad_x: 8.0,
        input_pad_x: 8.0,
        input_pad_y: 4.0,
        input_pad_y_compact: 3.0,
        checkbox_box: 16.0,
        checkbox_gap: 8.0,
        list_row_h: 24.0,
    };
}

/// Corner radii for the app's surfaces and controls, in whole pixels
/// (matching [`CornerRadius::same`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radii {
    /// Cards: the three panes, the settings card and the landing buttons.
    pub card: u8,
    /// The status-bar thin card.
    pub status: u8,
    /// Interactive controls (buttons, inputs, dropdowns, checkboxes).
    pub control: u8,
    /// Tabs and segmented options.
    pub tab: u8,
    /// Floating windows (settings overlay, shortcuts window).
    pub window: u8,
    /// Popups, menus and tooltips.
    pub menu: u8,
    /// Small pills: tab close zone, window-button hover, preview badge.
    pub small: u8,
    /// Tiny highlights: preview page placeholder, selection rects, scrollbar
    /// handles, completion rows.
    pub tiny: u8,
}

impl Radii {
    /// The one and only radius set (identical in both themes).
    pub const DEFAULT: Self = Self {
        card: 10,
        status: 8,
        control: 6,
        tab: 8,
        window: 12,
        menu: 10,
        small: 4,
        tiny: 2,
    };
}

/// Stroke widths used for borders, separators and accents, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strokes {
    /// Structural hairlines (gutter separator, tab underline, error-bar top).
    pub hairline: f32,
    /// 1px card / control outlines.
    pub card: f32,
    /// Accent strokes: active-tab frame, segmented selected ring.
    pub accent: f32,
    /// Outline of the open file in the project tree.
    pub outline: f32,
    /// Visible width of the pane-splitter handle while dragging.
    pub splitter: f32,
}

impl Strokes {
    /// The one and only stroke-width set (identical in both themes).
    pub const DEFAULT: Self = Self {
        hairline: 1.0,
        card: 1.0,
        accent: 2.0,
        outline: 1.5,
        splitter: 2.0,
    };
}

/// The color palette of one theme.
///
/// Surface depth, neutral fills and semantic families are explicit tokens:
/// `board` is `surface-bg`, `card` is `surface-1`, `raised` is `surface-2`.
/// `widget_*` fields map egui's widget-state vocabulary onto the token rules:
/// `widget_inactive_fill` is `fill-neutral`, `widget_hover_fill` is
/// `fill-neutral-hover`, `widget_stroke` is `border-default`, and
/// `widget_active_stroke` is `border-accent`. Semantic families expose both
/// `fg` and `bg` through [`SemanticKind`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorTheme {
    // -- Surfaces ----------------------------------------------------------
    /// Shared backdrop behind the cards: window background / panel fill.
    pub board: Color32,
    /// Surface of the three column cards and the status-bar card.
    pub card: Color32,
    /// Raised surface: popups, menus, floating windows and the title bar.
    pub raised: Color32,
    /// Fill for a preview page whose raster has not arrived yet.
    pub placeholder: Color32,
    /// 1px outline of a preview page.
    pub page_border: Color32,
    /// Alternating-row stripe of settings tables and the editor current-line
    /// band. Replaces egui's additive `faint_bg_color`, which is invisible on
    /// the light card.
    pub band: Color32,

    // -- Lines -------------------------------------------------------------
    /// 1px outline of a card, weaker than [`Self::hairline`].
    pub card_stroke: Color32,
    /// Structural line inside a card (gutter, tab underline, error-bar top).
    pub hairline: Color32,
    /// Edge of interactive widgets (egui `noninteractive.bg_stroke`); also used
    /// for popup/window borders.
    pub widget_stroke: Color32,

    // -- Text --------------------------------------------------------------
    /// Default text color (egui `text_color()`): labels, body copy.
    pub text: Color32,
    /// Strong / selected text (egui `strong_text_color()`).
    pub text_strong: Color32,
    /// Enabled secondary text: status values, pane titles, inactive tabs,
    /// menu shortcuts. Opaque and >= 4.5:1 on both card and raised surfaces.
    pub text_muted: Color32,
    /// Disabled content, placeholders and non-informational hints. Never use
    /// this for enabled information that the user must read.
    pub text_weak: Color32,

    // -- Accent, selection, cursor ----------------------------------------
    /// The app's accent color (egui `hyperlink_color`).
    ///
    /// Recorded usages: active-tab accent frame and continuation line (2px);
    /// segmented-control selected ring (2px); project tree: folder icon,
    /// main-file label + star, open-file outline (1.5px); landing-button icon
    /// on hover; preview reveal ring and text cursor; settings folder-icon
    /// fallback.
    ///
    /// NOT to be confused with the editor's syntax-highlight `Link` color
    /// (`typ-edit-editor/src/highlight.rs`: dark `rgb(79,193,255)`, light
    /// `rgb(0,0,210)`), which is a separate token.
    pub accent: Color32,
    /// Text selection background (egui `selection.bg_fill`).
    pub selection: Color32,
    /// Selected-text color / focus ring (egui `selection.stroke.color`).
    pub selection_text: Color32,
    /// Foreground on accent-filled primary controls.
    pub on_accent: Color32,
    /// Text cursor (egui `text_cursor.stroke.color`).
    pub caret: Color32,

    // -- Status ------------------------------------------------------------
    /// Info / neutral notification text and outline.
    pub info: Color32,
    /// Error / destructive outline and fill. Saturated on purpose; use
    /// [`Self::error_text`] for text.
    pub error: Color32,
    /// Error text with WCAG AA contrast on card, raised and error tints.
    pub error_text: Color32,
    /// Warning outline and fill. Saturated on purpose; use
    /// [`Self::warn_text`] for text.
    pub warn: Color32,
    /// Warning text with WCAG AA contrast on card, raised and warning tints.
    pub warn_text: Color32,
    /// Success / positive text (e.g. a git "added" badge).
    pub success: Color32,
    /// High-transparency info surface for Alert/Toast containers.
    pub info_bg: Color32,
    /// High-transparency error surface for Alert/Toast containers.
    pub error_bg: Color32,
    /// High-transparency warning surface for Alert/Toast containers.
    pub warn_bg: Color32,
    /// High-transparency success surface for Alert/Toast containers.
    pub success_bg: Color32,
    /// Error squiggle under a diagnostic range.
    pub squiggle_error: Color32,
    /// Warning squiggle under a diagnostic range.
    pub squiggle_warning: Color32,

    // -- Interactive widget states (egui `widgets.*`) ----------------------
    /// `widgets.inactive`: the resting fill of buttons, checkboxes and tracks.
    pub widget_inactive_fill: Color32,
    /// `widgets.inactive`: the resting text color of buttons.
    pub widget_inactive_text: Color32,
    /// `widgets.hovered`: fill while hovered.
    pub widget_hover_fill: Color32,
    /// `widgets.hovered`: border while hovered.
    pub widget_hover_stroke: Color32,
    /// `widgets.hovered`: text while hovered.
    pub widget_hover_text: Color32,
    /// `widgets.active`: fill while pressed / selected.
    pub widget_active_fill: Color32,
    /// `widgets.active`: border while pressed / selected.
    pub widget_active_stroke: Color32,
    /// `widgets.active`: text while pressed / selected.
    pub widget_active_text: Color32,
    /// `widgets.open`: fill of an expanded control (e.g. an open menu button).
    pub widget_open_fill: Color32,
    /// `widgets.open`: text of an expanded control.
    pub widget_open_text: Color32,
}

impl ColorTheme {
    /// Dark palette.
    pub const DARK: Self = Self {
        board: Color32::from_gray(10),
        card: Color32::from_gray(27),
        raised: Color32::from_gray(40),
        placeholder: Color32::from_gray(60),
        page_border: white_alpha(28),
        band: white_alpha(tokens::BAND_WHITE_ALPHA),

        card_stroke: white_alpha(14),
        hairline: white_alpha(tokens::HAIRLINE_ALPHA),
        widget_stroke: Color32::from_gray(60),

        text: Color32::from_gray(170),
        text_strong: Color32::WHITE,
        text_muted: Color32::from_gray(150),
        // `Visuals::dark().weak_text_color()`: gray(140) * 0.6 in gamma space.
        text_weak: Color32::from_rgba_premultiplied(84, 84, 84, 153),

        accent: Color32::from_rgb(90, 170, 255),
        selection: Color32::from_rgb(0, 92, 128),
        selection_text: Color32::from_rgb(192, 222, 255),
        on_accent: Color32::from_rgb(8, 18, 28),
        caret: Color32::from_rgb(192, 222, 255),

        info: Color32::from_rgb(100, 181, 246),
        error: Color32::from_rgb(255, 0, 0),
        error_text: Color32::from_rgb(255, 107, 107),
        warn: Color32::from_rgb(255, 143, 0),
        warn_text: Color32::from_rgb(255, 183, 77),
        success: Color32::from_rgb(80, 200, 120),
        info_bg: tint(100, 181, 246, 40),
        error_bg: tint(255, 0, 0, 40),
        warn_bg: tint(255, 143, 0, 40),
        success_bg: tint(80, 200, 120, 40),
        squiggle_error: Color32::from_rgb(230, 60, 60),
        squiggle_warning: Color32::from_rgb(255, 183, 77),

        widget_inactive_fill: Color32::from_gray(60),
        widget_inactive_text: Color32::from_gray(180),
        widget_hover_fill: Color32::from_gray(70),
        widget_hover_stroke: Color32::from_gray(150),
        widget_hover_text: Color32::from_gray(240),
        // Active keeps the hover weight (a micro-adjust, not a darker press)
        // and adds the accent border. The fill token therefore equals hover.
        widget_active_fill: Color32::from_gray(70),
        widget_active_stroke: Color32::from_rgb(90, 170, 255),
        widget_active_text: Color32::WHITE,
        widget_open_fill: Color32::from_gray(70),
        widget_open_text: Color32::WHITE,
    };

    /// Light palette.
    pub const LIGHT: Self = Self {
        board: Color32::from_gray(230),
        card: Color32::from_gray(255),
        // A floating surface must be one tonal step *below* the white card in
        // light mode (surface-2 is darker than surface-1).
        raised: Color32::from_gray(246),
        placeholder: Color32::from_gray(230),
        // White paper on a white card needs >= 3:1 at the page boundary.
        page_border: Color32::from_black_alpha(110),
        band: Color32::from_black_alpha(tokens::BAND_BLACK_ALPHA),

        card_stroke: Color32::from_black_alpha(28),
        hairline: Color32::from_black_alpha(tokens::HAIRLINE_ALPHA),
        widget_stroke: Color32::from_gray(190),

        text: Color32::from_gray(60),
        text_strong: Color32::BLACK,
        text_muted: Color32::from_gray(111),
        // `Visuals::light().weak_text_color()`: gray(80) * 0.6 in gamma space.
        text_weak: Color32::from_rgba_premultiplied(48, 48, 48, 153),

        accent: Color32::from_rgb(0, 155, 255),
        selection: Color32::from_rgb(144, 209, 255),
        selection_text: Color32::from_rgb(0, 83, 125),
        on_accent: Color32::from_rgb(0, 31, 48),
        caret: Color32::from_rgb(0, 83, 125),

        info: Color32::from_rgb(0, 102, 204),
        error: Color32::from_rgb(255, 0, 0),
        error_text: Color32::from_rgb(198, 40, 40),
        warn: Color32::from_rgb(255, 100, 0),
        warn_text: Color32::from_rgb(168, 87, 0),
        success: Color32::from_rgb(0, 140, 60),
        info_bg: tint(0, 102, 204, 26),
        error_bg: tint(255, 0, 0, 26),
        warn_bg: tint(255, 100, 0, 26),
        success_bg: tint(0, 140, 60, 26),
        squiggle_error: Color32::from_rgb(230, 60, 60),
        squiggle_warning: Color32::from_rgb(178, 106, 0),

        widget_inactive_fill: Color32::from_gray(230),
        widget_inactive_text: Color32::from_gray(60),
        widget_hover_fill: Color32::from_gray(220),
        widget_hover_stroke: Color32::from_gray(105),
        widget_hover_text: Color32::BLACK,
        widget_active_fill: Color32::from_gray(220),
        widget_active_stroke: Color32::from_rgb(0, 155, 255),
        widget_active_text: Color32::BLACK,
        widget_open_fill: Color32::from_gray(220),
        widget_open_text: Color32::BLACK,
    };

    /// The palette for `dark`.
    pub fn of(dark: bool) -> &'static Self {
        if dark { &Self::DARK } else { &Self::LIGHT }
    }

    /// The palette matching `ui`'s active dark/light mode.
    pub fn from_ui(ui: &egui::Ui) -> &'static Self {
        Self::of(ui.visuals().dark_mode)
    }

    /// Corner radius `r` as an egui [`CornerRadius`].
    pub const fn corner(r: u8) -> CornerRadius {
        CornerRadius::same(r)
    }
}

/// Installs the token state language into stock egui widget visuals.
///
/// egui maps keyboard focus to the *active* visuals
/// (`Response::widget_state`), so the accent active stroke below is also what
/// gives stock buttons, ComboBox triggers and selectable rows a visible accent
/// focus outline. Hand-drawn widgets read [`ColorTheme`] directly and are not
/// affected by this function.
pub fn install_widget_visuals(visuals: &mut egui::Visuals, theme: &Theme) {
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(theme.radii.control);
    }

    visuals.widgets.noninteractive.bg_stroke =
        Stroke::new(theme.strokes.card, theme.colors.widget_stroke);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(theme.strokes.card, theme.colors.text);
    visuals.widgets.inactive.fg_stroke = Stroke::new(theme.strokes.card, theme.colors.text);
    visuals.widgets.hovered.fg_stroke = Stroke::new(theme.strokes.card, theme.colors.text_strong);
    visuals.widgets.active.fg_stroke = Stroke::new(theme.strokes.card, theme.colors.text_strong);
    visuals.widgets.inactive.bg_fill = theme.colors.widget_inactive_fill;
    visuals.widgets.inactive.weak_bg_fill = theme.colors.band;
    visuals.widgets.hovered.bg_fill = theme.colors.widget_hover_fill;
    visuals.widgets.hovered.weak_bg_fill = theme.colors.widget_hover_fill;
    visuals.widgets.hovered.bg_stroke =
        Stroke::new(theme.strokes.card, theme.colors.widget_hover_stroke);
    visuals.widgets.active.bg_fill = theme.colors.widget_active_fill;
    visuals.widgets.active.weak_bg_fill = theme.colors.widget_active_fill;
    visuals.widgets.active.bg_stroke =
        Stroke::new(theme.strokes.card, theme.colors.widget_active_stroke);
    visuals.widgets.open.bg_fill = theme.colors.widget_open_fill;
    visuals.widgets.open.weak_bg_fill = theme.colors.widget_open_fill;
    visuals.widgets.open.bg_stroke = Stroke::new(theme.strokes.card, theme.colors.accent);
    visuals.widgets.open.fg_stroke = Stroke::new(theme.strokes.card, theme.colors.widget_open_text);

    visuals.window_stroke = Stroke::new(theme.strokes.card, theme.colors.widget_stroke);
    visuals.faint_bg_color = theme.colors.band;
    visuals.text_edit_bg_color = Some(theme.colors.band);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Surfaces form the physical-depth ladder the token spec requires.
    #[test]
    fn surfaces_follow_the_depth_ladder() {
        let dark = ColorTheme::DARK;
        assert!(dark.board.r() < dark.card.r());
        assert!(dark.card.r() < dark.raised.r());

        let light = ColorTheme::LIGHT;
        assert!(light.board.r() < light.card.r());
        assert!(
            light.raised.r() < light.card.r(),
            "surface-2 must be darker than surface-1 in light mode"
        );
    }

    /// Fill-neutral is a relative grayscale token: darker on light surfaces,
    /// brighter on dark surfaces. Active keeps the hover weight.
    #[test]
    fn neutral_fills_follow_the_theme_direction() {
        let dark = ColorTheme::DARK;
        assert!(dark.widget_inactive_fill.r() < dark.widget_hover_fill.r());
        assert_eq!(dark.widget_active_fill, dark.widget_hover_fill);

        let light = ColorTheme::LIGHT;
        assert!(light.widget_inactive_fill.r() > light.widget_hover_fill.r());
        assert_eq!(light.widget_active_fill, light.widget_hover_fill);
    }

    /// Active/focus/selected outlines are the accent color, never white/black.
    #[test]
    fn active_border_is_the_accent() {
        assert_eq!(
            ColorTheme::DARK.widget_active_stroke,
            ColorTheme::DARK.accent
        );
        assert_eq!(
            ColorTheme::LIGHT.widget_active_stroke,
            ColorTheme::LIGHT.accent
        );
    }

    /// Stock egui controls must receive the same state language as the
    /// hand-drawn widgets, especially the accent active/focus outline.
    #[test]
    fn stock_widget_visuals_follow_the_state_tokens() {
        for (mut visuals, theme) in [
            (egui::Visuals::dark(), &Theme::DARK),
            (egui::Visuals::light(), &Theme::LIGHT),
        ] {
            install_widget_visuals(&mut visuals, theme);
            assert_eq!(
                visuals.widgets.active.bg_fill,
                theme.colors.widget_active_fill
            );
            assert_eq!(visuals.widgets.active.bg_stroke.color, theme.colors.accent);
            assert_eq!(
                visuals.widgets.noninteractive.fg_stroke.color,
                theme.colors.text
            );
            assert_eq!(visuals.widgets.inactive.fg_stroke.color, theme.colors.text);
            assert_eq!(
                visuals.widgets.hovered.fg_stroke.color,
                theme.colors.text_strong
            );
            assert_eq!(
                visuals.widgets.hovered.bg_stroke.color,
                theme.colors.widget_hover_stroke
            );
            assert_eq!(visuals.widgets.open.bg_stroke.color, theme.colors.accent);
        }
    }

    /// Every semantic family has a foreground color and a translucent surface
    /// tint, and the tint is neither fully opaque nor fully transparent.
    #[test]
    fn semantic_families_are_complete() {
        for colors in [ColorTheme::DARK, ColorTheme::LIGHT] {
            for kind in SemanticKind::ALL {
                let fg = kind.fg(&colors);
                let bg = kind.bg(&colors);
                assert_ne!(fg, bg, "{kind:?} foreground and background must differ");
                assert!(
                    (1..255).contains(&bg.a()),
                    "{kind:?} background must be a translucent tint"
                );
            }
        }
    }

    #[test]
    fn semantic_surfaces_are_opaque_and_tinted() {
        for colors in [ColorTheme::DARK, ColorTheme::LIGHT] {
            for kind in SemanticKind::ALL {
                let surface = kind.surface(&colors);
                assert_eq!(
                    surface.a(),
                    255,
                    "{kind:?} toast/alerts surface must be opaque"
                );
                assert_ne!(
                    surface, colors.raised,
                    "{kind:?} surface must keep a visible semantic tint"
                );
            }
        }
    }

    /// WCAG 2.x relative luminance of one sRGB channel.
    fn linear(channel: u8) -> f32 {
        let c = f32::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Color32) -> f32 {
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Composite a premultiplied `Color32` over an opaque surface.
    fn over(fg: Color32, bg: Color32) -> Color32 {
        let a = f32::from(fg.a()) / 255.0;
        let blend = |f: u8, b: u8| (f32::from(f) + f32::from(b) * (1.0 - a)).round() as u8;
        Color32::from_rgb(
            blend(fg.r(), bg.r()),
            blend(fg.g(), bg.g()),
            blend(fg.b(), bg.b()),
        )
    }

    #[test]
    fn contrast_ladder_keeps_enabled_text_readable() {
        for colors in [ColorTheme::DARK, ColorTheme::LIGHT] {
            for surface in [colors.card, colors.raised] {
                assert!(
                    contrast(colors.text, surface) >= 4.5,
                    "text on {surface:?} must meet WCAG AA"
                );
                assert!(
                    contrast(colors.text_muted, surface) >= 4.5,
                    "text_muted on {surface:?} must meet WCAG AA"
                );
                assert!(
                    contrast(colors.text, surface) > contrast(colors.text_muted, surface),
                    "primary text must stay stronger than muted text"
                );
                assert!(
                    contrast(colors.text_muted, surface)
                        > contrast(over(colors.text_weak, surface), surface),
                    "muted text must stay stronger than disabled text"
                );
                assert!(
                    contrast(colors.error_text, surface) >= 4.5,
                    "error text on {surface:?} must meet WCAG AA"
                );
                assert!(
                    contrast(colors.warn_text, surface) >= 4.5,
                    "warning text on {surface:?} must meet WCAG AA"
                );
            }
            assert!(
                contrast(colors.error_text, over(colors.error_bg, colors.card)) >= 4.5,
                "error text on its tint must meet WCAG AA"
            );
            assert!(
                contrast(colors.warn_text, over(colors.warn_bg, colors.card)) >= 4.5,
                "warning text on its tint must meet WCAG AA"
            );
        }
    }

    #[test]
    fn theme_selection_is_consistent() {
        assert_eq!(Theme::of(true).colors, ColorTheme::DARK);
        assert_eq!(Theme::of(false).colors, ColorTheme::LIGHT);
        assert_eq!(Theme::LIGHT.spacing, Theme::DARK.spacing);
        assert_eq!(Theme::LIGHT.radii, Theme::DARK.radii);
        assert_eq!(Theme::LIGHT.strokes, Theme::DARK.strokes);
    }
}
