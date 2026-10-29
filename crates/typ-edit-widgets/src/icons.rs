//! Vector icons for tree rows and the window controls, converted from Material
//! Symbols SVG paths (960x960 viewBox).
//!
//! tui-whitelist: pure painting. Every icon is a caller-allocated rect plus
//! `painter()` path geometry; there is no child-element layout here.

use egui::{Color32, Pos2, Rect};

/// Side of the (square) icon cell in points.
pub const ICON_PX: f32 = 14.0;

/// Extent of the source viewBox; every normalized point lies in `0..=VIEWBOX` on both axes.
const VIEWBOX: f32 = 960.0;

/// Thickness of the outlined folder ring in viewBox units (the source path insets the inner
/// contour by 60 units).
const STROKE_UNITS: f32 = 60.0;

/// Convex polygons filling the closed-folder silhouette, in viewBox coordinates (y grows
/// downwards, clockwise winding). epaint only fills convex paths, so the tab and the body are
/// painted as separate pieces.
static CLOSED_FILL: &[&[[f32; 2]]] = &[
    // Tab: the slanted flap left of the fold.
    &[
        [80.0, 220.0],
        [140.0, 160.0],
        [421.0, 160.0],
        [481.0, 220.0],
    ],
    // Body: the rounded rectangle below the tab, its corners approximated by straight cuts.
    &[
        [80.0, 220.0],
        [820.0, 220.0],
        [880.0, 280.0],
        [880.0, 740.0],
        [820.0, 800.0],
        [140.0, 800.0],
        [80.0, 740.0],
    ],
];

/// Convex polygons filling the open-folder silhouette. The pieces tile the source path exactly:
/// tab, upper body, left strip, bottom-left corner and the slanted front flap.
static OPEN_FILL: &[&[[f32; 2]]] = &[
    &[
        [80.0, 220.0],
        [140.0, 160.0],
        [421.0, 160.0],
        [481.0, 220.0],
    ],
    &[[80.0, 220.0], [820.0, 220.0], [880.0, 280.0], [80.0, 280.0]],
    &[[80.0, 280.0], [140.0, 280.0], [140.0, 740.0], [80.0, 740.0]],
    &[[80.0, 740.0], [140.0, 740.0], [140.0, 800.0]],
    &[
        [140.0, 800.0],
        [140.0, 740.0],
        [242.0, 340.0],
        [940.0, 340.0],
        [833.0, 754.0],
        [770.0, 800.0],
    ],
];

/// Outer contour of the closed folder, in painting order (viewBox coordinates).
static CLOSED_OUTLINE: &[[f32; 2]] = &[
    [140.0, 800.0],
    [80.0, 740.0],
    [80.0, 220.0],
    [140.0, 160.0],
    [421.0, 160.0],
    [481.0, 220.0],
    [820.0, 220.0],
    [880.0, 280.0],
    [880.0, 740.0],
    [820.0, 800.0],
];

/// Outer contour of the open folder (first subpath of the source icon).
static OPEN_OUTLINE: &[[f32; 2]] = &[
    [140.0, 800.0],
    [80.0, 740.0],
    [80.0, 220.0],
    [140.0, 160.0],
    [421.0, 160.0],
    [481.0, 220.0],
    [820.0, 220.0],
    [880.0, 280.0],
    [455.0, 280.0],
    [395.0, 220.0],
    [140.0, 220.0],
    [140.0, 740.0],
    [242.0, 340.0],
    [940.0, 340.0],
    [833.0, 754.0],
    [770.0, 800.0],
];

/// Paints a folder icon into `rect`.
///
/// `open` selects the opened-folder variant, `filled` the solid variant; `color` tints the icon.
pub fn folder(ui: &egui::Ui, rect: Rect, open: bool, filled: bool, color: Color32) {
    if filled {
        paint_fill(ui, rect, if open { OPEN_FILL } else { CLOSED_FILL }, color);
    } else {
        paint_outline(
            ui,
            rect,
            if open { OPEN_OUTLINE } else { CLOSED_OUTLINE },
            color,
        );
    }
}

