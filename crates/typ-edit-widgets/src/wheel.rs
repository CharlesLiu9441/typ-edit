//! Frame-paced resampling for trackpad wheel input.
//!
//! On Windows, winit reports a precision touchpad as a stream of
//! [`egui::MouseWheelUnit::Line`] events with [`egui::TouchPhase::Move`]. There is
//! no start/end phase and no pixel-precise delta; the sub-detent stream is
//! indistinguishable from a slow wheel at the event layer. egui treats line
//! events as discrete wheel notches and low-pass filters them, so when the
//! stream arrives every other frame the filtered output alternates between a
//! large and a small step (roughly 50% amplitude modulation at 30 Hz).
//!
//! This resampler is only enabled after a trackpad-shaped stream is detected:
//! a fractional line delta (sub-detent scroll) or another raw wheel event
//! within [`TRACKPAD_EVENT_GAP`]. It consumes egui's already-smoothed delta and
//! runs it through a four-frame box filter and a longer exponential release
//! queue. The release time constant follows the observed event interval so
//! short gaps between OS event batches do not drain to zero and restart; the
//! queue intentionally keeps the residual moving instead of clamping it at a
//! fixed frame boundary. Ordinary one-notch mouse wheel input stays on egui's
//! smoother untouched.
//!
//! The resampler is idempotent within one egui frame: the second multipass
//! (when a layout pass requests a discard) must not advance the filter again.

use egui::{Event, MouseWheelUnit, Vec2};

/// Two raw wheel events closer together than this are treated as one continuous
/// precision-touchpad stream.
const TRACKPAD_EVENT_GAP: f64 = 0.12;

/// After this long without a raw event and with the release queue drained, the
/// next wheel notch is treated as an isolated mouse notch again.
const TRACKPAD_IDLE: f64 = 1.0;

/// Longest release time constant used to bridge gaps between event batches.
const RELEASE_TAU_MAX: f32 = 0.25;

/// One frame of scroll input after resampling.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelStep {
    /// Scroll to apply this frame.
    pub delta: Vec2,
    /// Whether the filter still has non-zero history for later frames.
    pub active: bool,
}

/// Converts an event-cadence wheel stream into a frame-cadence stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct WheelResampler {
    history: [Vec2; 4],
    history_sum: Vec2,
    history_index: usize,
    pending: Vec2,
    last_raw: Option<f64>,
    interval: f64,
    tracking: bool,
    last_frame: Option<u64>,
}

impl WheelResampler {
    /// Drops the filter history. Call when the pointer leaves the scroll surface
    /// so a later hover cannot replay motion that arrived while it was away.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Consumes one frame's `smooth_scroll_delta` and returns what should be
    /// applied now. `smooth` is egui's filtered hand-off, not a raw event.
    /// `take` masks the axes this scroll surface owns; a non-owned axis stays
    /// with the parent and must not accumulate in the filter.
    ///
    /// Call at most once per frame for a given scroll surface; the second
    /// multipass receives no fresh delta and must not advance the filter.
    pub fn step(&mut self, ui: &egui::Ui, smooth: Vec2, take: Vec2) -> WheelStep {
        let frame = ui.ctx().cumulative_frame_nr();
        let (now, dt, raw, line, precise) = ui.input(|input| {
            let mut raw = false;
            let mut line = false;
            let mut precise = false;
            for event in &input.events {
                let Event::MouseWheel { unit, delta, .. } = event else {
                    continue;
                };
                if *delta == Vec2::ZERO {
                    continue;
                }
                raw = true;
                if matches!(unit, MouseWheelUnit::Line) {
                    line = true;
                    if delta.x.fract() != 0.0 || delta.y.fract() != 0.0 {
                        // Sub-detent line deltas are the Windows precision
                        // touchpad shape; external notches are integer lines.
                        precise = true;
                    }
                }
            }
            (
                input.time,
                input.stable_dt.clamp(0.001, 0.1),
                raw,
                line,
                precise,
            )
        });

        if self.last_frame == Some(frame) {
            // A second layout pass receives no fresh delta; never advance twice.
            return WheelStep {
                delta: Vec2::ZERO,
                active: self.pending != Vec2::ZERO || self.history_sum != Vec2::ZERO,
            };
        }
        self.last_frame = Some(frame);

        if raw {
            let previous = self.last_raw;
            let follows_recent = previous.is_some_and(|last| now - last <= TRACKPAD_EVENT_GAP);
            if precise || (line && follows_recent) {
                self.tracking = true;
            }
            if let Some(last) = previous {
                let gap = now - last;
                if gap > 0.0 {
                    self.interval = if self.interval == 0.0 {
                        gap
                    } else {
                        self.interval * 0.5 + gap * 0.5
                    }
                    .clamp(0.008, 0.12);
                }
            }
            self.last_raw = Some(now);
        }

        if !self.tracking {
            self.history = [Vec2::ZERO; 4];
            self.history_sum = Vec2::ZERO;
            self.history_index = 0;
            self.pending = Vec2::ZERO;
            return WheelStep {
                delta: smooth,
                active: false,
            };
        }

        let input = smooth * take;
        let old = self.history[self.history_index];
        self.history[self.history_index] = input;
        self.history_sum += input - old;
        self.history_index = (self.history_index + 1) % self.history.len();
        self.pending += self.history_sum / self.history.len() as f32;
        let tau = if self.interval > 0.0 {
            (self.interval as f32 * 4.0).clamp(0.08, RELEASE_TAU_MAX)
        } else {
            0.15
        };
        let release = 1.0 - (-dt / tau).exp();
        let delta = self.pending * release;
        self.pending -= delta;
        if self.pending.abs().max_elem() < 0.001 {
            self.pending = Vec2::ZERO;
        }
        if self.pending == Vec2::ZERO
            && self.history_sum == Vec2::ZERO
            && self.last_raw.is_none_or(|last| now - last > TRACKPAD_IDLE)
        {
            self.tracking = false;
        }
        WheelStep {
            delta,
            active: self.pending != Vec2::ZERO || self.history_sum != Vec2::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_cadence(gap: usize, frames: usize) -> Vec<Vec2> {
        let ctx = egui::Context::default();
        let mut resampler = WheelResampler::default();
        let mut output = Vec::new();
        for frame in 0..frames {
            let events = if frame % gap == 0 {
                vec![Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -1.0 / 120.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }]
            } else {
                Vec::new()
            };
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(frame as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                let smooth = ui.input(|input| input.smooth_scroll_delta);
                let step = resampler.step(ui, smooth, Vec2::splat(1.0));
                output.push(step.delta);
                ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
                ui.ctx().request_repaint();
            });
            out.drop_without_applying_deltas();
        }
        output
    }

