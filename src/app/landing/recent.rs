//! The 最近项目 card on the landing page: a centered, full-width card holding a
//! header with the clear-all action and one clickable row per recent project.
//!
//! The card and its rows are children of the landing page's tui column: the
//! card is one padded container, each row a tui row with a flexible body leaf
//! (self-painted name/path over one hit area) and a fixed remove leaf.

use crate::app::state::UiState;
use crate::settings::Settings;
use std::path::{Path, PathBuf};
use typ_edit_i18n::t;
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::icons::{WindowGlyph, window_glyph};
use typ_edit_widgets::separator::separator;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{
        AlignItems, Dimension, JustifyContent, LengthPercentage, Rect as TaffyRect, Size, Style,
    },
};

/// Corner radius of the recent-projects card.
const RECENT_CARD_RADIUS: u8 = 10;
/// Inner padding of the recent-projects card.
const RECENT_CARD_PAD: f32 = 12.0;
/// Horizontal margin around the recent-projects card, as a fraction of the
/// available width.
const RECENT_CARD_MARGIN_FRAC: f32 = 0.10;
/// Smallest horizontal margin around the recent-projects card.
const RECENT_CARD_MARGIN_MIN: f32 = 24.0;
/// Largest horizontal margin around the recent-projects card.
const RECENT_CARD_MARGIN_MAX: f32 = 48.0;
/// Smallest width the recent-projects card shrinks to on a narrow window.
const RECENT_CARD_MIN_W: f32 = 220.0;
/// Height of one recent-project row.
const RECENT_ROW_H: f32 = 40.0;
/// Width of the trailing remove cell in a recent-project row.
const RECENT_REMOVE_W: f32 = 32.0;
/// Side of the vector close icon inside the remove cell.
const RECENT_REMOVE_ICON: f32 = 12.0;
/// Horizontal padding inside a recent-project row.
const RECENT_ROW_PAD: f32 = 10.0;
/// Vertical gap between a row's name and its path.
const RECENT_TEXT_GAP: f32 = 2.0;
/// Vertical gap between consecutive recent-project rows.
const RECENT_ROW_GAP: f32 = 4.0;
/// Gap between the buttons row and the card.
const RECENT_TOP_GAP: f32 = 28.0;

/// App-only effects produced by rendering the 最近项目 card, applied by the
/// caller after the render returns.
#[derive(Default)]
pub(in crate::app) struct RecentOutcome {
    /// A recent project to open.
    pub(in crate::app) open: Option<PathBuf>,
    /// A recent project to drop from the list.
    pub(in crate::app) remove: Option<PathBuf>,
    /// Whether 清除 was chosen.
    pub(in crate::app) clear: bool,
}

/// The card surface: fixed width, auto height, inner padding. Its explicit
/// width is a border box in taffy 0.9.2, so `content_w` is used for the text.
fn card_style(card_w: f32) -> Style {
    let mut style = tui::column();
    style.size = Size {
        width: Dimension::length(card_w),
        height: Dimension::auto(),
    };
    style.align_self = Some(AlignItems::Center);
    style.padding = TaffyRect {
        left: LengthPercentage::length(RECENT_CARD_PAD),
        right: LengthPercentage::length(RECENT_CARD_PAD),
        top: LengthPercentage::length(RECENT_CARD_PAD),
        bottom: LengthPercentage::length(RECENT_CARD_PAD),
    };
    style
}

impl UiState {
    /// The 最近项目 card under the landing buttons. Drawn only while the feature
    /// is enabled and the list is non-empty, so a disabled feature leaves the
    /// page exactly as it was.
    pub(super) fn recent_projects_section(
        &mut self,
        tree: &mut Tui,
        settings: &Settings,
    ) -> RecentOutcome {
        let mut out = RecentOutcome::default();
        if !settings.workspace.recent_projects_enabled
            || settings.workspace.recent_projects.is_empty()
        {
            return out;
        }
        let base = tree.current_id().with("recent_projects");
        tree.style(tui::fixed(RECENT_TOP_GAP))
            .id(base.with("top_gap"))
            .add_empty();

        let full = tree.egui_ui().available_width();
        let margin = (full * RECENT_CARD_MARGIN_FRAC)
            .clamp(RECENT_CARD_MARGIN_MIN, RECENT_CARD_MARGIN_MAX)
            .min(((full - RECENT_CARD_MIN_W) * 0.5).max(0.0));
        let card_w = (full - 2.0 * margin).max(RECENT_CARD_MIN_W).min(full);

        let recents = settings.workspace.recent_projects.clone();
        let mut open: Option<PathBuf> = None;
        let mut remove: Option<PathBuf> = None;
        let mut clear = false;
        tree.style(card_style(card_w))
            .id(base.with("card"))
            .add_with_background_ui(
                |ui, container| {
                    let rect = container.full_container();
                    let theme = Theme::from_ui(ui);
                    let painter = ui.painter();
                    painter.rect_filled(
                        rect,
                        egui::CornerRadius::same(RECENT_CARD_RADIUS),
                        theme.colors.card,
                    );
                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::same(RECENT_CARD_RADIUS),
                        egui::Stroke::new(theme.strokes.card, theme.colors.card_stroke),
                        egui::StrokeKind::Inside,
                    );
                },
                |tree, _| {
                    let mut header = tui::row();
                    header.align_items = Some(AlignItems::Center);
                    header.justify_content = Some(JustifyContent::SpaceBetween);
                    tree.style(header).id(base.with("header")).add(|row| {
                        row.style(tui::label_leaf())
                            .id(base.with("title"))
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .label(egui::RichText::new(t("landing.recent.title")).strong());
                        clear = row.id(base.with("clear")).ui(|ui| {
                            button(ui, &t("landing.recent.clear"), ButtonVariant::Quiet, true)
                                .clicked()
                        });
                    });
                    tree.id(base.with("separator")).ui(separator);
                    // Rows live in a capped scroll area: with a long MRU list
                    // the card must not push the landing page past the window.
                    // tui-whitelist: ScrollArea is the allowed container; the
                    // rows are plain egui widgets, not nested tui nodes.
                    let rows_id = base.with("rows");
                    let row_stride = RECENT_ROW_H + RECENT_ROW_GAP;
                    let max_rows = 4.0;
                    tree.id(rows_id).ui(|ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(rows_id.with("scroll"))
                            .max_height(row_stride * max_rows)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for path in &recents {
                                    let (open_row, remove_row) = recent_row(ui, path);
                                    if open_row {
                                        open = Some(path.clone());
                                    }
                                    if remove_row {
                                        remove = Some(path.clone());
                                    }
                                    ui.add_space(RECENT_ROW_GAP);
                                }
                            });
                    });
                },
            );

        if let Some(path) = open {
            out.open = Some(path);
        } else if let Some(path) = remove {
            out.remove = Some(path);
        } else if clear {
            out.clear = true;
        }
        out
    }
}

