//! Touch-pan momentum (inertia) math.
//!
//! While two fingers pan, the centroid position is sampled with its frame time.
//! On release the recent samples give a velocity (points/second); above a small
//! threshold the scroll continues with an exponential decay (`v *= d^(dt*60)`,
//! i.e. `d` per 60 Hz tick) until the speed drops below the stop threshold.
//! Wheel input does not use this module: egui already supplies an already-
//! smoothed per-frame `smooth_scroll_delta`. The math is pure so it can be unit
//! tested without a `Ui`.

use egui::Vec2;

/// Samples older than this are ignored when estimating the release velocity.
pub(super) const VELOCITY_WINDOW: f64 = 0.080;
/// Velocity multiplier per 60 Hz frame of the decay.
pub(super) const DECAY_PER_TICK: f32 = 0.92;
/// Momentum stops once the speed drops below this many points per second.
pub(super) const STOP_SPEED: f32 = 20.0;
/// Release velocity (points/second) from `(time, centroid)` samples, using the
/// oldest and newest samples within [`VELOCITY_WINDOW`] of `now`.
pub(super) fn velocity(samples: &[(f64, Vec2)], now: f64) -> Vec2 {
    let cutoff = now - VELOCITY_WINDOW;
    let mut first: Option<(f64, Vec2)> = None;
    let mut last: Option<(f64, Vec2)> = None;
    for (time, pos) in samples {
        if *time < cutoff {
            continue;
        }
        first.get_or_insert((*time, *pos));
        last = Some((*time, *pos));
    }
    let (Some((t0, p0)), Some((t1, p1))) = (first, last) else {
        return Vec2::ZERO;
    };
    let dt = t1 - t0;
    if dt <= 0.0 {
        return Vec2::ZERO;
    }
    (p1 - p0) / dt as f32
}

/// One exponential decay step over `dt` seconds: `v * factor^(dt * 60)`.
pub(super) fn decay(velocity: Vec2, dt: f32, factor: f32) -> Vec2 {
    let ticks = dt.clamp(0.0, 0.1) * 60.0;
    velocity * factor.clamp(0.0, 1.0).powf(ticks)
}

/// Whether `velocity` is still fast enough to keep coasting.
pub(super) fn is_moving(velocity: Vec2) -> bool {
    velocity.length() >= STOP_SPEED
}

/// Drops samples older than [`VELOCITY_WINDOW`], keeping at least two so the
/// span used by [`velocity`] stays non-empty.
pub(super) fn trim(samples: &mut Vec<(f64, Vec2)>, now: f64) {
    let cutoff = now - VELOCITY_WINDOW;
    while samples.len() > 2 && samples[0].0 < cutoff {
        samples.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    #[test]
    fn velocity_uses_the_window_endpoints() {
        let samples = [
            (0.0, Vec2::ZERO),
            (0.05, vec2(0.0, -10.0)),
            (0.1, vec2(0.0, -30.0)),
        ];
        let v = velocity(&samples, 0.1);
        assert!(v.x.abs() < 1e-4);
        assert!((v.y + 400.0).abs() < 1e-3, "got {v:?}");
    }

    #[test]
    fn velocity_is_zero_without_two_samples_in_the_window() {
        assert_eq!(velocity(&[], 1.0), Vec2::ZERO);
        assert_eq!(velocity(&[(0.0, vec2(5.0, 5.0))], 1.0), Vec2::ZERO);
    }

    #[test]
    fn velocity_ignores_samples_before_the_window() {
        let samples = [
            (0.0, Vec2::ZERO),
            (0.95, vec2(0.0, -10.0)),
            (1.0, vec2(0.0, -20.0)),
        ];
        let v = velocity(&samples, 1.0);
        assert!((v.y + 200.0).abs() < 1e-3, "got {v:?}");
    }

    #[test]
    fn decay_drops_by_the_factor_each_60hz_tick() {
        let out = decay(vec2(100.0, 0.0), 1.0 / 60.0, DECAY_PER_TICK);
        assert!((out.x - 92.0).abs() < 0.01, "got {out:?}");
    }

    #[test]
    fn decay_is_identity_for_zero_dt() {
        assert_eq!(decay(vec2(3.0, -4.0), 0.0, DECAY_PER_TICK), vec2(3.0, -4.0));
    }

    #[test]
    fn is_moving_uses_the_stop_threshold() {
        assert!(!is_moving(vec2(STOP_SPEED - 0.1, 0.0)));
        assert!(is_moving(vec2(STOP_SPEED, 0.0)));
    }

    #[test]
    fn trim_keeps_at_least_two_samples() {
        let mut samples = vec![
            (0.0, Vec2::ZERO),
            (0.5, Vec2::ZERO),
            (1.0, Vec2::ZERO),
            (1.1, Vec2::ZERO),
        ];
        trim(&mut samples, 1.1);
        assert!(samples.len() >= 2);
        assert!(samples.last().is_some_and(|(time, _)| *time == 1.1));
    }
}
