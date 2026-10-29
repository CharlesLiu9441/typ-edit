//! Path primitives shared by the fill and stroke tessellators.
//!
//! A [`Path2`] stores an ordered list of flat segments in path space (typst
//! points, x-right / y-down). Quadratics are converted to cubics by callers, so
//! the segment set is deliberately small.

use typst_library::layout::{Point, Size};
use typst_library::visualize::{Curve, CurveItem};

use crate::affine::Affine2;

/// One path segment.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Seg {
    /// Move the pen to `(x, y)` without drawing.
    Move(f64, f64),
    /// Draw a line to `(x, y)`.
    Line(f64, f64),
    /// control1.x, control1.y, control2.x, control2.y, end.x, end.y (cubic only; quadratics are converted to cubics by callers).
    Cubic(f64, f64, f64, f64, f64, f64),
    /// Close the current subpath.
    Close,
}

/// Converts a quadratic Bézier into the equivalent cubic using the 2/3
/// control-point rule, matching how typst curves are represented internally.
pub(crate) fn quad_to_cubic(start: (f64, f64), control: (f64, f64), end: (f64, f64)) -> Seg {
    let first = (
        start.0 + (2.0 / 3.0) * (control.0 - start.0),
        start.1 + (2.0 / 3.0) * (control.1 - start.1),
    );
    let second = (
        end.0 + (2.0 / 3.0) * (control.0 - end.0),
        end.1 + (2.0 / 3.0) * (control.1 - end.1),
    );
    Seg::Cubic(first.0, first.1, second.0, second.1, end.0, end.1)
}

/// A path in path space (typst points, x-right / y-down).
#[derive(Clone, Debug, Default)]
pub(crate) struct Path2(pub Vec<Seg>);

impl Path2 {
    /// Converts a typst [`Curve`] (points are `Point`, converted with `to_pt`).
    pub fn from_curve(curve: &Curve) -> Self {
        let mut segs = Vec::with_capacity(curve.0.len());
        for item in &curve.0 {
            match item {
                CurveItem::Move(p) => segs.push(Seg::Move(p.x.to_pt(), p.y.to_pt())),
                CurveItem::Line(p) => segs.push(Seg::Line(p.x.to_pt(), p.y.to_pt())),
                CurveItem::Cubic(a, b, c) => segs.push(Seg::Cubic(
                    a.x.to_pt(),
                    a.y.to_pt(),
                    b.x.to_pt(),
                    b.y.to_pt(),
                    c.x.to_pt(),
                    c.y.to_pt(),
                )),
                CurveItem::Close => segs.push(Seg::Close),
            }
        }
        Self(segs)
    }

    /// Converts a usvg `tiny_skia_path::Path` into a [`Path2`].
    ///
    /// SVG's close-then-continue semantics do not line up with a pen that
    /// always returns to the subpath start, so an explicit `Move` back to the
    /// most recent `MoveTo` is emitted whenever a drawing segment follows a
    /// `Close` (the same fixup vello_svg applies). Quadratics are converted
    /// with [`quad_to_cubic`].
    pub fn from_tiny_skia(path: &usvg::tiny_skia_path::Path) -> Self {
        use usvg::tiny_skia_path::PathSegment;
        let mut segs = Vec::new();
        let mut just_closed = false;
        let mut start = (0.0_f64, 0.0_f64);
        let mut current = (0.0_f64, 0.0_f64);
        for elt in path.segments() {
            match elt {
                PathSegment::MoveTo(p) => {
                    if std::mem::take(&mut just_closed) {
                        segs.push(Seg::Move(start.0, start.1));
                    }
                    start = (p.x as f64, p.y as f64);
                    current = start;
                    segs.push(Seg::Move(start.0, start.1));
                }
                PathSegment::LineTo(p) => {
                    if std::mem::take(&mut just_closed) {
                        segs.push(Seg::Move(start.0, start.1));
                    }
                    current = (p.x as f64, p.y as f64);
                    segs.push(Seg::Line(current.0, current.1));
                }
                PathSegment::QuadTo(c, p) => {
                    if std::mem::take(&mut just_closed) {
                        segs.push(Seg::Move(start.0, start.1));
                    }
                    let control = (c.x as f64, c.y as f64);
                    let end = (p.x as f64, p.y as f64);
                    segs.push(quad_to_cubic(current, control, end));
                    current = end;
                }
                PathSegment::CubicTo(c1, c2, p) => {
                    if std::mem::take(&mut just_closed) {
                        segs.push(Seg::Move(start.0, start.1));
                    }
                    current = (p.x as f64, p.y as f64);
                    segs.push(Seg::Cubic(
                        c1.x as f64,
                        c1.y as f64,
                        c2.x as f64,
                        c2.y as f64,
                        current.0,
                        current.1,
                    ));
                }
                PathSegment::Close => {
                    just_closed = true;
                    // A subsequent segment starts a new subpath at the closed
                    // subpath's initial point, so the pen returns there.
                    current = start;
                    segs.push(Seg::Close);
                }
            }
        }
        Self(segs)
    }

