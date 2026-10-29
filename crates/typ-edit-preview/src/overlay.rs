//! Light-blue fading ring overlay: a short-lived marker drawn at a page-local point,
//! fading from opaque to transparent over [`Ring::LIFETIME`].

use std::time::Instant;

/// A ring shown at a page-local point, fading out over [`Ring::LIFETIME`].
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    pub page: usize,
    pub point_pt: (f32, f32),
    pub start: Instant,
}

impl Ring {
    pub const LIFETIME: std::time::Duration = std::time::Duration::from_secs(2);

    pub fn new(page: usize, point_pt: (f32, f32)) -> Self {
        Self {
            page,
            point_pt,
            start: Instant::now(),
        }
    }

    /// `1.0 -> 0.0` over the lifetime; `None` when finished.
    pub fn alpha(&self) -> Option<f32> {
        let t = self.start.elapsed().as_secs_f32() / Self::LIFETIME.as_secs_f32();
        if t >= 1.0 { None } else { Some(1.0 - t) }
    }
}
