//! A small 2D affine transform used to map typst frame points into egui points.
//!
//! The transform uses the typst [`Transform`](typst_library::layout::Transform)
//! layout, i.e. it describes a scale-skew-translate mapping:
//!
//! ```text
//! x' = sx * x + kx * y + tx
//! y' = ky * x + sy * y + ty
//! ```

use typst_library::layout::Transform;

/// A 2D affine transform (scale, skew, translate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    /// Horizontal scale.
    pub sx: f64,
    /// Vertical shear.
    pub ky: f64,
    /// Horizontal shear.
    pub kx: f64,
    /// Vertical scale.
    pub sy: f64,
    /// Horizontal translation.
    pub tx: f64,
    /// Vertical translation.
    pub ty: f64,
}

impl Affine2 {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        sx: 1.0,
        ky: 0.0,
        kx: 0.0,
        sy: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// A pure translation.
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self {
            sx: 1.0,
            ky: 0.0,
            kx: 0.0,
            sy: 1.0,
            tx,
            ty,
        }
    }

    /// A uniform scale followed by a translation.
    pub fn uniform_scale_translate(scale: f64, tx: f64, ty: f64) -> Self {
        Self {
            sx: scale,
            ky: 0.0,
            kx: 0.0,
            sy: scale,
            tx,
            ty,
        }
    }

    /// Maps a point through this transform.
    pub fn map(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.sx * x + self.kx * y + self.tx,
            self.ky * x + self.sy * y + self.ty,
        )
    }

    /// Returns the transform that applies `self` first and `next` afterwards.
    ///
    /// This is function composition: the returned mapping is `next(self(p))`.
    pub fn then(self, next: Self) -> Self {
        Self {
            sx: next.sx * self.sx + next.kx * self.ky,
            kx: next.sx * self.kx + next.kx * self.sy,
            tx: next.sx * self.tx + next.kx * self.ty + next.tx,
            ky: next.ky * self.sx + next.sy * self.ky,
            sy: next.ky * self.kx + next.sy * self.sy,
            ty: next.ky * self.tx + next.sy * self.ty + next.ty,
        }
    }

    /// Returns the inverse mapping, or `None` when the transform is singular.
    pub fn invert(self) -> Option<Self> {
        let det = self.sx * self.sy - self.kx * self.ky;
        if det.abs() < f64::EPSILON {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Self {
            sx: self.sy * inv_det,
            ky: -self.ky * inv_det,
            kx: -self.kx * inv_det,
            sy: self.sx * inv_det,
            tx: (self.kx * self.ty - self.sy * self.tx) * inv_det,
            ty: (self.ky * self.tx - self.sx * self.ty) * inv_det,
        })
    }

    /// Converts a typst [`Transform`] into an [`Affine2`].
    pub fn from_typst(t: &Transform) -> Self {
        Self {
            sx: t.sx.get(),
            ky: t.ky.get(),
            kx: t.kx.get(),
            sy: t.sy.get(),
            tx: t.tx.to_pt(),
            ty: t.ty.to_pt(),
        }
    }

    /// Converts a usvg (tiny-skia) transform into an [`Affine2`].
    ///
    /// The two share the same `(sx, ky, kx, sy, tx, ty)` field layout and the
    /// same point mapping, so this is a plain widening conversion.
    pub(crate) fn from_usvg(t: usvg::Transform) -> Self {
        Self {
            sx: f64::from(t.sx),
            ky: f64::from(t.ky),
            kx: f64::from(t.kx),
            sy: f64::from(t.sy),
            tx: f64::from(t.tx),
            ty: f64::from(t.ty),
        }
    }
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst_library::layout::{Abs, Ratio};

    fn assert_close(actual: (f64, f64), expected: (f64, f64)) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-9 && (actual.1 - expected.1).abs() < 1e-9,
            "got {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn map_applies_scale_skew_translate() {
        let m = Affine2 {
            sx: 2.0,
            ky: 0.5,
            kx: 0.25,
            sy: 3.0,
            tx: 10.0,
            ty: -4.0,
        };
        // x' = 2*1 + 0.25*2 + 10 = 12.5 ; y' = 0.5*1 + 3*2 - 4 = 2.5
        assert_close(m.map(1.0, 2.0), (12.5, 2.5));
    }

    #[test]
    fn then_applies_self_first_then_next() {
        let a = Affine2::uniform_scale_translate(2.0, 1.0, 1.0);
        let b = Affine2::translate(5.0, -3.0);
        let composed = a.then(b);
        assert_close(
            composed.map(3.0, 4.0),
            b.map(a.map(3.0, 4.0).0, a.map(3.0, 4.0).1),
        );
    }

    #[test]
    fn identity_is_neutral_for_then() {
        let m = Affine2 {
            sx: 2.0,
            ky: 0.1,
            kx: -0.2,
            sy: 0.5,
            tx: 7.0,
            ty: 9.0,
        };
        assert_eq!(m.then(Affine2::IDENTITY), m);
        assert_eq!(Affine2::IDENTITY.then(m), m);
    }

    #[test]
    fn translate_is_const_and_maps_correctly() {
        assert_close(Affine2::translate(3.0, -2.0).map(1.0, 1.0), (4.0, -1.0));
    }

    #[test]
    fn from_typst_maps_fields() {
        let t = Transform {
            sx: Ratio::new(2.0),
            ky: Ratio::new(0.5),
            kx: Ratio::new(-0.5),
            sy: Ratio::new(3.0),
            tx: Abs::pt(4.0),
            ty: Abs::pt(-6.0),
        };
        assert_eq!(
            Affine2::from_typst(&t),
            Affine2 {
                sx: 2.0,
                ky: 0.5,
                kx: -0.5,
                sy: 3.0,
                tx: 4.0,
                ty: -6.0,
            }
        );
    }

    #[test]
    fn from_typst_identity_is_identity() {
        assert_eq!(
            Affine2::from_typst(&Transform::identity()),
            Affine2::IDENTITY
        );
    }
}