    /// A rectangle from `(0, 0)` to `size`. Negative width/height are
    /// normalized so the box still spans from the origin.
    pub fn rect(size: Size) -> Self {
        let (w, h) = (size.x.to_pt(), size.y.to_pt());
        let (x0, x1) = (w.min(0.0), w.max(0.0));
        let (y0, y1) = (h.min(0.0), h.max(0.0));
        Self(vec![
            Seg::Move(x0, y0),
            Seg::Line(x1, y0),
            Seg::Line(x1, y1),
            Seg::Line(x0, y1),
            Seg::Close,
        ])
    }

    /// A single open line segment from `(0, 0)` to `end`.
    pub fn line(end: Point) -> Self {
        Self(vec![
            Seg::Move(0.0, 0.0),
            Seg::Line(end.x.to_pt(), end.y.to_pt()),
        ])
    }

    /// Applies `m` to every point in the path.
    pub fn map(&self, m: &Affine2) -> Self {
        let mut out = Vec::with_capacity(self.0.len());
        for seg in &self.0 {
            out.push(match *seg {
                Seg::Move(x, y) => {
                    let (a, b) = m.map(x, y);
                    Seg::Move(a, b)
                }
                Seg::Line(x, y) => {
                    let (a, b) = m.map(x, y);
                    Seg::Line(a, b)
                }
                Seg::Cubic(x1, y1, x2, y2, x3, y3) => {
                    let (a, b) = m.map(x1, y1);
                    let (c, d) = m.map(x2, y2);
                    let (e, f) = m.map(x3, y3);
                    Seg::Cubic(a, b, c, d, e, f)
                }
                Seg::Close => Seg::Close,
            });
        }
        Self(out)
    }

    /// Whether the path has no segments.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Control-point bounding box `(x0, y0, x1, y1)`, or `None` when there is no
    /// point at all.
    #[cfg(test)]
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut bounds = None;
        for seg in &self.0 {
            match *seg {
                Seg::Move(x, y) | Seg::Line(x, y) => add_point(&mut bounds, x, y),
                Seg::Cubic(x1, y1, x2, y2, x3, y3) => {
                    add_point(&mut bounds, x1, y1);
                    add_point(&mut bounds, x2, y2);
                    add_point(&mut bounds, x3, y3);
                }
                Seg::Close => {}
            }
        }
        bounds
    }

    /// Builds a lyon path. When `m` is `Some`, every point is transformed by it
    /// (used to build a fill path directly in screen space).
    pub(crate) fn to_lyon(&self, m: Option<&Affine2>) -> lyon::path::Path {
        let map = |x: f64, y: f64| -> (f32, f32) {
            match m {
                Some(m) => {
                    let (a, b) = m.map(x, y);
                    (a as f32, b as f32)
                }
                None => (x as f32, y as f32),
            }
        };
        let mut builder = lyon::path::Path::builder();
        let mut open = false;
        for seg in &self.0 {
            match *seg {
                Seg::Move(x, y) => {
                    if open {
                        builder.end(false);
                    }
                    let (a, b) = map(x, y);
                    builder.begin(lyon::math::point(a, b));
                    open = true;
                }
                Seg::Line(x, y) => {
                    if open {
                        let (a, b) = map(x, y);
                        builder.line_to(lyon::math::point(a, b));
                    }
                }
                Seg::Cubic(x1, y1, x2, y2, x3, y3) => {
                    if open {
                        let (a, b) = map(x1, y1);
                        let (c, d) = map(x2, y2);
                        let (e, f) = map(x3, y3);
                        builder.cubic_bezier_to(
                            lyon::math::point(a, b),
                            lyon::math::point(c, d),
                            lyon::math::point(e, f),
                        );
                    }
                }
                Seg::Close => {
                    if open {
                        builder.close();
                        open = false;
                    }
                }
            }
        }
        if open {
            builder.end(false);
        }
        builder.build()
    }
}

