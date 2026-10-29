//! A conservative 2D axis-aligned box in points (y down).

/// A conservative axis-aligned box.
///
/// Callers construct boxes that over-approximate their ink; every operation
/// here preserves that property, so culling by [`Extents::intersects`] can never
/// drop a visible item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extents {
    /// Left edge.
    pub x0: f64,
    /// Right edge.
    pub x1: f64,
    /// Top edge.
    pub y0: f64,
    /// Bottom edge.
    pub y1: f64,
}

impl Extents {
    /// Builds extents from a top-left corner and a size.
    pub const fn from_rect(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x0: x,
            x1: x + width,
            y0: y,
            y1: y + height,
        }
    }

    /// Whether the two closed boxes overlap.
    pub fn intersects(self, other: Self) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    /// Smallest box containing both inputs.
    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            x1: self.x1.max(other.x1),
            y0: self.y0.min(other.y0),
            y1: self.y1.max(other.y1),
        }
    }

    /// Largest box contained in both inputs.
    ///
    /// When the inputs do not overlap, the box collapses to a zero-area point
    /// (still a conservative over-approximation of the empty intersection).
    pub fn intersection(self, other: Self) -> Self {
        let x0 = self.x0.max(other.x0);
        let x1 = self.x1.min(other.x1).max(x0);
        let y0 = self.y0.max(other.y0);
        let y1 = self.y1.min(other.y1).max(y0);
        Self { x0, x1, y0, y1 }
    }

    /// Width of the box.
    pub fn width(self) -> f64 {
        self.x1 - self.x0
    }

    /// Height of the box.
    pub fn height(self) -> f64 {
        self.y1 - self.y0
    }

    /// Horizontal centre, used by the BVH split.
    pub fn center_x(self) -> f64 {
        0.5 * (self.x0 + self.x1)
    }

    /// Vertical centre, used by the BVH split.
    pub fn center_y(self) -> f64 {
        0.5 * (self.y0 + self.y1)
    }

    /// Moves the box by `dx`, `dy`.
    pub fn translate(self, dx: f64, dy: f64) -> Self {
        Self {
            x0: self.x0 + dx,
            x1: self.x1 + dx,
            y0: self.y0 + dy,
            y1: self.y1 + dy,
        }
    }

    /// Grows the box by `pad` on every side.
    pub fn pad(self, pad: f64) -> Self {
        Self {
            x0: self.x0 - pad,
            x1: self.x1 + pad,
            y0: self.y0 - pad,
            y1: self.y1 + pad,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersection_is_closed_and_symmetric() {
        let a = Extents::from_rect(0.0, 0.0, 10.0, 10.0);
        let b = Extents::from_rect(10.0, 10.0, 5.0, 5.0);
        assert!(a.intersects(b));
        assert!(b.intersects(a));
        assert!(!a.intersects(Extents::from_rect(10.01, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn union_covers_both_inputs() {
        let a = Extents::from_rect(0.0, 0.0, 10.0, 5.0);
        let b = Extents::from_rect(-4.0, 8.0, 2.0, 2.0);
        let u = a.union(b);
        assert_eq!(u.x0, -4.0);
        assert_eq!(u.y1, 10.0);
        assert_eq!(u.x1, 10.0);
        assert!(u.intersects(a) && u.intersects(b));
    }

    #[test]
    fn disjoint_intersection_collapses_to_point() {
        let a = Extents::from_rect(0.0, 0.0, 1.0, 1.0);
        let b = Extents::from_rect(10.0, 10.0, 1.0, 1.0);
        let i = a.intersection(b);
        assert_eq!(i.width(), 0.0);
        assert_eq!(i.height(), 0.0);
    }
}
