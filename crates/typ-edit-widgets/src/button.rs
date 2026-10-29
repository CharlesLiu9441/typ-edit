//! The button family: filled/outlined/quiet text buttons and square icon
//! buttons, all drawn by hand from the design-system [`Theme`].
//!
//! Every function is a free function over `ui` and returns the widget's
//! [`Response`], so `.on_hover_text` / `.on_disabled_hover_text` keep working.
//! Keyboard activation is inherited from `Sense::click()` and
//! [`Response::clicked`], which folds in egui's keyboard-generated
//! `FAKE_PRIMARY_CLICKED`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, TextStyle, Ui, Vec2,
};

use crate::interact::selection_hover;
use crate::theme::Theme;

/// One of the app's button roles; see `docs/design/widgets-redesign.md` §2.1.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Dialog primary action (confirm / save / export).
    Primary,
    /// Dialog secondary action (cancel / close).
    Secondary,
    /// Destructive action (delete / discard).
    Danger,
    /// Toolbar / find-bar / ghost button.
    Quiet,
    /// Empty-state call to action.
    Cta,
}

/// The interaction state driving a button's colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Normal,
    Hovered,
    Pressed,
}

/// Draws a single-line text button and returns its response.
///
/// `variant` picks the role (and its height and padding); `enabled == false`
/// paints the disabled state and returns a response whose `clicked()` is
/// `false` but whose `.on_disabled_hover_text` still works.
pub fn button(ui: &mut Ui, label: &str, variant: ButtonVariant, enabled: bool) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        let (height, pad_x) = metrics(variant, theme);
        let font = TextStyle::Body.resolve(ui.style());
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
        let size = Vec2::new(galley.size().x + 2.0 * pad_x, height);
        // tui-whitelist: 单叶 allocate_exact_size（按钮自身体积 + 命中区），无子元素排布。
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        let state = interact_state(can_interact, &response);
        let (fill, stroke, text) = match state {
            Some(state) => state_colors(variant, theme, ui.visuals().dark_mode, state),
            None => disabled_colors(variant, theme),
        };

        let radius = CornerRadius::same(theme.radii.control);
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, radius, fill);
        }
        if let Some(stroke) = stroke {
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
        }
        if state.is_some() && response.has_focus() {
            // Focus is the keyboard-active state. Destructive actions keep the
            // semantic error outline so focus never masks the error meaning.
            let color = match variant {
                ButtonVariant::Danger => theme.colors.error,
                ButtonVariant::Primary => theme.colors.on_accent,
                _ => theme.colors.accent,
            };
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(theme.strokes.accent, color),
                StrokeKind::Inside,
            );
        }
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            text,
        );
        if state.is_some() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

/// Draws a square glyph button (for example `−`, `+`, `◀`, `▶`, `×`) and
/// returns its response.
///
/// `size` is the side of the square; the glyph is painted at `size * 0.55` in
/// the proportional font.
pub fn icon_button(ui: &mut Ui, glyph: &str, size: f32, enabled: bool) -> Response {
    icon_button_impl(ui, glyph, size, enabled, None)
}

/// [`icon_button`] with a custom resting glyph color; used by the pane-collapse
/// arrow, which rests at `colors.text_weak`.
pub fn icon_button_tinted(
    ui: &mut Ui,
    glyph: &str,
    size: f32,
    enabled: bool,
    rest: Color32,
) -> Response {
    icon_button_impl(ui, glyph, size, enabled, Some(rest))
}

