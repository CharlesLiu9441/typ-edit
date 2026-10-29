//! Shared overlay scrollbar with egui's floating-scrollbar look (thin at rest,
//! widening while hovered) and a common drag interaction. Used by the preview
//! canvas and by the tab strip's overflow scrollbar.

use egui::{CornerRadius, Id, Rect, Sense, Ui};

/// Upper bound for the bar's widened width, in points: egui's floating preset
/// widens to `bar_width` (10), which reads too heavy over the canvas/pages.
pub const MAX_WIDTH: f32 = 6.0;

/// Draws one floating scrollbar in `track` and returns the position it was
/// dragged to.
///
/// `track` is both the interaction zone and the bar's length; the bar is drawn
/// centred across the track's cross axis, widening from the style's
/// `floating_width` to [`MAX_WIDTH`] while hovered (the colours, opacities and
/// minimum handle length likewise come from [`egui::style::ScrollStyle`]).
/// `frac` is the scroll position (0.0 = start), `visible` the share of the
/// track the viewport covers, and `outer_hovered` means the pointer is over the
/// owning area (e.g. the canvas), which fades the resting bar in. With
/// `always_visible` the handle keeps the style's `active_handle_opacity` at
/// rest instead of fading out entirely (for strips whose bar is the only hint).
///
/// Dragging follows egui's scrollbar convention: pressing the handle keeps the
/// pointer's grab offset, pressing anywhere else centres the handle on the
/// pointer first; either way the handle then follows the pointer while held.
pub struct ScrollbarInput {
    pub id: Id,
    pub track: Rect,
    pub horizontal: bool,
    pub frac: f32,
    pub visible: f32,
    pub outer_hovered: bool,
    pub always_visible: bool,
}

pub fn scrollbar(ui: &mut Ui, input: ScrollbarInput) -> Option<f32> {
    let ScrollbarInput {
        id,
        track,
        horizontal,
        frac,
        visible,
        outer_hovered,
        always_visible,
    } = input;
    let (
        floating_width,
        bar_width,
        handle_min_length,
        foreground_color,
        dormant_handle_opacity,
        active_handle_opacity,
        interact_handle_opacity,
        dormant_background_opacity,
        active_background_opacity,
        interact_background_opacity,
    ) = {
        let style = &ui.style().spacing.scroll;
        (
            style.floating_width,
            style.bar_width,
            style.handle_min_length,
            style.foreground_color,
            style.dormant_handle_opacity,
            style.active_handle_opacity,
            style.interact_handle_opacity,
            style.dormant_background_opacity,
            style.active_background_opacity,
            style.interact_background_opacity,
        )
    };

    let track_len = if horizontal {
        track.width()
    } else {
        track.height()
    };
    if !track_len.is_finite() || track_len <= 0.0 {
        return None;
    }
    let frac = frac.clamp(0.0, 1.0);
    let track_start = if horizontal { track.min.x } else { track.min.y };
    let handle_len =
        (track_len * visible.clamp(0.0, 1.0)).clamp(handle_min_length.min(track_len), track_len);
    let travel = track_len - handle_len;
    let handle_start = track_start + frac * travel;

    // tui-whitelist: 命中区 interact（滑轨/滑块为纯绘制几何，不参与子元素排布）。
    let response = ui.interact(track, id, Sense::click_and_drag());
    let hovering_bar = response.hovered() || response.dragged();
    let widen = ui
        .ctx()
        .animate_bool_responsive(id.with("bar_hover"), hovering_bar);
    let width = egui::lerp(floating_width..=bar_width.min(MAX_WIDTH), widen);
    let outer = ui
        .ctx()
        .animate_bool_responsive(id.with("outer_hover"), outer_hovered);
    let handle_opacity = if hovering_bar {
        interact_handle_opacity
    } else if always_visible {
        active_handle_opacity
    } else {
        egui::lerp(dormant_handle_opacity..=active_handle_opacity, outer)
    };
    let background_opacity = if hovering_bar {
        interact_background_opacity
    } else if outer_hovered {
        active_background_opacity
    } else {
        dormant_background_opacity
    };

    // The grab offset is computed once when the press starts and kept for the
    // rest of the hold, so pressing the handle does not jump it.
    let grab_id = id.with("grab");
    let mut new_frac = None;
    if travel > 0.0 {
        if let Some(pointer) = response.interact_pointer_pos() {
            let coordinate = if horizontal { pointer.x } else { pointer.y };
            let grab = match ui.ctx().memory(|m| m.data.get_temp::<f32>(grab_id)) {
                Some(grab) => grab,
                None => {
                    let grab =
                        if coordinate >= handle_start && coordinate <= handle_start + handle_len {
                            coordinate - handle_start
                        } else {
                            let start = (coordinate - handle_len * 0.5)
                                .clamp(track_start, track_start + travel);
                            coordinate - start
                        };
                    ui.ctx().memory_mut(|m| m.data.insert_temp(grab_id, grab));
                    grab
                }
            };
            let start = (coordinate - grab).clamp(track_start, track_start + travel);
            new_frac = Some((start - track_start) / travel);
        } else {
            ui.ctx().memory_mut(|m| m.data.remove_temp::<f32>(grab_id));
        }
    }

    // Paint (egui's floating look): a background strip of the animated width
    // across the whole track, then the handle on top.
    let cross_center = if horizontal {
        track.center().y
    } else {
        track.center().x
    };
    let bar_rect = if horizontal {
        Rect::from_min_max(
            egui::pos2(track.min.x, cross_center - width * 0.5),
            egui::pos2(track.max.x, cross_center + width * 0.5),
        )
    } else {
        Rect::from_min_max(
            egui::pos2(cross_center - width * 0.5, track.min.y),
            egui::pos2(cross_center + width * 0.5, track.max.y),
        )
    };
    let handle_rect = if horizontal {
        Rect::from_min_max(
            egui::pos2(handle_start, bar_rect.min.y),
            egui::pos2(handle_start + handle_len, bar_rect.max.y),
        )
    } else {
        Rect::from_min_max(
            egui::pos2(bar_rect.min.x, handle_start),
            egui::pos2(bar_rect.max.x, handle_start + handle_len),
        )
    };

    let visuals = ui.visuals();
    let hovering_handle = response.hovered()
        && ui.input(|i| {
            i.pointer
                .latest_pos()
                .is_some_and(|p| handle_rect.contains(p))
        });
    let widget = if response.is_pointer_button_down_on() {
        &visuals.widgets.active
    } else if hovering_handle {
        &visuals.widgets.hovered
    } else {
        &visuals.widgets.inactive
    };
    let handle_color = (if foreground_color {
        widget.fg_stroke.color
    } else {
        widget.bg_fill
    })
    .gamma_multiply(handle_opacity);
    let background_color = ui
        .visuals()
        .extreme_bg_color
        .gamma_multiply(background_opacity);
    let radius: CornerRadius = widget.corner_radius;

    ui.painter().rect_filled(bar_rect, radius, background_color);
    ui.painter().rect_filled(handle_rect, radius, handle_color);
    new_frac
}
