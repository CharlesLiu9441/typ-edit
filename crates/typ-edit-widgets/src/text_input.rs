//! Styled single-line text inputs.
//!
//! These wrap the stock [`egui::TextEdit::singleline`] with a custom frame and
//! re-paint the state stroke themselves. tui-whitelist: the stock widget owns
//! its own internal layout; this module only adds the frame/stroke. Text editing — selection, cursor, IME,
//! undo, clipboard, keyboard focus — is left entirely to egui: the function
//! returns the `TextEdit`'s own [`Response`] unchanged.
//!
//! The builder exists so the error and password states share one frame/stroke
//! implementation. `form_input` / `inline_input` are the common shortcuts.

use egui::{CornerRadius, Frame, Id, Margin, Response, Stroke, StrokeKind, TextEdit, Ui};

use crate::theme::Theme;

/// Builder for a token-styled single-line input.
pub struct TextInput<'a> {
    text: &'a mut String,
    hint: &'a str,
    width: f32,
    id: Option<Id>,
    compact: bool,
    password: bool,
    error: bool,
}

impl<'a> TextInput<'a> {
    /// A compact input for the find bar / inline filters: `radii.small`, 3 pt
    /// vertical padding.
    pub fn inline(text: &'a mut String, hint: &'a str, width: f32) -> Self {
        Self {
            text,
            hint,
            width,
            id: None,
            compact: true,
            password: false,
            error: false,
        }
    }

    /// A form input for settings tables and dialogs: `radii.control`, 4 pt
    /// vertical padding.
    pub fn form(text: &'a mut String, hint: &'a str, width: f32) -> Self {
        Self {
            text,
            hint,
            width,
            id: None,
            compact: false,
            password: false,
            error: false,
        }
    }

    /// Uses a stable egui id, required when the caller requests focus itself.
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Masks the text (community password fields).
    pub fn password(mut self) -> Self {
        self.password = true;
        self
    }

    /// Paints the semantic error outline instead of the normal/focus outline.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Adds the input and paints its token outline.
    pub fn show(self, ui: &mut Ui) -> Response {
        let Self {
            text,
            hint,
            width,
            id,
            compact,
            password,
            error,
        } = self;

        let theme = Theme::from_ui(ui);
        let (pad_y, radius) = if compact {
            (theme.spacing.input_pad_y_compact, theme.radii.small)
        } else {
            (theme.spacing.input_pad_y, theme.radii.control)
        };
        let pad_x = theme.spacing.input_pad_x;
        let frame = Frame::new()
            .fill(theme.colors.band)
            .inner_margin(Margin::symmetric(pad_x as i8, pad_y as i8))
            .corner_radius(CornerRadius::same(radius));

        // `hint_text` is always painted with egui's `weak_text_color()`, which the
        // app-wide style pins to `colors.text_weak`; `text_color` is pinned here.
        let mut edit = TextEdit::singleline(text)
            .frame(frame)
            // `width` is the total control width, including the frame padding;
            // this keeps call sites independent of the inner margin token.
            .desired_width((width - 2.0 * pad_x).max(1.0))
            .hint_text(hint.to_owned())
            .text_color(theme.colors.text);
        if password {
            edit = edit.password(true);
        }
        if let Some(id) = id {
            edit = edit.id(id);
        }
        let response = ui.add(edit);

        let stroke = if !response.enabled() {
            Stroke::new(
                theme.strokes.card,
                theme.colors.widget_stroke.gamma_multiply(0.5),
            )
        } else if error {
            // A semantic error outline wins over focus: the user must keep the
            // error state visible while the field is being corrected.
            let width = if response.has_focus() || response.hovered() {
                theme.strokes.accent
            } else {
                theme.strokes.card
            };
            Stroke::new(width, theme.colors.error)
        } else if response.has_focus() {
            Stroke::new(theme.strokes.accent, theme.colors.accent)
        } else if response.hovered() {
            Stroke::new(theme.strokes.card, theme.colors.widget_hover_stroke)
        } else {
            Stroke::new(theme.strokes.card, theme.colors.widget_stroke)
        };
        ui.painter().rect_stroke(
            response.rect,
            CornerRadius::same(radius),
            stroke,
            StrokeKind::Inside,
        );
        response
    }
}

/// A compact text input for the find bar: `radii.small`, 3 pt vertical padding.
pub fn inline_input(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    id: Option<Id>,
) -> Response {
    let mut input = TextInput::inline(text, hint, width);
    if let Some(id) = id {
        input = input.id(id);
    }
    input.show(ui)
}

/// A form text input for settings tables and dialogs: `radii.control`,
/// 4 pt vertical padding.
pub fn form_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    TextInput::form(text, hint, width).show(ui)
}

/// A masked form input using the same frame and state language as
/// [`form_input`].
pub fn form_password(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    TextInput::form(text, hint, width).password().show(ui)
}
