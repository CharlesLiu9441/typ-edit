//! Color conversion and paint-space helpers shared by the canvas leaves.

use egui::Color32;
use typst_library::layout::Rect;
use typst_library::visualize::Color;

use crate::affine::Affine2;
use crate::bounds::Extents;

/// Painting state for one mesh: how path space and its hard container map onto
/// egui screen points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintCtx {
    /// path space -> screen points
    pub to_screen: Affine2,
    /// hard-container space -> screen points
    pub container_to_screen: Affine2,
    /// container size in points
    pub container_size: (f64, f64),
    /// pixels per logical point of the host screen
    pub pixels_per_point: f32,
    /// paint applied to text (changes the default `RelativeTo` to Parent)
    pub on_text: bool,
}

/// Converts a typst color into an sRGB egui color.
pub(crate) fn color32(color: &Color) -> Color32 {
    let (r, g, b, a) = color.to_process().to_rgb().into_components();
    Color32::from_rgba_unmultiplied(channel(r), channel(g), channel(b), channel(a))
}

/// Converts a typst rectangle into path-space [`Extents`].
pub(crate) fn rect_bbox(rect: Rect) -> Extents {
    Extents {
        x0: rect.min.x.to_pt(),
        x1: rect.max.x.to_pt(),
        y0: rect.min.y.to_pt(),
        y1: rect.max.y.to_pt(),
    }
}

/// UV coordinates for a quad's corners, in `[top-left, top-right, bottom-right,
/// bottom-left]` order.
pub(crate) const QUAD_UVS: [(f32, f32); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

/// Triangle indices for a quad's corners.
pub(crate) const QUAD_INDICES: [u32; 6] = [0, 1, 2, 0, 2, 3];

fn channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_paint_maps_to_opaque_black() {
        assert_eq!(color32(&Color::BLACK), Color32::BLACK);
    }

    #[test]
    fn channels_round_and_clamp() {
        assert_eq!(channel(-1.0), 0);
        assert_eq!(channel(0.0), 0);
        assert_eq!(channel(1.0), 255);
        assert_eq!(channel(2.0), 255);
    }
}
