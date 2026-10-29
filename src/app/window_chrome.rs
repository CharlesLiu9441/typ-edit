//! OS window chrome for the frameless viewport: the custom title strip (menus,
//! drag zone, window buttons) and native edge resizing.

use super::dispatch::CommandState;
use super::menu::MenuOutcome;
use super::state::UiState;
use crate::branding;
use crate::keybindings::KeyBindings;
use crate::settings::Settings;
use typ_edit_i18n::t;
use typ_edit_widgets::icons::{WINDOW_ICON_PX, WindowGlyph, arrow_back, window_glyph};
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{AlignItems, Dimension, Size, Style},
};

/// Fixed height of the custom title/menu strip; the resize-edge detection
/// excludes this band at the top of the window.
pub(super) const TITLE_BAR_H: f32 = 28.0;
/// Width of each of the three window buttons (minimize / maximize-restore / close).
const WINDOW_BTN_W: f32 = 44.0;
/// Width of the title-strip home button shown left of the window buttons.
const HOME_BTN_W: f32 = 36.0;
/// Side of the app icon at the far left of the title strip, in logical pixels.
const TITLE_ICON_PX: f32 = 16.0;
/// Gap between the title-strip icon and the menus (or the drag zone).
const TITLE_ICON_GAP: f32 = 6.0;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn macos_corner_radius(maximized: bool, fullscreen: bool) -> f64 {
    if maximized || fullscreen { 0.0 } else { 8.0 }
}

/// Inputs for the custom title strip.
pub(super) struct TitleBarInput<'a, 'b> {
    pub has_root: bool,
    pub settings: &'a Settings,
    pub bindings: &'a KeyBindings,
    pub state: &'a CommandState<'b>,
    pub has_document: bool,
    pub exporting: bool,
}

impl UiState {
    /// The custom title strip: brand icon and menus on the left, a drag zone with
    /// the centered document title, and the home / window buttons on the right.
    /// Returns the app-only effects of the menu buttons for the caller to apply.
    ///
    /// The whole strip is one tui row (a child of the root column's fixed 28pt
    /// title band): the drag zone is added before the buttons so the buttons win
    /// overlapping clicks, exactly as the former hand-ordered row did.
    pub(super) fn title_bar(
        &mut self,
        tree: &mut Tui,
        input: TitleBarInput<'_, '_>,
    ) -> MenuOutcome {
        let TitleBarInput {
            has_root,
            settings,
            bindings,
            state,
            has_document,
            exporting,
        } = input;
        let mut menu = MenuOutcome::default();
        let row_id = egui::Id::new("typ_edit_title_bar");
        let mut style = tui::row();
        style.align_items = Some(AlignItems::Center);
        style.size = Size {
            width: Dimension::percent(1.0),
            height: Dimension::length(TITLE_BAR_H),
        };
        tree.style(style).id(row_id).add_with_background_ui(
            |ui, container| {
                ui.painter().rect_filled(
                    container.full_container(),
                    0,
                    crate::theme::title_bar(ui),
                );
            },
            |tree, _| {
                let strip = tree.egui_ui().max_rect();
                let strip_h = TITLE_BAR_H;

                // Brand icon at the far left of the strip, in every state
                // (workspace and landing alike), then a fixed gap before the menus.
                gap_leaf(tree, row_id.with("left_inset"), 6.0);
                tree.style(tui::fixed(TITLE_ICON_PX))
                    .id(row_id.with("icon"))
                    .ui(|ui| {
                        let texture = branding::icon_texture(
                            ui.ctx(),
                            &mut self.icon_small,
                            branding::ICON_64,
                        );
                        ui.add(
                            egui::Image::new(&texture)
                                .fit_to_exact_size(egui::Vec2::splat(TITLE_ICON_PX)),
                        );
                    });
                gap_leaf(tree, row_id.with("icon_gap"), TITLE_ICON_GAP);

                // The landing page (no workspace) has no menu bar.
                if has_root {
                    menu = self.main_menu(tree, settings, bindings, state, has_document, exporting);
                }

                // Drag zone first: it claims the remaining width, then the fixed
                // buttons are registered after it so they win their own clicks.
                let show_home = has_root && !self.landing;
                let mut drag_style = tui::flexible();
                drag_style.size = Size {
                    width: Dimension::auto(),
                    height: Dimension::length(strip_h),
                };
                tree.style(drag_style).id(row_id.with("drag")).ui(|ui| {
                    let (drag_rect, drag) =
                        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
                    self.paint_title(ui, strip, drag_rect);
                    if drag.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                });

                if show_home
                    && fixed_slot(tree, row_id.with("home"), HOME_BTN_W, strip_h, |ui| {
                        home_button(ui, strip_h)
                    })
                {
                    self.landing = true;
                }
                let maximized = tree
                    .egui_ui()
                    .ctx()
                    .input(|i| i.viewport().maximized)
                    .unwrap_or(false);
                fixed_slot(tree, row_id.with("minimize"), WINDOW_BTN_W, strip_h, |ui| {
                    window_button(ui, strip_h, WindowButton::Minimize, maximized)
                });
                fixed_slot(tree, row_id.with("maximize"), WINDOW_BTN_W, strip_h, |ui| {
                    window_button(ui, strip_h, WindowButton::MaximizeRestore, maximized)
                });
                fixed_slot(tree, row_id.with("close"), WINDOW_BTN_W, strip_h, |ui| {
                    window_button(ui, strip_h, WindowButton::Close, maximized)
                });
            },
        );
        menu
    }