    #[test]
    fn a_thirty_hz_line_stream_has_no_alternating_dead_frames() {
        let output = run_cadence(2, 50);
        let steady: Vec<f32> = output[20..40].iter().map(|delta| -delta.y).collect();
        assert!(
            steady.iter().all(|step| *step > 0.0),
            "every steady-state frame must scroll, got {steady:?}"
        );
        assert!(
            steady.windows(2).all(|pair| pair[1] >= pair[0] - 0.0001),
            "the decay must not restart from zero: {steady:?}"
        );
    }

    #[test]
    fn an_unowned_axis_never_stays_pending() {
        let ctx = egui::Context::default();
        let mut resampler = WheelResampler::default();
        for frame in 0..4 {
            let events = vec![Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(-0.25, 0.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            }];
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(frame as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                let smooth = ui.input(|input| input.smooth_scroll_delta);
                let step = resampler.step(ui, smooth, egui::vec2(0.0, 1.0));
                assert_eq!(step.delta, Vec2::ZERO);
                assert!(!step.active);
                ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
            });
            out.drop_without_applying_deltas();
        }
    }

    #[test]
    fn a_burst_decays_without_a_late_restart() {
        let ctx = egui::Context::default();
        let mut resampler = WheelResampler::default();
        let mut output = Vec::new();
        for frame in 0..30 {
            let events = if frame < 3 {
                vec![Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -1.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }]
            } else {
                Vec::new()
            };
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(frame as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                let smooth = ui.input(|input| input.smooth_scroll_delta);
                output.push(resampler.step(ui, smooth, Vec2::splat(1.0)).delta);
                ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
            });
            out.drop_without_applying_deltas();
        }
        assert!(
            output[8..20].iter().all(|delta| *delta != Vec2::ZERO),
            "the decay must bridge the gap instead of stopping and restarting: {output:?}"
        );
        for pair in output[8..].windows(2) {
            assert!(
                pair[1].y >= pair[0].y - 0.01,
                "the burst must not grow again after its input stopped: {output:?}"
            );
        }
    }

    #[test]
    fn an_isolated_mouse_notch_is_not_delayed_by_the_queue() {
        let ctx = egui::Context::default();
        let mut resampler = WheelResampler::default();
        let mut output = Vec::new();
        for frame in 0..4 {
            let events = if frame == 0 {
                vec![Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -1.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }]
            } else {
                Vec::new()
            };
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(frame as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                let smooth = ui.input(|input| input.smooth_scroll_delta);
                output.push(resampler.step(ui, smooth, Vec2::splat(1.0)).delta);
                ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
            });
            out.drop_without_applying_deltas();
        }
        assert_eq!(
            output[0].y, -12.748_316,
            "the first isolated notch must use egui's own first step"
        );
    }
}