/// Extends a running bounding box with one point.
#[cfg(test)]
fn add_point(bounds: &mut Option<(f64, f64, f64, f64)>, x: f64, y: f64) {
    *bounds = Some(match *bounds {
        None => (x, y, x, y),
        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst_library::layout::Abs;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn move_xy(seg: Seg) -> Option<(f64, f64)> {
        match seg {
            Seg::Move(x, y) => Some((x, y)),
            _ => None,
        }
    }

    fn line_xy(seg: Seg) -> Option<(f64, f64)> {
        match seg {
            Seg::Line(x, y) => Some((x, y)),
            _ => None,
        }
    }

    #[test]
    fn rect_normalizes_negative_sizes() {
        let path = Path2::rect(Size::new(Abs::pt(-4.0), Abs::pt(-6.0)));
        let b = path.bounds().unwrap();
        assert!(
            close(b.0, -4.0) && close(b.1, -6.0) && close(b.2, 0.0) && close(b.3, 0.0),
            "{b:?}"
        );
    }

    #[test]
    fn rect_spans_origin_to_positive_size() {
        let path = Path2::rect(Size::new(Abs::pt(10.0), Abs::pt(20.0)));
        assert_eq!(path.bounds(), Some((0.0, 0.0, 10.0, 20.0)));
        assert!(matches!(path.0[0], Seg::Move(x, y) if close(x, 0.0) && close(y, 0.0)));
        assert!(matches!(path.0[4], Seg::Close));
    }

    #[test]
    fn line_has_move_then_line() {
        let path = Path2::line(Point::new(Abs::pt(3.0), Abs::pt(4.0)));
        assert_eq!(path.0.len(), 2);
        assert_eq!(move_xy(path.0[0]), Some((0.0, 0.0)));
        assert_eq!(line_xy(path.0[1]), Some((3.0, 4.0)));
    }

    #[test]
    fn from_curve_preserves_segments() {
        let mut curve = Curve::new();
        curve.move_(Point::new(Abs::pt(1.0), Abs::pt(2.0)));
        curve.line(Point::new(Abs::pt(3.0), Abs::pt(4.0)));
        curve.cubic(
            Point::new(Abs::pt(5.0), Abs::pt(6.0)),
            Point::new(Abs::pt(7.0), Abs::pt(8.0)),
            Point::new(Abs::pt(9.0), Abs::pt(10.0)),
        );
        curve.close();

        let path = Path2::from_curve(&curve);
        assert_eq!(path.0.len(), 4);
        assert_eq!(move_xy(path.0[0]), Some((1.0, 2.0)));
        assert_eq!(line_xy(path.0[1]), Some((3.0, 4.0)));
        assert!(matches!(
            path.0[2],
            Seg::Cubic(5.0, 6.0, 7.0, 8.0, 9.0, 10.0)
        ));
        assert!(matches!(path.0[3], Seg::Close));
    }

    #[test]
    fn map_transforms_all_control_points() {
        let path = Path2(vec![
            Seg::Move(1.0, 1.0),
            Seg::Cubic(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
        ]);
        let m = Affine2::uniform_scale_translate(2.0, 10.0, 100.0);
        let mapped = path.map(&m);
        assert!(matches!(mapped.0[0], Seg::Move(12.0, 102.0)));
        assert!(matches!(
            mapped.0[1],
            Seg::Cubic(12.0, 104.0, 16.0, 108.0, 20.0, 112.0)
        ));
    }

    #[test]
    fn empty_path_has_no_bounds() {
        assert!(Path2::default().is_empty());
        assert_eq!(Path2::default().bounds(), None);
    }
}
