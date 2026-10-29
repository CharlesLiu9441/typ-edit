//! Segmented selector: a row of mutually-exclusive options inside one rounded
//! track; the selected option is circled by an accent rounded-rect outline with
//! the same corner radius as the tab silhouettes. Presentation-only: returns
//! whether the caller's selection changed.

use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2};

use crate::tab_bar::TAB_RADIUS;
use crate::theme::Theme;

/// Corner radius of one option and of its accent ring (matches the tabs).
const OPTION_RADIUS: u8 = TAB_RADIUS;
/// Padding from the track edge to the option row; the track radius nests one step out.
const TRACK_PAD: f32 = 2.0;
/// Horizontal padding inside one option, around its label.
const OPTION_PAD_X: f32 = 12.0;
/// Gap between neighbouring options.
const OPTION_GAP: f32 = 2.0;
/// Height of the track (and of each option).
const HEIGHT: f32 = 28.0;
/// Thickness of the selected option's accent ring.
const RING_W: f32 = 2.0;

/// Draws a segmented selector over `options` and returns whether the selection
/// changed.
///
/// Every option is a `(value, label)` pair; the options are mutually exclusive
/// and laid out left to right inside one rounded track. The selected option is
/// circled by an accent rounded-rect outline (no fill), matching the tab
/// silhouettes; a hovered unselected option gets the tab bar's translucent
/// hover fill. Labels use the body font and the same colour language as the
/// tabs (selected accent, hovered full text, otherwise weak text).
///
/// The widget is presentation-only: a click on an option other than the current
/// one writes it into `selected` and returns `true`; every other click and an
/// empty `options` slice return `false` without touching the state.
pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    options: &[(T, &str)],
    selected: &mut T,
) -> bool {
    if options.is_empty() {
        return false;
    }

    let font = TextStyle::Body.resolve(ui.style());
    let theme = Theme::from_ui(ui);
    let galleys: Vec<Arc<egui::Galley>> = options
        .iter()
        .map(|(_, label)| {
            let job = egui::text::LayoutJob::simple_singleline(
                (*label).to_owned(),
                font.clone(),
                Color32::PLACEHOLDER,
            );
            ui.painter().layout_job(job)
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .map(|galley| galley.size().x + 2.0 * OPTION_PAD_X)
        .collect();
    let track_w =
        widths.iter().sum::<f32>() + OPTION_GAP * (options.len() - 1) as f32 + 2.0 * TRACK_PAD;

    // tui-whitelist: 单叶 allocate_exact_size（整条分段控件 + 命中区），选项矩形为纯绘制几何。
    let (track, response) = ui.allocate_exact_size(Vec2::new(track_w, HEIGHT), Sense::click());
    let rects = option_rects(track, &widths);
    let visuals = ui.visuals().clone();
    let hovered = ui
        .input(|i| i.pointer.hover_pos())
        .and_then(|pos| hit(&rects, pos));

    let painter = ui.painter();
    painter.rect_filled(
        track,
        CornerRadius::same(OPTION_RADIUS + TRACK_PAD as u8),
        visuals.widgets.inactive.bg_fill,
    );
    for (index, rect) in rects.iter().enumerate() {
        let is_selected = options[index].0 == *selected;
        let is_hovered = hovered == Some(index);
        if is_selected {
            painter.rect_stroke(
                *rect,
                CornerRadius::same(OPTION_RADIUS),
                Stroke::new(RING_W, visuals.hyperlink_color),
                StrokeKind::Inside,
            );
        } else if is_hovered {
            painter.rect_filled(
                *rect,
                CornerRadius::same(OPTION_RADIUS),
                visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
            );
        }
        let color = if is_selected {
            visuals.widgets.active.fg_stroke.color
        } else if is_hovered {
            visuals.text_color()
        } else {
            theme.colors.text_muted
        };
        let galley = galleys[index].clone();
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            color,
        );
    }
    if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.has_focus() {
        painter.rect_stroke(
            track,
            CornerRadius::same(OPTION_RADIUS + TRACK_PAD as u8),
            Stroke::new(RING_W, theme.colors.accent),
            StrokeKind::Inside,
        );
    }

    if response.has_focus() && !options.is_empty() {
        let mut step = 0isize;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                step -= 1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                step += 1;
            }
        });
        if step != 0 {
            let current = options
                .iter()
                .position(|(value, _)| value == selected)
                .unwrap_or(0);
            let next = (current as isize + step).rem_euclid(options.len() as isize) as usize;
            if options[next].0 != *selected {
                *selected = options[next].0;
                return true;
            }
        }
    }

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(index) = hit(&rects, pos)
        && options[index].0 != *selected
    {
        *selected = options[index].0;
        return true;
    }
    false
}

/// Option rects inside `track`, left to right: each is `HEIGHT - 2*TRACK_PAD`
/// tall, inset by `TRACK_PAD`, separated by `OPTION_GAP`.
pub(crate) fn option_rects(track: Rect, widths: &[f32]) -> Vec<Rect> {
    let mut x = track.left() + TRACK_PAD;
    widths
        .iter()
        .map(|width| {
            let rect = Rect::from_min_size(
                Pos2::new(x, track.top() + TRACK_PAD),
                Vec2::new(*width, HEIGHT - 2.0 * TRACK_PAD),
            );
            x += width + OPTION_GAP;
            rect
        })
        .collect()
}

/// Index of the option containing `pos`, if any (gaps and the track padding are
/// dead zones).
pub(crate) fn hit(rects: &[Rect], pos: Pos2) -> Option<usize> {
    rects.iter().position(|rect| rect.contains(pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical two-option track used by both helper tests.
    fn two_option_rects() -> Vec<Rect> {
        let track = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(100.0, HEIGHT));
        option_rects(track, &[30.0, 40.0])
    }

    #[test]
    fn option_rects_places_options_left_to_right() {
        let rects = two_option_rects();
        assert_eq!(rects.len(), 2);
        assert_eq!(
            rects[0],
            Rect::from_min_max(Pos2::new(2.0, 2.0), Pos2::new(32.0, 26.0)),
            "first option starts at TRACK_PAD and is HEIGHT - 2*TRACK_PAD tall"
        );
        assert_eq!(
            rects[1],
            Rect::from_min_max(Pos2::new(34.0, 2.0), Pos2::new(74.0, 26.0)),
            "second option follows after its width plus OPTION_GAP"
        );
    }

    #[test]
    fn hit_selects_the_option_under_the_pointer() {
        let rects = two_option_rects();
        assert_eq!(hit(&rects, Pos2::new(17.0, 14.0)), Some(0), "first centre");
        assert_eq!(hit(&rects, Pos2::new(54.0, 14.0)), Some(1), "second centre");
        assert_eq!(
            hit(&rects, Pos2::new(33.0, 14.0)),
            None,
            "the gap between options is a dead zone"
        );
        assert_eq!(
            hit(&rects, Pos2::new(90.0, 14.0)),
            None,
            "the track padding is a dead zone"
        );
    }
}
