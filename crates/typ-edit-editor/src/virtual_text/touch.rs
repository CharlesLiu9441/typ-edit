//! Two-finger touch panning for the virtualized text path.
//!
//! egui delivers raw [`egui::Event::Touch`] events (no synthesized scroll), so
//! this module tracks the active fingers in [`VirtualTextState`] itself. While
//! two or more fingers are down the pointer/selection pass is suppressed; a
//! single finger is left to the pointer path so tapping still selects.
//!
//! Content follows the fingers: a centroid delta `d` scrolls by `-d`, the same
//! sign convention as [`egui::InputState::smooth_scroll_delta`] (a positive
//! delta moves the content right/down). On release the recent centroid samples
//! give a velocity (see [`super::momentum`]); above the stop threshold the pan
//! coasts with exponential decay on both axes until it settles.

use egui::{Event, Pos2, TouchPhase, Vec2, pos2};

use super::VirtualTextState;
use super::chrome;
use super::layout::LineData;
use super::momentum;
use crate::WrapMode;

/// Applies this frame's touch events: pans on a two-finger gesture and coasts
/// after release.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    viewport_rect: egui::Rect,
    wrap_mode: WrapMode,
    line_count: usize,
    viewport_h: f32,
    now: f64,
) {
    let events = ui.input(|i| i.events.clone());
    for event in &events {
        if let Event::Touch { id, phase, pos, .. } = event {
            match phase {
                TouchPhase::Start | TouchPhase::Move => {
                    match state.touches.iter_mut().find(|(touch, _)| *touch == *id) {
                        Some(slot) => slot.1 = *pos,
                        None => state.touches.push((*id, *pos)),
                    }
                }
                TouchPhase::End | TouchPhase::Cancel => {
                    state.touches.retain(|(touch, _)| *touch != *id);
                }
            }
        }
    }

    let horizontal = !matches!(wrap_mode, WrapMode::Always);
    if state.touches.len() < 2 {
        // Inertia only once every finger has lifted; a lone remaining finger
        // hands the gesture back to the pointer path without coasting.
        if state.touch_panning && state.touches.is_empty() {
            let released = momentum::velocity(&state.touch_samples, now);
            state.momentum = momentum::is_moving(released).then_some(released);
            // Start the clock at the last move sample, not at the release
            // event: coasting on the release frame with the full inter-frame
            // step removes the one-frame dead spot that read as a stutter at
            // the start of inertia.
            state.momentum_time = state
                .touch_samples
                .last()
                .map(|(time, _)| *time)
                .unwrap_or(now)
                .min(now);
        }
        state.touch_panning = false;
        state.touch_centroid = None;
        state.multi_touch = false;
        state.touch_samples.clear();
        coast(state, ui, data, horizontal, line_count, viewport_h, now);
        return;
    }

    // A fresh two-finger pan cancels any coasting from the previous one.
    state.momentum = None;
    let centroid = centroid(&state.touches);
    if !state.touch_panning && viewport_rect.contains(centroid) {
        state.touch_panning = true;
        state.touch_centroid = None;
    }
    state.multi_touch = state.touch_panning;
    if !state.touch_panning {
        return;
    }
    if let Some(previous) = state.touch_centroid {
        let delta = centroid - previous;
        chrome::scroll_vertical(state, ui, data, line_count, viewport_h, -delta.y);
        if horizontal {
            state.h_offset = (state.h_offset - delta.x).clamp(0.0, state.max_h_offset);
        }
        state.last_scroll_time = now;
    }
    state.touch_centroid = Some(centroid);
    state.touch_samples.push((now, centroid.to_vec2()));
    momentum::trim(&mut state.touch_samples, now);
    ui.ctx().request_repaint();
}

/// Continues a released pan with exponential decay until the speed drops below
/// the stop threshold. Each step counts as scrolling, so the anchor is only
/// normalized once the coast stops.
#[allow(clippy::too_many_arguments)]
fn coast(
    state: &mut VirtualTextState,
    ui: &egui::Ui,
    data: &LineData,
    horizontal: bool,
    line_count: usize,
    viewport_h: f32,
    now: f64,
) {
    let Some(velocity) = state.momentum else {
        return;
    };
    let dt = ((now - state.momentum_time).max(0.0) as f32).min(0.1);
    let next = momentum::decay(velocity, dt, momentum::DECAY_PER_TICK);
    chrome::scroll_vertical(state, ui, data, line_count, viewport_h, -next.y * dt);
    if horizontal {
        state.h_offset = (state.h_offset - next.x * dt).clamp(0.0, state.max_h_offset);
    }
    state.momentum_time = now;
    if momentum::is_moving(next) {
        state.last_scroll_time = now;
        state.momentum = Some(next);
        ui.ctx().request_repaint();
    } else {
        // Do not stamp the stop frame as a scroll: the scroll pass that follows
        // this call sees an idle frame with no momentum and normalizes the
        // anchor immediately, so layout settles without a timer wake-up.
        state.momentum = None;
    }
}

fn centroid(touches: &[(egui::TouchId, Pos2)]) -> Pos2 {
    let count = touches.len() as f32;
    let sum = touches
        .iter()
        .map(|(_, pos)| pos.to_vec2())
        .fold(Vec2::ZERO, |acc, v| acc + v);
    pos2(sum.x / count, sum.y / count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_text::layout::LineData;

    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let chain = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .insert(egui::FontFamily::Name("sans".into()), chain);
        ctx.set_fonts(fonts);
        ctx
    }

    fn data<'a>(buffer: &'a typ_edit_buffer::TextBuffer) -> LineData<'a> {
        LineData {
            buffer,
            text_rev: 1,
            parse_generation: 0,
            highlights: &[],
            blocked_ranges: &[],
            code_ranges: &[],
            math_ranges: &[],
            math_atoms: &[],
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            wrap_width: f32::INFINITY,
        }
    }

    #[test]
    fn momentum_coast_clamps_h_offset_to_the_shared_max() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\nb");
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let mut state = VirtualTextState {
                max_h_offset: 100.0,
                h_offset: 90.0,
                momentum: Some(Vec2::new(-10_000.0, 0.0)),
                momentum_time: 0.0,
                ..VirtualTextState::default()
            };

            coast(&mut state, ui, &data, true, 2, 400.0, 0.05);

            assert_eq!(
                state.h_offset, 100.0,
                "momentum cannot overshoot the shared max"
            );
        })
        .drop_without_applying_deltas();
    }
}