/// One recent-project row: name over path in the body, a separate trailing
/// remove zone, and a focus ring for keyboard users. Returns `(open, remove)`.
fn recent_row(ui: &mut egui::Ui, path: &Path) -> (bool, bool) {
    let theme = Theme::from_ui(ui);
    let radius = egui::CornerRadius::same(theme.radii.control);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), RECENT_ROW_H),
        egui::Sense::click(),
    );
    let remove_zone = egui::Rect::from_min_max(
        egui::pos2(rect.right() - RECENT_REMOVE_W, rect.top()),
        rect.max,
    );
    // A separate click response makes the remove affordance a real keyboard
    // focus stop instead of a pointer-only region inside the open row.
    let remove = ui.interact(
        remove_zone,
        response.id.with("remove"),
        egui::Sense::click(),
    );
    let body_rect =
        egui::Rect::from_min_max(rect.min, egui::pos2(remove_zone.left(), rect.bottom()));
    let hovering_remove = remove.hovered() || remove.is_pointer_button_down_on();
    let hovering_body = response.hovered() && !hovering_remove;
    let pressed = response.is_pointer_button_down_on();

    if hovering_body {
        ui.painter().rect_filled(
            body_rect,
            radius,
            if pressed {
                theme.colors.widget_active_fill.gamma_multiply(0.5)
            } else {
                theme.colors.widget_hover_fill.gamma_multiply(0.5)
            },
        );
    }
    if hovering_remove {
        ui.painter().rect_filled(
            remove_zone,
            radius,
            if remove.is_pointer_button_down_on() {
                theme.colors.widget_active_fill
            } else {
                theme.colors.widget_hover_fill
            },
        );
    }

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let full = path.display().to_string();
    let text_w = (body_rect.width() - 2.0 * RECENT_ROW_PAD).max(0.0);
    let name_galley = truncated(ui, &name, egui::FontId::proportional(13.0), text_w);
    let name_h = name_galley.size().y;
    let path_galley = truncated(ui, &full, egui::FontId::proportional(11.0), text_w);
    let total_h = name_h + RECENT_TEXT_GAP + path_galley.size().y;
    let top = rect.center().y - total_h * 0.5;
    ui.painter().galley(
        egui::pos2(rect.left() + RECENT_ROW_PAD, top),
        name_galley,
        theme.colors.text_strong,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + RECENT_ROW_PAD, top + name_h + RECENT_TEXT_GAP),
        path_galley,
        theme.colors.text_muted,
    );

    window_glyph(
        ui,
        egui::Rect::from_center_size(remove_zone.center(), egui::Vec2::splat(RECENT_REMOVE_ICON)),
        WindowGlyph::Close,
        if remove.is_pointer_button_down_on() && hovering_remove {
            theme.colors.accent
        } else if hovering_remove {
            theme.colors.text_strong
        } else {
            theme.colors.text_muted
        },
    );

    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    if remove.has_focus() {
        ui.painter().rect_stroke(
            remove_zone,
            radius,
            egui::Stroke::new(theme.strokes.accent, theme.colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    if response.hovered() || remove.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let _ = remove.clone().on_hover_text(t("landing.recent.remove"));

    let keyboard_open = response.has_focus()
        && ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Space)
        });
    let keyboard_remove = remove.has_focus()
        && ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Space)
        });
    let remove_clicked = remove.clicked() || keyboard_remove;
    let open_clicked = (response.clicked() && !remove_clicked) || keyboard_open;
    (open_clicked, remove_clicked)
}

/// Lays `text` out on one line, truncating it to `max_width`.
fn truncated(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, egui::Color32::PLACEHOLDER);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    ui.painter().layout_job(job)
}