fn icon_button_impl(
    ui: &mut Ui,
    glyph: &str,
    size: f32,
    enabled: bool,
    rest: Option<Color32>,
) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        // tui-whitelist: 单叶 allocate_exact_size（图标按钮自身体积 + 命中区）。
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
        let state = interact_state(can_interact, &response);
        let (fill, text) = match state {
            Some(State::Hovered) => (
                theme.colors.widget_hover_fill.gamma_multiply(0.5),
                theme.colors.text_strong,
            ),
            Some(State::Pressed) => (
                theme.colors.widget_active_fill.gamma_multiply(0.5),
                theme.colors.text_strong,
            ),
            Some(State::Normal) => (Color32::TRANSPARENT, rest.unwrap_or(theme.colors.text)),
            None => (
                Color32::TRANSPARENT,
                theme.colors.text_weak.gamma_multiply(0.5),
            ),
        };
        let radius = CornerRadius::same(if size >= theme.spacing.icon_btn {
            theme.radii.control
        } else {
            theme.radii.small
        });
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, radius, fill);
        }
        if state.is_some() && (response.has_focus() || state == Some(State::Pressed)) {
            let width = if response.has_focus() {
                theme.strokes.accent
            } else {
                theme.strokes.card
            };
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(width, theme.colors.accent),
                StrokeKind::Inside,
            );
        }
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(size * 0.55),
            text,
        );
        if state.is_some() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

/// Draws a square icon button whose icon is painted by `paint` (vector icons).
///
/// State, focus ring, fill and cursor follow the same language as
/// [`icon_button`]; only the glyph is caller-supplied.
pub fn icon_button_painted(
    ui: &mut Ui,
    size: f32,
    enabled: bool,
    paint: impl FnOnce(&Ui, Rect, Color32),
) -> Response {
    icon_button_painted_impl(ui, size, enabled, false, paint)
}

/// [`icon_button_painted`] for a destructive control such as a close button:
/// hover/press paint the semantic error color as a solid surface and the glyph
/// flips to white. At rest the button stays transparent like its siblings.
pub fn icon_button_painted_danger(
    ui: &mut Ui,
    size: f32,
    enabled: bool,
    paint: impl FnOnce(&Ui, Rect, Color32),
) -> Response {
    icon_button_painted_impl(ui, size, enabled, true, paint)
}

fn icon_button_painted_impl(
    ui: &mut Ui,
    size: f32,
    enabled: bool,
    danger: bool,
    paint: impl FnOnce(&Ui, Rect, Color32),
) -> Response {
    let can_interact = enabled && ui.is_enabled();
    ui.add_enabled_ui(enabled, |ui| {
        let theme = Theme::from_ui(ui);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
        let state = interact_state(can_interact, &response);
        let (fill, color) = match state {
            Some(State::Hovered | State::Pressed) if danger => (theme.colors.error, Color32::WHITE),
            Some(State::Hovered) => (
                theme.colors.widget_hover_fill.gamma_multiply(0.5),
                theme.colors.text_strong,
            ),
            Some(State::Pressed) => (
                theme.colors.widget_active_fill.gamma_multiply(0.5),
                theme.colors.text_strong,
            ),
            Some(State::Normal) => (Color32::TRANSPARENT, theme.colors.text_muted),
            None => (
                Color32::TRANSPARENT,
                theme.colors.text_weak.gamma_multiply(0.5),
            ),
        };
        let radius = CornerRadius::same(if size >= theme.spacing.icon_btn {
            theme.radii.control
        } else {
            theme.radii.small
        });
        let painter = ui.painter();
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, radius, fill);
        }
        if state.is_some() && (response.has_focus() || state == Some(State::Pressed)) {
            let width = if response.has_focus() {
                theme.strokes.accent
            } else {
                theme.strokes.card
            };
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(width, theme.colors.accent),
                StrokeKind::Inside,
            );
        }
        paint(ui, rect, color);
        if state.is_some() && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

/// The `(height, horizontal padding)` of `variant`'s text button.
fn metrics(variant: ButtonVariant, theme: &Theme) -> (f32, f32) {
    let spacing = &theme.spacing;
    match variant {
        ButtonVariant::Primary | ButtonVariant::Secondary | ButtonVariant::Danger => {
            (spacing.control_h, spacing.control_pad_x)
        }
        ButtonVariant::Quiet => (spacing.quiet_h, spacing.quiet_pad_x),
        ButtonVariant::Cta => (spacing.cta_h, spacing.cta_pad_x),
    }
}

/// Maps the response to a state, or `None` while disabled.
fn interact_state(can_interact: bool, response: &Response) -> Option<State> {
    if !can_interact {
        None
    } else if response.is_pointer_button_down_on() {
        Some(State::Pressed)
    } else if response.hovered() {
        Some(State::Hovered)
    } else {
        Some(State::Normal)
    }
}