    /// Paints `last_title` centered on the whole strip, clamped so it stays
    /// between the menus and the window buttons; weak color, truncated to the
    /// drag area's width.
    fn paint_title(&self, ui: &egui::Ui, strip: egui::Rect, drag: egui::Rect) {
        if drag.width() < 80.0 {
            return;
        }
        let color = Theme::from_ui(ui).colors.text_muted;
        let mut job = egui::text::LayoutJob::simple_singleline(
            self.last_title.clone(),
            egui::TextStyle::Body.resolve(ui.style()),
            color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(drag.width() - 8.0);
        let galley = ui.painter().layout_job(job);
        let half = 0.5 * galley.size().x;
        let min_center = drag.left() + half;
        let max_center = drag.right() - half;
        let center_x = if min_center <= max_center {
            strip.center().x.clamp(min_center, max_center)
        } else {
            drag.center().x
        };
        let pos = egui::pos2(center_x - half, drag.center().y - 0.5 * galley.size().y);
        ui.painter().with_clip_rect(drag).galley(pos, galley, color);
    }

    /// Drives native edge resizing for the frameless window: outside the title
    /// strip, hovering an edge sets the resize cursor and a press begins the
    /// native resize.
    pub(super) fn window_resize_edges(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized).unwrap_or(false) {
            return;
        }
        let rect = ctx.viewport_rect();
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let Some(direction) = resize_direction(pos, rect, 6.0, TITLE_BAR_H) else {
            return;
        };
        let cursor = match direction {
            egui::ResizeDirection::West => egui::CursorIcon::ResizeWest,
            egui::ResizeDirection::East => egui::CursorIcon::ResizeEast,
            egui::ResizeDirection::South => egui::CursorIcon::ResizeSouth,
            egui::ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
            egui::ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
            _ => return,
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}

/// Which resizable edge of the window (if any) `pos` lies in.
///
/// The top band (`title_bar_h`) is never a resize zone: it belongs to the title
/// strip, whose drag zone and buttons must keep receiving the pointer. There is
/// no north edge for the same reason.
pub(super) fn resize_direction(
    pos: egui::Pos2,
    rect: egui::Rect,
    margin: f32,
    title_bar_h: f32,
) -> Option<egui::ResizeDirection> {
    if pos.y < rect.top() + title_bar_h {
        return None;
    }
    let west = pos.x < rect.left() + margin;
    let east = pos.x > rect.right() - margin;
    let south = pos.y > rect.bottom() - margin;
    match (west, east, south) {
        (true, _, true) => Some(egui::ResizeDirection::SouthWest),
        (_, true, true) => Some(egui::ResizeDirection::SouthEast),
        (true, _, false) => Some(egui::ResizeDirection::West),
        (_, true, false) => Some(egui::ResizeDirection::East),
        (_, _, true) => Some(egui::ResizeDirection::South),
        _ => None,
    }
}

/// One fixed-width empty leaf (the exact gaps of the title strip).
fn gap_leaf(tree: &mut Tui, id: egui::Id, width: f32) {
    tree.style(tui::fixed(width)).id(id).add_empty();
}

/// One exactly-sized title-strip leaf (home / window buttons), returning what
/// the leaf's content closure returned.
fn fixed_slot<T>(
    tree: &mut Tui,
    id: egui::Id,
    width: f32,
    height: f32,
    add: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    let mut style: Style = tui::fixed(width);
    style.size = Size {
        width: Dimension::length(width),
        height: Dimension::length(height),
    };
    tree.style(style).id(id).ui(add)
}

/// One of the three window buttons in the custom title strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowButton {
    Minimize,
    MaximizeRestore,
    Close,
}

/// Allocates and paints one window button. The icon is painted with shapes only
/// (never font glyphs); returns whether it was clicked.
fn window_button(ui: &mut egui::Ui, height: f32, button: WindowButton, maximized: bool) -> bool {
    let theme = Theme::from_ui(ui);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(WINDOW_BTN_W, height), egui::Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let is_close = button == WindowButton::Close;
    // The close control is the one destructive window action: hover/press paint
    // a pure red surface (the theme's semantic error color), not a gray hover
    // with a red glyph. The glyph flips to white so it stays legible.
    if hovered || pressed {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(theme.radii.small),
            if is_close {
                theme.colors.error
            } else if pressed {
                theme.colors.widget_active_fill
            } else {
                theme.colors.widget_hover_fill
            },
        );
    }
    if pressed || response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(theme.radii.small),
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let glyph_color = if is_close && (hovered || pressed) {
        egui::Color32::WHITE
    } else {
        theme.colors.widget_active_text
    };
    let glyph = match button {
        WindowButton::Minimize => WindowGlyph::Minimize,
        WindowButton::MaximizeRestore if maximized => WindowGlyph::Restore,
        WindowButton::MaximizeRestore => WindowGlyph::Maximize,
        WindowButton::Close => WindowGlyph::Close,
    };
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(WINDOW_ICON_PX));
    window_glyph(ui, icon_rect, glyph, glyph_color);

    let tooltip = match button {
        WindowButton::Minimize => t("window.minimize"),
        WindowButton::MaximizeRestore if maximized => t("window.restore"),
        WindowButton::MaximizeRestore => t("window.maximize"),
        WindowButton::Close => t("window.close"),
    };
    let response = response.on_hover_text(tooltip);
    if response.clicked() {
        let command = match button {
            WindowButton::Minimize => egui::ViewportCommand::Minimized(true),
            WindowButton::MaximizeRestore => egui::ViewportCommand::Maximized(!maximized),
            WindowButton::Close => egui::ViewportCommand::Close,
        };
        ui.ctx().send_viewport_cmd(command);
    }
    response.clicked()
}

/// Allocates and paints the title-strip home button shown left of the window
/// buttons while a workspace is open; returns whether it was clicked.
fn home_button(ui: &mut egui::Ui, height: f32) -> bool {
    let theme = Theme::from_ui(ui);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(HOME_BTN_W, height), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(theme.radii.small),
            theme.colors.widget_hover_fill,
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(theme.radii.small),
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(WINDOW_ICON_PX));
    arrow_back(ui, icon_rect, theme.colors.widget_active_text);
    response.on_hover_text(t("window.home")).clicked()
}

#[cfg(test)]
mod tests {
    use super::macos_corner_radius;

    #[test]
    fn macos_corner_radius_is_square_when_maximized_or_fullscreen() {
        assert_eq!(macos_corner_radius(false, false), 8.0);
        assert_eq!(macos_corner_radius(true, false), 0.0);
        assert_eq!(macos_corner_radius(false, true), 0.0);
        assert_eq!(macos_corner_radius(true, true), 0.0);
    }
}