/// Fills convex `polygons` (viewBox coordinates, clockwise) into `rect`.
fn paint_fill(ui: &egui::Ui, rect: Rect, polygons: &[&[[f32; 2]]], color: Color32) {
    for polygon in polygons {
        let points = polygon.iter().map(|p| map_to_rect(rect, *p)).collect();
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// Strokes the `outline` polyline inside `rect`, inset by half the stroke width so the stroke
/// stays within the cell. The rounded corners of the source path are approximated by straight
/// segments (the radius is well below a pixel at `ICON_PX`).
fn paint_outline(ui: &egui::Ui, rect: Rect, outline: &[[f32; 2]], color: Color32) {
    let width = (rect.width() * STROKE_UNITS / VIEWBOX).max(1.0);
    let draw_rect = rect.shrink(width * 0.5);
    let points = outline.iter().map(|p| map_to_rect(draw_rect, *p)).collect();
    ui.painter().add(egui::Shape::closed_line(
        points,
        egui::Stroke::new(width, color),
    ));
}

/// Maps a viewBox point into `rect`; the cell is square, so one scale serves both axes.
fn map_to_rect(rect: Rect, point: [f32; 2]) -> Pos2 {
    let scale = rect.width() / VIEWBOX;
    egui::pos2(rect.min.x + point[0] * scale, rect.min.y + point[1] * scale)
}

/// Side of a window-control icon cell in points.
pub const WINDOW_ICON_PX: f32 = 16.0;

/// Thickness of the ring glyphs (maximize / restore) in viewBox units; the source
/// paths inset their inner contour by 60 units.
const WINDOW_STROKE_UNITS: f32 = 60.0;

/// Which window-control glyph to paint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowGlyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Window glyphs use centered coordinates: (0, 0) is the middle of the glyph box
/// and 960 units span one [`WINDOW_ICON_PX`] cell, so every glyph is optically
/// centered regardless of the padding in its source viewBox.
///
/// The close glyph as two overlapping convex bars; the source path is a single
/// non-convex 12-gon whose union equals these two bars.
static WINDOW_CLOSE_FILL: &[&[[f32; 2]]] = &[
    // "/" bar, between the bottom-left and top-right end cuts.
    &[
        [-231.0, 273.0],
        [-273.0, 231.0],
        [231.0, -273.0],
        [273.0, -231.0],
    ],
    // "\" bar, between the top-left and bottom-right end cuts.
    &[
        [-273.0, -231.0],
        [-231.0, -273.0],
        [273.0, 231.0],
        [231.0, 273.0],
    ],
];

/// The minimize glyph: a single bar.
static WINDOW_MINIMIZE_FILL: &[&[[f32; 2]]] = &[&[
    [-340.0, -30.0],
    [340.0, -30.0],
    [340.0, 30.0],
    [-340.0, 30.0],
]];

/// The maximize ring's centerline; the stroke straddles it, giving the source
/// path's radius-60 outer corners and sharp inner corners. The corners are
/// straight cuts (their radius is well below a pixel at [`WINDOW_ICON_PX`]).
static WINDOW_MAXIMIZE_PATH: &[[f32; 2]] = &[
    [-310.0, -280.0],
    [-280.0, -310.0],
    [280.0, -310.0],
    [310.0, -280.0],
    [310.0, 280.0],
    [280.0, 310.0],
    [-280.0, 310.0],
    [-310.0, 280.0],
];

/// The restore glyph's back layer: the centerline of an L-shaped band, open at
/// its top and right ends.
static WINDOW_RESTORE_BACK: &[[f32; 2]] = &[
    [-310.0, -204.0],
    [-310.0, 280.0],
    [-280.0, 310.0],
    [204.0, 310.0],
];

/// The restore glyph's front layer: a ring centerline like [`WINDOW_MAXIMIZE_PATH`],
/// shifted to the top-right.
static WINDOW_RESTORE_FRONT: &[[f32; 2]] = &[
    [-190.0, -280.0],
    [-160.0, -310.0],
    [280.0, -310.0],
    [310.0, -280.0],
    [310.0, 160.0],
    [280.0, 190.0],
    [-160.0, 190.0],
    [-190.0, 160.0],
];

/// Paints a window-control glyph (Material Symbols geometry) centered in `rect`.
pub fn window_glyph(ui: &egui::Ui, rect: Rect, glyph: WindowGlyph, color: Color32) {
    match glyph {
        WindowGlyph::Minimize => paint_window_fill(ui, rect, WINDOW_MINIMIZE_FILL, color),
        WindowGlyph::Close => paint_window_fill(ui, rect, WINDOW_CLOSE_FILL, color),
        WindowGlyph::Maximize => paint_window_line(ui, rect, WINDOW_MAXIMIZE_PATH, true, color),
        WindowGlyph::Restore => {
            paint_window_line(ui, rect, WINDOW_RESTORE_BACK, false, color);
            paint_window_line(ui, rect, WINDOW_RESTORE_FRONT, true, color);
        }
    }
}

/// Fills convex window-glyph `polygons` (centered coordinates) into `rect`.
fn paint_window_fill(ui: &egui::Ui, rect: Rect, polygons: &[&[[f32; 2]]], color: Color32) {
    for polygon in polygons {
        let points = polygon
            .iter()
            .map(|p| map_window_to_rect(rect, *p))
            .collect();
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// Strokes one window-glyph centerline into `rect`; the source paths are ring
/// centerlines, so the stroke straddles them with no inset.
fn paint_window_line(ui: &egui::Ui, rect: Rect, path: &[[f32; 2]], closed: bool, color: Color32) {
    let width = (rect.width() * WINDOW_STROKE_UNITS / VIEWBOX).max(1.0);
    let points = path.iter().map(|p| map_window_to_rect(rect, *p)).collect();
    let stroke = egui::Stroke::new(width, color);
    ui.painter().add(if closed {
        egui::Shape::closed_line(points, stroke)
    } else {
        egui::Shape::line(points, stroke)
    });
}

/// Maps a centered window-glyph point into `rect`.
fn map_window_to_rect(rect: Rect, point: [f32; 2]) -> Pos2 {
    let scale = rect.width() / VIEWBOX;
    egui::pos2(
        rect.center().x + point[0] * scale,
        rect.center().y + point[1] * scale,
    )
}

/// Centerline radius of the settings gear's center ring, in centered icon units
/// (the source ring spans radius 70..130, so the 60-unit stroke is centered here).
const SETTINGS_RING_UNITS: f32 = 100.0;

/// Outer contour of the Material Symbols `settings` gear in centered cell
/// coordinates, screen-clockwise (the inside stroke and its winding must agree),
/// flattened from the source path's quadratics.
static SETTINGS_GEAR: &[[f32; 2]] = &[
    [-92.0, 400.0],
    [-112.0, 274.0],
    [-116.8, 272.2],
    [-121.6, 270.2],
    [-126.5, 268.0],
    [-131.5, 265.8],
    [-136.5, 263.3],
    [-141.6, 260.7],
    [-146.8, 257.9],
    [-152.0, 255.0],
    [-157.2, 252.0],
    [-162.2, 248.9],
    [-167.0, 245.9],
    [-171.8, 242.8],
    [-176.3, 239.6],
    [-180.7, 236.4],
    [-184.9, 233.2],
    [-189.0, 230.0],
    [-307.0, 284.0],
    [-400.0, 120.0],
    [-292.0, 41.0],
    [-292.5, 38.7],
    [-292.9, 36.3],
    [-293.3, 33.9],
    [-293.6, 31.4],
    [-293.9, 28.8],
    [-294.2, 26.1],
    [-294.4, 23.3],
    [-294.5, 20.5],
    [-294.6, 17.7],
    [-294.7, 14.9],
    [-294.8, 12.2],
    [-294.9, 9.6],
    [-294.9, 7.1],
    [-295.0, 4.7],
    [-295.0, 2.3],
    [-295.0, 0.0],
    [-295.0, -2.3],
    [-295.0, -4.7],
    [-294.9, -7.1],
    [-294.9, -9.6],
    [-294.8, -12.2],
    [-294.7, -14.9],
    [-294.6, -17.7],
    [-294.5, -20.5],
    [-294.4, -23.3],
    [-294.2, -26.1],
    [-293.9, -28.8],
    [-293.6, -31.4],
    [-293.3, -33.9],
    [-292.9, -36.3],
    [-292.5, -38.7],
    [-292.0, -41.0],
    [-400.0, -120.0],
    [-307.0, -284.0],
    [-189.0, -230.0],
    [-184.9, -233.2],
    [-180.7, -236.4],
    [-176.3, -239.6],
    [-171.8, -242.8],
    [-167.0, -245.9],
    [-162.2, -248.9],
    [-157.2, -252.0],
    [-152.0, -255.0],
    [-146.8, -257.9],
    [-141.6, -260.6],
    [-136.5, -263.2],
    [-131.5, -265.5],
    [-126.5, -267.7],
    [-121.6, -269.6],
    [-116.8, -271.4],
    [-112.0, -273.0],
    [-92.0, -400.0],
    [92.0, -400.0],
    [112.0, -274.0],
    [116.8, -272.2],
    [121.7, -270.2],
    [126.6, -268.1],
    [131.6, -265.9],
    [136.7, -263.5],
    [141.9, -261.0],
    [147.2, -258.3],
    [152.5, -255.5],
    [157.8, -252.6],
    [162.8, -249.6],
    [167.7, -246.5],
    [172.4, -243.4],
    [176.8, -240.1],
    [181.1, -236.8],
    [185.1, -233.5],
    [189.0, -230.0],
    [307.0, -284.0],
    [400.0, -120.0],
    [292.0, -43.0],
    [292.5, -40.5],
    [292.9, -37.9],
    [293.3, -35.3],
    [293.6, -32.6],
    [293.9, -29.9],
    [294.2, -27.2],
    [294.4, -24.4],
    [294.5, -21.5],
    [294.6, -18.6],
    [294.7, -15.8],
    [294.8, -13.1],
    [294.9, -10.4],
    [294.9, -7.7],
    [295.0, -5.1],
    [295.0, -2.5],
    [295.0, 0.0],
    [295.0, 2.5],
    [295.0, 5.1],
    [294.9, 7.6],
    [294.9, 10.2],
    [294.8, 12.9],
    [294.7, 15.6],
    [294.6, 18.3],
    [294.5, 21.0],
    [294.4, 23.7],
    [294.2, 26.4],
    [293.9, 29.1],
    [293.6, 31.8],
    [293.3, 34.4],
    [292.9, 36.9],
    [292.5, 39.5],
    [292.0, 42.0],
    [400.0, 120.0],
    [307.0, 284.0],
    [189.0, 230.0],
    [184.9, 233.2],
    [180.7, 236.5],
    [176.4, 239.7],
    [171.9, 242.9],
    [167.2, 246.1],
    [162.5, 249.2],
    [157.6, 252.4],
    [152.5, 255.5],
    [147.4, 258.5],
    [142.3, 261.3],
    [137.2, 264.0],
    [132.1, 266.4],
    [127.1, 268.6],
    [122.0, 270.6],
    [117.0, 272.4],
    [112.0, 274.0],
    [92.0, 400.0],
    [-92.0, 400.0],
];

/// Paints the Material Symbols `settings` gear (outlined) centered in `rect`.
pub fn settings(ui: &egui::Ui, rect: Rect, color: Color32) {
    let width = (rect.width() * WINDOW_STROKE_UNITS / VIEWBOX).max(1.0);
    let contour: Vec<Pos2> = SETTINGS_GEAR
        .iter()
        .map(|p| map_window_to_rect(rect, *p))
        .collect();
    ui.painter().add(egui::Shape::closed_line(
        contour,
        egui::epaint::PathStroke::new(width, color).inside(),
    ));

    const RING_STEPS: usize = 48;
    let radius = SETTINGS_RING_UNITS * rect.width() / VIEWBOX;
    let ring: Vec<Pos2> = (0..RING_STEPS)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / RING_STEPS as f32;
            rect.center() + egui::vec2(angle.cos() * radius, angle.sin() * radius)
        })
        .collect();
    ui.painter().add(egui::Shape::closed_line(
        ring,
        egui::Stroke::new(width, color),
    ));
}

/// Paints the tree row's expand/collapse chevron centered in `rect`: a right-pointing triangle
/// while collapsed, a down-pointing one while expanded.
pub fn chevron(ui: &egui::Ui, rect: Rect, expanded: bool, color: Color32) {
    let center = rect.center();
    let s = (rect.width().min(rect.height()) * 0.28).max(1.5);
    let points = if expanded {
        vec![
            egui::pos2(center.x - s, center.y - s * 0.55),
            egui::pos2(center.x + s, center.y - s * 0.55),
            egui::pos2(center.x, center.y + s * 0.7),
        ]
    } else {
        vec![
            egui::pos2(center.x - s * 0.55, center.y - s),
            egui::pos2(center.x - s * 0.55, center.y + s),
            egui::pos2(center.x + s * 0.7, center.y),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Left-pointing chevron for the collapse control. Mirrors the horizontal
/// points of [`chevron`] without depending on a font glyph.
pub fn chevron_left(ui: &egui::Ui, rect: Rect, color: Color32) {
    let center = rect.center();
    let s = (rect.width().min(rect.height()) * 0.28).max(1.5);
    let points = vec![
        egui::pos2(center.x + s * 0.55, center.y - s),
        egui::pos2(center.x + s * 0.55, center.y + s),
        egui::pos2(center.x - s * 0.7, center.y),
    ];
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// The two overlapping squares of the dock icon, recentred as a pair.
///
/// Anchoring both rectangles at the same top-left (as the first version did)
/// made the shifted back square push the visible union above the button's
/// centre. Laying the union out around `rect.center()` keeps the icon optically
/// centred while preserving the overlap.
fn dock_rects(rect: Rect) -> (Rect, Rect) {
    let size = rect.width().min(rect.height());
    let square = size * 0.49;
    let offset = size * 0.10;
    let span = square + offset;
    // Horizontally the union runs from the front-left to the back-right, so
    // its centre is `square + offset` wide. Vertically the back square starts
    // `offset` above the front, so the union's centre sits at
    // `(square - offset) / 2` from the front's top edge.
    let base = egui::pos2(
        rect.center().x - span * 0.5,
        rect.center().y - (square - offset) * 0.5,
    );
    let front = egui::Rect::from_min_size(base, egui::Vec2::splat(square));
    let back = egui::Rect::from_min_size(
        base + egui::vec2(offset, -offset),
        egui::Vec2::splat(square),
    );
    (front, back)
}

/// Two overlapping outlined squares for the pop-out / dock control.
pub fn dock(ui: &egui::Ui, rect: Rect, color: Color32) {
    let size = rect.width().min(rect.height());
    let (front, back) = dock_rects(rect);
    let stroke = egui::Stroke::new((size * 0.06).max(1.0), color);
    let radius = egui::CornerRadius::same(1);
    let painter = ui.painter();
    painter.rect_stroke(back, radius, stroke, egui::StrokeKind::Inside);
    painter.rect_stroke(front, radius, stroke, egui::StrokeKind::Inside);
}

/// Convex polygons filling the Material Symbols `arrow_back` glyph, in viewBox
/// coordinates. The source uses `viewBox="0 -960 960 960"`, so every y is
/// shifted down by 960 to land in the module's `0..=VIEWBOX` space. The filled
/// arrow is non-convex, so it is tiled by four convex pieces: the tip triangle,
/// the two barb quads, and the shaft rectangle.
static ARROW_BACK_FILL: &[&[[f32; 2]]] = &[
    // Tip triangle between the two barbs and the shaft opening.
    &[[160.0, 480.0], [274.0, 450.0], [274.0, 510.0]],
    // Upper barb.
    &[
        [160.0, 480.0],
        [480.0, 160.0],
        [522.0, 202.0],
        [274.0, 450.0],
    ],
    // Lower barb.
    &[
        [160.0, 480.0],
        [274.0, 510.0],
        [522.0, 758.0],
        [480.0, 800.0],
    ],
    // Shaft.
    &[
        [274.0, 450.0],
        [800.0, 450.0],
        [800.0, 510.0],
        [274.0, 510.0],
    ],
];

/// Paints the Material Symbols `arrow_back` glyph (a left-pointing arrow) into
/// `rect`. Used for both the title-strip home button and the landing page's
/// back-to-project button.
pub fn arrow_back(ui: &egui::Ui, rect: Rect, color: Color32) {
    paint_fill(ui, rect, ARROW_BACK_FILL, color);
}

/// Convex polygons filling the Material Symbols `bookmark` ribbon, in viewBox
/// coordinates (y grows downwards, clockwise winding). The source glyph has a
/// bottom notch that makes the silhouette concave, so it is tiled by a top bar
/// and two body halves split along the notch's vertical centerline.
static BOOKMARK_FILL: &[&[[f32; 2]]] = &[
    // Top bar above the notch.
    &[
        [200.0, 120.0],
        [760.0, 120.0],
        [760.0, 200.0],
        [200.0, 200.0],
    ],
    // Left half of the ribbon, down to the notch's left slope.
    &[
        [200.0, 200.0],
        [480.0, 200.0],
        [480.0, 720.0],
        [200.0, 840.0],
    ],
    // Right half of the ribbon, down to the notch's right slope.
    &[
        [480.0, 200.0],
        [760.0, 200.0],
        [760.0, 840.0],
        [480.0, 720.0],
    ],
];

/// Outline of the bookmark ribbon, in painting order (viewBox coordinates).
static BOOKMARK_OUTLINE: &[[f32; 2]] = &[
    [200.0, 120.0],
    [760.0, 120.0],
    [760.0, 840.0],
    [480.0, 720.0],
    [200.0, 840.0],
];

/// Paints the Material Symbols `bookmark` ribbon into `rect`; `filled` selects
/// the solid variant, `color` tints it. The glyph is centered in the (square)
/// cell, so the caller passes the full icon box.
pub fn bookmark(ui: &egui::Ui, rect: Rect, filled: bool, color: Color32) {
    if filled {
        paint_fill(ui, rect, BOOKMARK_FILL, color);
    } else {
        paint_outline(ui, rect, BOOKMARK_OUTLINE, color);
    }
}

/// Normalized `(0..=1)` vertices of the check mark polyline, mapped into the
/// icon cell left to right. The extremes sit 3.5 units inside a 16 pt cell.
static CHECK_POINTS: &[[f32; 2]] = &[[0.21875, 0.5], [0.40625, 0.71875], [0.78125, 0.25]];

/// Thickness of the check mark polyline, in points.
const CHECK_STROKE: f32 = 2.0;

/// Paints a check mark (a 3-point polyline) into `rect`.
pub fn check(ui: &egui::Ui, rect: Rect, color: Color32) {
    let points = CHECK_POINTS
        .iter()
        .map(|p| map_unit_to_rect(rect, *p))
        .collect();
    ui.painter().add(egui::Shape::line(
        points,
        egui::Stroke::new(CHECK_STROKE, color),
    ));
}

/// Maps a normalized point into `rect`.
fn map_unit_to_rect(rect: Rect, point: [f32; 2]) -> Pos2 {
    egui::pos2(
        rect.min.x + point[0] * rect.width(),
        rect.min.y + point[1] * rect.height(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross product of the two edges meeting at each vertex; positive for clockwise winding.
    fn is_convex(polygon: &[[f32; 2]]) -> bool {
        (0..polygon.len()).all(|i| {
            let a = polygon[i];
            let b = polygon[(i + 1) % polygon.len()];
            let c = polygon[(i + 2) % polygon.len()];
            (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]) >= 0.0
        })
    }

    #[test]
    fn icon_cell_is_positive() {
        const { assert!(ICON_PX > 0.0) };
    }

    #[test]
    fn fill_and_outline_sets_are_non_empty() {
        assert!(CLOSED_FILL.iter().all(|p| p.len() >= 3));
        assert!(OPEN_FILL.iter().all(|p| p.len() >= 3));
        assert!(CLOSED_OUTLINE.len() >= 3);
        assert!(OPEN_OUTLINE.len() >= 3);
    }

    #[test]
    fn fill_polygons_are_convex() {
        for polygon in CLOSED_FILL.iter().chain(OPEN_FILL.iter()) {
            assert!(is_convex(polygon), "non-convex fill polygon: {polygon:?}");
        }
    }

    #[test]
    fn arrow_back_fill_polygons_are_convex() {
        for polygon in ARROW_BACK_FILL {
            assert!(is_convex(polygon), "non-convex fill polygon: {polygon:?}");
        }
    }

    #[test]
    fn arrow_back_vertices_stay_within_the_viewbox() {
        assert!(ARROW_BACK_FILL.iter().all(|polygon| polygon.len() >= 3));
        for [x, y] in ARROW_BACK_FILL.iter().flat_map(|polygon| polygon.iter()) {
            assert!(
                (0.0..=VIEWBOX).contains(x) && (0.0..=VIEWBOX).contains(y),
                "vertex ({x}, {y}) leaves the viewBox"
            );
        }
    }

    #[test]
    fn bookmark_fill_polygons_are_convex() {
        for polygon in BOOKMARK_FILL {
            assert!(is_convex(polygon), "non-convex fill polygon: {polygon:?}");
        }
    }

    #[test]
    fn bookmark_vertices_stay_within_the_viewbox() {
        assert!(BOOKMARK_FILL.iter().all(|polygon| polygon.len() >= 3));
        assert!(BOOKMARK_OUTLINE.len() >= 5);
        let vertices = BOOKMARK_FILL
            .iter()
            .flat_map(|polygon| polygon.iter())
            .chain(BOOKMARK_OUTLINE.iter());
        for [x, y] in vertices {
            assert!(
                (0.0..=VIEWBOX).contains(x) && (0.0..=VIEWBOX).contains(y),
                "vertex ({x}, {y}) leaves the viewBox"
            );
        }
    }

    #[test]
    fn normalized_vertices_stay_within_the_viewbox() {
        let fills = CLOSED_FILL.iter().chain(OPEN_FILL.iter());
        let vertices = fills
            .flat_map(|polygon| polygon.iter())
            .chain([CLOSED_OUTLINE, OPEN_OUTLINE].into_iter().flatten());
        for [x, y] in vertices {
            assert!(
                (0.0..=VIEWBOX).contains(x) && (0.0..=VIEWBOX).contains(y),
                "vertex ({x}, {y}) leaves the viewBox"
            );
        }
    }

    #[test]
    fn window_fill_polygons_are_convex() {
        for polygon in WINDOW_CLOSE_FILL.iter().chain(WINDOW_MINIMIZE_FILL.iter()) {
            assert!(is_convex(polygon), "non-convex fill polygon: {polygon:?}");
        }
    }

    #[test]
    fn window_glyph_vertices_stay_within_the_cell() {
        let vertices = WINDOW_CLOSE_FILL
            .iter()
            .chain(WINDOW_MINIMIZE_FILL.iter())
            .flat_map(|polygon| polygon.iter())
            .chain(
                [
                    WINDOW_MAXIMIZE_PATH,
                    WINDOW_RESTORE_BACK,
                    WINDOW_RESTORE_FRONT,
                ]
                .into_iter()
                .flatten(),
            );
        for [x, y] in vertices {
            let (x, y) = (x + VIEWBOX / 2.0, y + VIEWBOX / 2.0);
            assert!(
                (0.0..=VIEWBOX).contains(&x) && (0.0..=VIEWBOX).contains(&y),
                "vertex ({x}, {y}) leaves the icon cell"
            );
        }
    }

    #[test]
    fn window_icon_cell_is_positive() {
        const { assert!(WINDOW_ICON_PX > 0.0) };
    }

    #[test]
    fn dock_icon_is_vertically_centred_on_its_button() {
        let rect = Rect::from_center_size(Pos2::new(40.0, 30.0), egui::Vec2::splat(18.0));
        let (front, back) = dock_rects(rect);
        let union = Rect::from_min_max(front.min.min(back.min), front.max.max(back.max));
        assert!((union.center().x - rect.center().x).abs() < 1e-4);
        assert!((union.center().y - rect.center().y).abs() < 1e-4);
        assert!(rect.contains_rect(union), "the icon stays inside its cell");
    }

    #[test]
    fn check_vertices_stay_within_the_cell() {
        assert_eq!(CHECK_POINTS.len(), 3);
        for [x, y] in CHECK_POINTS {
            assert!(
                (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y),
                "check vertex ({x}, {y}) leaves the normalized cell"
            );
        }
        let cell = Rect::from_min_size(Pos2::ZERO, egui::vec2(16.0, 16.0));
        let half = CHECK_STROKE * 0.5;
        for [x, y] in CHECK_POINTS {
            let point = map_unit_to_rect(cell, [*x, *y]);
            assert!(
                point.x - half >= cell.min.x
                    && point.x + half <= cell.max.x
                    && point.y - half >= cell.min.y
                    && point.y + half <= cell.max.y,
                "check vertex ({point:?}) with its stroke leaves the cell"
            );
        }
    }

    #[test]
    fn settings_gear_is_clockwise_and_within_the_cell() {
        assert!(SETTINGS_GEAR.len() >= 3);
        let area: f32 = (0..SETTINGS_GEAR.len())
            .map(|index| {
                let a = SETTINGS_GEAR[index];
                let b = SETTINGS_GEAR[(index + 1) % SETTINGS_GEAR.len()];
                a[0] * b[1] - b[0] * a[1]
            })
            .sum();
        assert!(
            area > 0.0,
            "the gear contour must wind screen-clockwise for the inside stroke"
        );
        for [x, y] in SETTINGS_GEAR {
            let (x, y) = (x + VIEWBOX / 2.0, y + VIEWBOX / 2.0);
            assert!(
                (0.0..=VIEWBOX).contains(&x) && (0.0..=VIEWBOX).contains(&y),
                "vertex ({x}, {y}) leaves the icon cell"
            );
        }
    }
}