/// The `(fill, stroke, text)` of `variant` in `state`, per §2.1.2.
fn state_colors(
    variant: ButtonVariant,
    theme: &Theme,
    dark: bool,
    state: State,
) -> (Color32, Option<Stroke>, Color32) {
    let c = &theme.colors;
    let hairline = theme.strokes.card;
    match variant {
        ButtonVariant::Primary => {
            let fill = match state {
                State::Normal => c.accent,
                State::Hovered => selection_hover(c.accent, dark),
                State::Pressed => selection_hover(c.accent, dark),
            };
            let stroke =
                (state == State::Pressed).then(|| Stroke::new(theme.strokes.accent, c.on_accent));
            (fill, stroke, c.on_accent)
        }
        ButtonVariant::Secondary => match state {
            // Face type: neutral fill at rest, no outline, grayscale hover,
            // accent outline only while active.
            State::Normal => (c.widget_inactive_fill, None, c.text),
            State::Hovered => (c.widget_hover_fill, None, c.text_strong),
            State::Pressed => (
                c.widget_active_fill,
                Some(Stroke::new(theme.strokes.card, c.accent)),
                c.text_strong,
            ),
        },
        ButtonVariant::Danger => match state {
            // Semantic color stays on the outline/text; hover remains a
            // grayscale fill change. Active keeps the semantic error outline.
            State::Normal => (
                Color32::TRANSPARENT,
                Some(Stroke::new(hairline, c.error.gamma_multiply(0.55))),
                c.error,
            ),
            State::Hovered => (
                c.widget_hover_fill,
                Some(Stroke::new(hairline, c.error)),
                c.error,
            ),
            State::Pressed => (
                c.widget_active_fill,
                Some(Stroke::new(theme.strokes.outline, c.error)),
                c.error,
            ),
        },
        ButtonVariant::Quiet => {
            let (fill, stroke, text) = match state {
                State::Normal => (Color32::TRANSPARENT, None, c.text),
                State::Hovered => (c.widget_hover_fill.gamma_multiply(0.5), None, c.text_strong),
                State::Pressed => (
                    c.widget_active_fill.gamma_multiply(0.5),
                    Some(Stroke::new(theme.strokes.card, c.accent)),
                    c.text_strong,
                ),
            };
            (fill, stroke, text)
        }
        ButtonVariant::Cta => match state {
            // Line type: card surface at rest, high-contrast gray outline on
            // hover, accent only in the active state.
            State::Normal => (c.card, Some(Stroke::new(hairline, c.card_stroke)), c.text),
            State::Hovered => (
                c.widget_hover_fill,
                Some(Stroke::new(hairline, c.widget_hover_stroke)),
                c.text_strong,
            ),
            State::Pressed => (
                c.widget_active_fill,
                Some(Stroke::new(theme.strokes.outline, c.accent)),
                c.widget_active_text,
            ),
        },
    }
}

/// The `(fill, stroke, text)` of `variant` while disabled, per §2.1.2.
fn disabled_colors(variant: ButtonVariant, theme: &Theme) -> (Color32, Option<Stroke>, Color32) {
    let c = &theme.colors;
    let hairline = theme.strokes.card;
    match variant {
        ButtonVariant::Primary => (c.widget_inactive_fill, None, c.text_weak),
        ButtonVariant::Secondary => (
            c.widget_inactive_fill.gamma_multiply(0.5),
            None,
            c.text_weak,
        ),
        ButtonVariant::Danger => (
            Color32::TRANSPARENT,
            Some(Stroke::new(hairline, c.error.gamma_multiply(0.3))),
            c.error.gamma_multiply(0.5),
        ),
        ButtonVariant::Quiet => (Color32::TRANSPARENT, None, c.text_weak),
        ButtonVariant::Cta => (
            c.card,
            Some(Stroke::new(hairline, c.card_stroke)),
            c.text_weak,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_uses_the_accent_fill() {
        let theme = Theme::of(false);
        let (fill, _, text) = state_colors(ButtonVariant::Primary, theme, false, State::Normal);
        assert_eq!(fill, theme.colors.accent);
        assert_eq!(text, theme.colors.on_accent);
    }
}
