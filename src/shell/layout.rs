//! Three-column collapsible layout: `[tree | editor | preview]` with drag splitters and
//! collapse-to-strip buttons. Column sizes are stored as proportional weights, so collapsing a
//! pane never disturbs the ratios of the panes that stay open; splitter drags trade width only
//! between the two adjacent panes.
//!
//! The three columns are child nodes of the window's single tui tree (created by the frame
//! skeleton): each pane is one container with a painted card background, a header row and a
//! body container. Widths come from [`pane_widths`] — the same taffy style computation the tree
//! performs, without egui — so the splitter zones and the persisted weights never need to
//! read back egui_taffy's internal layout (see `docs/design/egui-taffy-migration-plan.md`).
//! The CSS freeze iteration inside taffy is numerically identical to the former iterative
//! solver (plan §11).
//!
//! Each pane has a third state besides open and collapsed: *popped out*, where the pane's
//! content renders in its own OS window and the main-window column degrades to a 32px dock
//! strip. A popped pane is still visible, so it behaves like an open pane for widths and for
//! [`ColumnLayout::can_collapse`]; only the strip decision treats it like a collapsed one. See
//! docs/architecture.md §7.

use egui::{CornerRadius, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind};
use typ_edit_i18n::t;
use typ_edit_widgets::theme::Theme;
use typ_edit_widgets::tui::{
    self, Tui, TuiBuilderLogic,
    taffy::{
        AlignItems, AvailableSpace, Dimension, Display, FlexDirection, LengthPercentage,
        Rect as TaffyRect, Size, Style, TaffyTree,
    },
};

use crate::theme;

/// Identifies a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Tree = 0,
    Editor = 1,
    Preview = 2,
}

/// All panes in draw order.
const PANES: [Pane; 3] = [Pane::Tree, Pane::Editor, Pane::Preview];
/// Short header titles (only shown while a pane is open). Shared with the
/// frame loop, which titles each popped pane's OS window from the same keys.
pub(crate) const PANE_TITLES: [&str; 3] = ["pane.tree", "pane.editor", "pane.preview"];

/// Gap between cards, and between a card and the window edge.
const CARD_GAP: f32 = 10.0;
/// Extra breathing room above every pane, below the title strip.
const PANE_TOP_GAP: f32 = CARD_GAP + 4.0;
/// Half gap: the inset towards a neighbouring card or the status bar.
const CARD_HALF_GAP: f32 = CARD_GAP * 0.5;
/// Horizontal content padding inside an open card.
const CARD_PAD_X: f32 = 6.0;
/// Bottom content padding inside a card.
const CARD_PAD_Y: f32 = 4.0;
/// Corner radius of a full card.
pub(crate) const CARD_RADIUS: u8 = 10;
/// Corner radius of a collapsed 24px strip.
const STRIP_RADIUS: u8 = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CardMetrics {
    gap: f32,
    top_gap: f32,
    half_gap: f32,
    radius: u8,
    strip_radius: u8,
}

impl CardMetrics {
    const NORMAL: Self = Self {
        gap: CARD_GAP,
        top_gap: PANE_TOP_GAP,
        half_gap: CARD_HALF_GAP,
        radius: CARD_RADIUS,
        strip_radius: STRIP_RADIUS,
    };

    const COMPACT: Self = Self {
        gap: 0.0,
        top_gap: 0.0,
        half_gap: 0.0,
        radius: 0,
        strip_radius: 0,
    };

    fn for_compact(compact: bool) -> Self {
        if compact { Self::COMPACT } else { Self::NORMAL }
    }
}

/// Persistent layout state.
///
/// `widths` are proportional weights rather than absolute widths: while every pane is open
/// they add up to the available width, and collapsing a pane never rescales them. Collapse and
/// expand are therefore automatically ratio-preserving — expanding restores the pre-collapse
/// arrangement exactly as long as the available width is unchanged. A splitter drag moves
/// width only between its two adjacent panes (the third pane is untouched), while proportional
/// redistribution stays reserved for collapse/expand.
pub struct ColumnLayout {
    pub widths: [f32; 3],
    pub collapsed: [bool; 3],
    /// Panes rendered in their own OS window. Runtime-only: never persisted, so
    /// a fresh session always starts fully docked. Invariant: `popped[i]` implies
    /// `!collapsed[i]`.
    pub popped: [bool; 3],
    /// Card rects painted by the most recent [`Self::add_panes`] pass.
    pub frame_pane_rects: [Rect; 3],
    /// Whether compact card metrics are active for this frame.
    pub compact: bool,
    /// Previous frame's card rects, used to request one settling frame when the
    /// shell geometry moves for any reason.
    previous_pane_rects: [Rect; 3],
}

impl Default for ColumnLayout {
    fn default() -> Self {
        Self {
            widths: [220.0, 560.0, 520.0],
            collapsed: [false, false, false],
            popped: [false; 3],
            frame_pane_rects: [Rect::NOTHING; 3],
            compact: false,
            previous_pane_rects: [Rect::NOTHING; 3],
        }
    }
}

impl ColumnLayout {
    /// Returns whether this frame's card geometry differs from the previous
    /// frame, advancing the snapshot. Used by the shell to request one more
    /// frame after any layout movement that was not itself pointer-driven.
    pub fn observe_frame_layout(&mut self) -> bool {
        let changed = self
            .frame_pane_rects
            .iter()
            .zip(self.previous_pane_rects)
            .any(|(&now, before)| !rects_close(now, before));
        self.previous_pane_rects = self.frame_pane_rects;
        changed
    }
}

fn rects_close(a: Rect, b: Rect) -> bool {
    a == b || ((a.min - b.min).length_sq() <= 0.01 && (a.max - b.max).length_sq() <= 0.01)
}

/// Builds the taffy styles of the three pane slots.
///
/// Shared by the rendered tree and [`pane_widths`], so the policy stays single-sourced:
///
/// * stripped panes (collapsed or popped) keep a fixed basis that leaves a
///   visible [`ColumnLayout::STRIP_W`] card after their own insets (equal growth
///   when *every* pane is stripped, reproducing the former fill behaviour);
/// * open panes share the free space in proportion to their weights
///   (equal shares when all open weights are zero);
/// * while the open space can host every open pane at [`ColumnLayout::MIN_OPEN_W`],
///   the min-size floor applies; below that threshold it is dropped so the panes
///   scale down proportionally instead of overflowing (former policy);
/// * each pane is a column container; its padding reproduces the former card
///   insets and content padding, so the header/body children land where the
///   manual `header_rect`/`body_rect` did.
fn pane_styles(
    available_w: f32,
    weights: [f32; 3],
    stripped: [bool; 3],
    metrics: CardMetrics,
) -> [Style; 3] {
    let stripped_count = stripped.iter().filter(|s| **s).count();
    let open_count = 3 - stripped_count;
    let avail = if available_w.is_finite() {
        available_w.max(0.0)
    } else {
        0.0
    };
    let stripped_total: f32 = (0..3).filter(|&i| stripped[i]).map(strip_slot_width).sum();
    let open_space = (avail - stripped_total).max(0.0);
    let floor = if open_count > 0 && open_space >= ColumnLayout::MIN_OPEN_W * open_count as f32 {
        ColumnLayout::MIN_OPEN_W
    } else {
        0.0
    };
    let open_total: f32 = (0..3).filter(|&i| !stripped[i]).map(|i| weights[i]).sum();
    let all_stripped = open_count == 0;

    std::array::from_fn(|i| {
        let mut style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: Some(AlignItems::Stretch),
            ..Default::default()
        };
        if stripped[i] {
            // With no open pane left, equal growth keeps the three dock strips
            // evenly sized instead of inheriting the asymmetric edge widths.
            style.flex_basis = Dimension::length(if all_stripped {
                0.0
            } else {
                strip_slot_width_for(i, metrics)
            });
            style.flex_grow = if all_stripped { 1.0 } else { 0.0 };
            style.flex_shrink = 1.0;
        } else {
            style.flex_basis = Dimension::length(0.0);
            style.flex_grow = if open_total > 0.0 { weights[i] } else { 1.0 };
            style.flex_shrink = 1.0;
            style.min_size = Size {
                width: Dimension::length(floor),
                height: Dimension::auto(),
            };
        }
        style
    })
}

/// Left/right card insets of pane `i`: full gap at the window edges, half gap
/// towards neighbours (adjacent insets add up to one uniform seam).
fn pane_insets_for(i: usize, metrics: CardMetrics) -> (f32, f32) {
    let l = if i == 0 {
        metrics.gap
    } else {
        metrics.half_gap
    };
    let r = if i == 2 {
        metrics.gap
    } else {
        metrics.half_gap
    };
    (l, r)
}

#[cfg(test)]
fn pane_insets(i: usize) -> (f32, f32) {
    pane_insets_for(i, CardMetrics::NORMAL)
}

/// Slot width of a stripped pane: the visible strip card plus both of its
/// horizontal insets. Edge panes carry the full outer gap, so their slot is
/// wider than the middle strip's; using one constant for all three made the
/// edge cards 5pt narrower than `STRIP_W`.
fn strip_slot_width(i: usize) -> f32 {
    strip_slot_width_for(i, CardMetrics::NORMAL)
}

fn strip_slot_width_for(i: usize, metrics: CardMetrics) -> f32 {
    let (l, r) = pane_insets_for(i, metrics);
    ColumnLayout::STRIP_W + l + r
}

/// Pure policy mirror of the rendered three-column layout: resolves the pane
/// slot widths for a given available width without egui.
///
/// Rounding is disabled so the result matches the pre-taffy policy within the
/// tests' f32 epsilon (the rendered tree goes through egui_taffy, whose taffy
/// tree keeps taffy's default whole-point rounding).
///
/// The caller guarantees at least one open pane (see the defensive re-open in
/// [`ColumnLayout::add_panes`]); for an all-stripped input the strips fall back
/// to the former equal-fill behaviour.
#[cfg(test)]
pub fn pane_widths(available_w: f32, weights: [f32; 3], stripped: [bool; 3]) -> [f32; 3] {
    pane_widths_with(available_w, weights, stripped, false)
}

/// [`pane_widths`] with the compact card metrics selected by `compact`.
pub fn pane_widths_with(
    available_w: f32,
    weights: [f32; 3],
    stripped: [bool; 3],
    compact: bool,
) -> [f32; 3] {
    let avail = if available_w.is_finite() {
        available_w.max(0.0)
    } else {
        0.0
    };
    if avail <= 0.0 {
        return [0.0; 3];
    }

    let styles = pane_styles(avail, weights, stripped, CardMetrics::for_compact(compact));
    let mut tree: TaffyTree<()> = TaffyTree::new();
    tree.disable_rounding();
    let children: Vec<_> = styles
        .into_iter()
        .map(|style| tree.new_leaf(style).expect("valid leaf style"))
        .collect();
    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Stretch),
                size: Size {
                    width: Dimension::length(avail),
                    height: Dimension::length(1.0),
                },
                ..Default::default()
            },
            &children,
        )
        .expect("valid row style");
    tree.compute_layout(
        root,
        Size {
            width: AvailableSpace::Definite(avail),
            height: AvailableSpace::Definite(1.0),
        },
    )
    .expect("layout computation");
    std::array::from_fn(|i| tree.layout(children[i]).expect("child layout").size.width)
}

/// One resize seam between two open panes. `seam` is the slot whose right edge
/// the splitter sits on; `left`/`right` are the open panes that trade width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SplitterTarget {
    left: usize,
    right: usize,
    seam: usize,
}

/// Resize seams for the current visibility. Adjacent open panes get one seam.
/// When only the middle pane is stripped, the two outer panes are still
/// separated by two visible gaps (one on each side of the dock strip); both are
/// draggable and trade width between the two open panes, so collapsing the
/// middle pane never freezes the outer split.
fn splitter_targets(stripped: [bool; 3]) -> Vec<SplitterTarget> {
    let open: Vec<usize> = (0..3).filter(|&i| !stripped[i]).collect();
    let mut targets = Vec::new();
    for pair in open.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        if right == left + 1 {
            targets.push(SplitterTarget {
                left,
                right,
                seam: left,
            });
        } else {
            // Exactly one stripped pane lies between the pair (there are only
            // three columns): expose both gaps around its dock strip.
            targets.push(SplitterTarget {
                left,
                right,
                seam: left,
            });
            targets.push(SplitterTarget {
                left,
                right,
                seam: right - 1,
            });
        }
    }
    targets
}

/// Applies one splitter drag to the live slot widths, keeping both open panes
/// at or above [`ColumnLayout::MIN_OPEN_W`] when the available space allows it.
/// Returns whether any width changed.
fn apply_splitter_delta(widths: &mut [f32; 3], target: SplitterTarget, delta: f32) -> bool {
    let delta_min = ColumnLayout::MIN_OPEN_W - widths[target.left];
    let delta_max = widths[target.right] - ColumnLayout::MIN_OPEN_W;
    if delta_min > delta_max {
        // Both neighbours are already below the floor (a scaled-down window):
        // no delta can satisfy it, so the drag is ignored.
        return false;
    }
    let delta = delta.clamp(delta_min, delta_max);
    if delta == 0.0 {
        return false;
    }
    widths[target.left] += delta;
    widths[target.right] -= delta;
    true
}

/// Merges a splitter drag's live pixel widths back into the persistent weights.
/// Stripped panes keep their stored weight: their live slot is the fixed dock
/// width, and persisting that would shrink the pane when it is expanded again.
fn splitter_weights(live: [f32; 3], stored: [f32; 3], stripped: [bool; 3]) -> [f32; 3] {
    std::array::from_fn(|i| if stripped[i] { stored[i] } else { live[i] })
}

/// Header container style for pane `i`: a centered row of `gap`-spaced items.
///
/// taffy 0.9.2 adds padding on top of the flex-distributed size, so the pane
/// container itself carries no padding: the card insets and paddings live on the
/// header/body children. The header's padding reproduces the former
/// `header_rect.shrink2(vec2(2, 1))` content box; its border box spans
/// `PANE_TOP_GAP + HEADER_H` from the pane slot top, so the body starts exactly
/// at the former `header_rect.bottom()`.
fn header_row_style(inset_l: f32, inset_r: f32, gap: f32, metrics: CardMetrics) -> Style {
    let mut style = tui::row();
    style.align_items = Some(AlignItems::Center);
    style.flex_basis = Dimension::length(ColumnLayout::HEADER_H - 2.0);
    style.flex_grow = 0.0;
    style.flex_shrink = 1.0;
    style.padding = TaffyRect {
        left: LengthPercentage::length(inset_l + CARD_PAD_X + 2.0),
        right: LengthPercentage::length(inset_r + CARD_PAD_X + 2.0),
        top: LengthPercentage::length(metrics.top_gap + 1.0),
        bottom: LengthPercentage::length(1.0),
    };
    style.gap = Size {
        width: LengthPercentage::length(gap),
        height: LengthPercentage::length(0.0),
    };
    style
}

/// Body container style for pane `i`: takes the remaining height below the
/// header and insets the content like the former `body_rect` (card pad + bottom
/// pad). The body is a column container now: pane content adds its own child
/// nodes to it (one tree, no nested `tui()`).
fn body_slot_style(inset_l: f32, inset_r: f32, metrics: CardMetrics) -> Style {
    let mut style = tui::column();
    tui::fill_main(&mut style);
    style.padding = TaffyRect {
        left: LengthPercentage::length(inset_l + CARD_PAD_X),
        right: LengthPercentage::length(inset_r + CARD_PAD_X),
        top: LengthPercentage::length(0.0),
        bottom: LengthPercentage::length(metrics.half_gap + CARD_PAD_Y),
    };
    style
}

/// Horizontal padding of the strip toggle leaf: the visible card runs from
/// `inset_l` to `slot - inset_r`, and the button is centred inside that card.
/// The leaf itself spans the whole slot, so the edge strips' asymmetric insets
/// cannot push the button off-centre.
fn strip_toggle_padding(inset_l: f32, inset_r: f32, btn: f32) -> (f32, f32) {
    let card_pad = (ColumnLayout::STRIP_W - btn).max(0.0) * 0.5;
    (inset_l + card_pad, inset_r + card_pad)
}

/// Strip toggle leaf style: the button centered on the visible strip card, with
/// the vertical extent of the former header band so the title leaf starts at
/// the former `body_rect.top`.
fn strip_toggle_style(inset_l: f32, inset_r: f32, btn: f32, metrics: CardMetrics) -> Style {
    let pad_top = metrics.top_gap + (ColumnLayout::HEADER_H - btn) * 0.5;
    let pad_bottom = (ColumnLayout::HEADER_H - btn) - (ColumnLayout::HEADER_H - btn) * 0.5;
    let (pad_left, pad_right) = strip_toggle_padding(inset_l, inset_r, btn);
    // `size` is the border box in taffy 0.9.2: the leaf spans the slot, and its
    // asymmetric horizontal padding makes the button's content box exactly
    // `btn` wide and centred on the painted card.
    let header_h = metrics.top_gap + ColumnLayout::HEADER_H;
    let mut style = Style {
        // The parent pane is a column, so flex-basis is the leaf's height; a
        // zero basis would collapse it to its padding and pull the strip title
        // up under the toggle.
        flex_basis: Dimension::length(header_h),
        flex_grow: 0.0,
        flex_shrink: 0.0,
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::length(header_h),
        },
        ..Default::default()
    };
    style.padding = TaffyRect {
        left: LengthPercentage::length(pad_left),
        right: LengthPercentage::length(pad_right),
        top: LengthPercentage::length(pad_top),
        bottom: LengthPercentage::length(pad_bottom),
    };
    style
}

/// Strip title leaf style: the former `body_rect` for a stripped pane.
fn strip_title_style(inset_l: f32, inset_r: f32, metrics: CardMetrics) -> Style {
    let mut style = tui::flexible();
    style.padding = TaffyRect {
        left: LengthPercentage::length(inset_l),
        right: LengthPercentage::length(inset_r),
        top: LengthPercentage::length(0.0),
        bottom: LengthPercentage::length(metrics.half_gap + CARD_PAD_Y),
    };
    style
}

/// An exactly-sized leaf (header buttons and the strip toggle).
fn fixed_slot(w: f32, h: f32) -> Style {
    Style {
        flex_basis: Dimension::length(w),
        flex_grow: 0.0,
        flex_shrink: 0.0,
        size: Size {
            width: Dimension::length(w),
            height: Dimension::length(h),
        },
        ..Default::default()
    }
}

/// The pane container id of pane `i` (stable across frames, independent of any
/// Ui-derived id).
fn pane_node_id(i: usize) -> Id {
    Id::new("typ_edit_shell_pane").with(i)
}

impl ColumnLayout {
    /// Width of a collapsed pane in points.
    pub const STRIP_W: f32 = 32.0;
    /// Minimum width of an open pane.
    pub const MIN_OPEN_W: f32 = 120.0;
    /// Width of the interactive splitter zone between two adjacent panes.
    ///
    /// Matches the 10px card seam, so the visible gap and the hit area coincide.
    pub const SPLITTER_W: f32 = 10.0;
    /// Height of each pane's header strip.
    pub const HEADER_H: f32 = 32.0;

    pub fn new(widths: [f32; 3], collapsed: [bool; 3]) -> Self {
        Self {
            widths,
            collapsed,
            popped: [false; 3],
            frame_pane_rects: [Rect::NOTHING; 3],
            compact: false,
            previous_pane_rects: [Rect::NOTHING; 3],
        }
    }

    /// Whether a pane renders as a 24px strip in the main window: either it is
    /// collapsed, or it has been popped out into its own window.
    pub fn is_stripped(&self, pane: usize) -> bool {
        self.collapsed[pane] || self.popped[pane]
    }

    /// The per-pane strip flags [`pane_widths`] consumes, treating a popped
    /// pane exactly like a collapsed one so it gets the strip width.
    pub fn effective_collapsed(&self) -> [bool; 3] {
        [
            self.is_stripped(0),
            self.is_stripped(1),
            self.is_stripped(2),
        ]
    }

    /// Applies a pop-out / dock toggle.
    ///
    /// Popping out is refused while the pane is collapsed (the invariant
    /// `popped => !collapsed` holds); docking always clears the flag. Widths are
    /// weights and are never touched, so docking restores the pane's open width.
    fn apply_pop_toggle(&mut self, pane: usize, popped: bool) {
        if popped {
            if !self.collapsed[pane] {
                self.popped[pane] = true;
            }
        } else {
            self.popped[pane] = false;
        }
    }

    /// Applies a collapse/expand toggle.
    ///
    /// Widths are weights and are never touched here: collapsing only flips the flag, so
    /// expanding the pane later brings its share back. The collapse direction is rejected while
    /// [`Self::can_collapse`] is false; expanding is always allowed.
    fn apply_toggle(&mut self, pane: usize, collapse: bool) {
        if collapse {
            if self.can_collapse() {
                self.collapsed[pane] = true;
            }
        } else {
            self.collapsed[pane] = false;
        }
    }

    pub fn open_count(&self) -> usize {
        self.collapsed.iter().filter(|c| !**c).count()
    }

    /// Whether any pane may be collapsed at all (needs > 1 open panes).
    pub fn can_collapse(&self) -> bool {
        self.open_count() > 1
    }

    /// Adds the three pane nodes to the window's tui tree (`tree`).
    ///
    /// Each pane is one container with a painted card background (insets from
    /// the pane slot rect), a header row (collapse / pop-out buttons and the
    /// title) and a body container running `add_contents` with the same tree;
    /// stripped panes render the toggle button and the vertical title instead.
    /// Header actions are applied right after the header, before the body
    /// container is added.
    ///
    /// `avail_w` is the width of the three-column area (the window width — the
    /// row spans the full central band).
    pub fn add_panes(
        &mut self,
        tree: &mut Tui,
        avail_w: f32,
        add_contents: &mut dyn FnMut(Pane, &mut Tui),
    ) {
        // Defensive: an all-collapsed layout must never happen (the UI gates collapsing on
        // `can_collapse`, but persisted settings could still contain it).
        if self.open_count() == 0 {
            self.collapsed[Pane::Editor as usize] = false;
        }

        // Styles come from the persistent weights; the weights themselves are left
        // untouched here (only splitter drags update them), which is what makes collapse
        // and expand ratio-preserving.
        let stripped = self.effective_collapsed();
        let metrics = CardMetrics::for_compact(self.compact);
        let styles = pane_styles(avail_w, self.widths, stripped, metrics);

        let theme = Theme::from_ui(tree.egui_ui());
        let btn = theme.spacing.icon_btn_small;
        let item_spacing = tree.egui_ui().spacing().item_spacing.x;
        let mut frame_rects = [Rect::NOTHING; 3];

        enum HeaderAction {
            Collapse(bool),
            Pop,
            Dock,
        }

        for i in 0..3 {
            let pane = PANES[i];
            let strip = stripped[i];
            let (inset_l, inset_r) = pane_insets_for(i, metrics);
            // Flags read before the closures; the closures never touch `tree`.
            let popped_i = self.popped[i];
            let collapsed_i = self.collapsed[i];
            let can_collapse = self.can_collapse();
            let mut action: Option<HeaderAction> = None;

            tree.style(styles[i].clone())
                .id(pane_node_id(i))
                .add_with_background_ui(
                    |ui, container| {
                        // Card: the pane slot rect, inset to the visible card.
                        let slot = container.full_container();
                        let card_rect = Rect::from_min_max(
                            egui::pos2(slot.left() + inset_l, slot.top() + metrics.top_gap),
                            egui::pos2(slot.right() - inset_r, slot.bottom() - metrics.half_gap),
                        );
                        if card_rect.width() >= 1.0 && card_rect.height() >= 1.0 {
                            frame_rects[i] = card_rect;
                            let radius = if strip {
                                metrics.strip_radius
                            } else {
                                metrics.radius
                            };
                            let painter = ui.painter();
                            painter.rect_filled(
                                card_rect,
                                CornerRadius::same(radius),
                                theme::card(ui),
                            );
                            painter.rect_stroke(
                                card_rect,
                                CornerRadius::same(radius),
                                Stroke::new(1.0, theme::card_stroke(ui)),
                                StrokeKind::Inside,
                            );
                        }
                    },
                    |tree, _| {
                        if strip {
                            // A single centered toggle/dock button above the vertical title.
                            tree.style(strip_toggle_style(inset_l, inset_r, btn, metrics))
                                .id(pane_node_id(i).with("strip_toggle"))
                                .ui(|ui| {
                                    let response = if popped_i {
                                        typ_edit_widgets::button::icon_button_painted(
                                            ui,
                                            btn,
                                            true,
                                            |ui, rect, color| {
                                                typ_edit_widgets::icons::dock(ui, rect, color)
                                            },
                                        )
                                        .on_hover_text(t("pane.pop_in"))
                                    } else if collapsed_i {
                                        typ_edit_widgets::button::icon_button_painted(
                                            ui,
                                            btn,
                                            true,
                                            |ui, rect, color| {
                                                typ_edit_widgets::icons::chevron(
                                                    ui, rect, false, color,
                                                )
                                            },
                                        )
                                        .on_hover_text(t("pane.expand"))
                                    } else {
                                        return;
                                    };
                                    if response.clicked() {
                                        action = Some(if popped_i {
                                            HeaderAction::Dock
                                        } else {
                                            HeaderAction::Collapse(false)
                                        });
                                    }
                                });
                            match action {
                                Some(HeaderAction::Collapse(collapsed)) => {
                                    self.apply_toggle(i, collapsed)
                                }
                                Some(HeaderAction::Pop) => self.apply_pop_toggle(i, true),
                                Some(HeaderAction::Dock) => self.apply_pop_toggle(i, false),
                                None => {}
                            }
                            tree.style(strip_title_style(inset_l, inset_r, metrics))
                                .id(pane_node_id(i).with("strip_title"))
                                .ui(|ui| {
                                    strip_title(ui, ui.max_rect(), &t(PANE_TITLES[i]));
                                });
                        } else {
                            tree.style(header_row_style(inset_l, inset_r, item_spacing, metrics))
                                .id(pane_node_id(i).with("header"))
                                .add(|tree| {
                                    tree.style(fixed_slot(btn, btn))
                                        .id(pane_node_id(i).with("collapse_btn"))
                                        .ui(|ui| {
                                            let response =
                                                typ_edit_widgets::button::icon_button_painted(
                                                    ui,
                                                    btn,
                                                    can_collapse,
                                                    |ui, rect, color| {
                                                        typ_edit_widgets::icons::chevron_left(
                                                            ui, rect, color,
                                                        )
                                                    },
                                                );
                                            let response = if can_collapse {
                                                response.on_hover_text(t("pane.collapse"))
                                            } else {
                                                response.on_disabled_hover_text(t("pane.keep_two"))
                                            };
                                            if response.clicked() {
                                                action = Some(HeaderAction::Collapse(true));
                                            }
                                        });
                                    // The pop-out button has no disabled state: even the
                                    // last docked pane may be popped into its own window.
                                    tree.style(fixed_slot(btn, btn))
                                        .id(pane_node_id(i).with("pop_btn"))
                                        .ui(|ui| {
                                            let pop =
                                                typ_edit_widgets::button::icon_button_painted(
                                                    ui,
                                                    btn,
                                                    true,
                                                    |ui, rect, color| {
                                                        typ_edit_widgets::icons::dock(
                                                            ui, rect, color,
                                                        )
                                                    },
                                                )
                                                .on_hover_text(t("pane.pop_out"));
                                            if pop.clicked() {
                                                action = Some(HeaderAction::Pop);
                                            }
                                        });
                                    tree.wrap_mode(egui::TextWrapMode::Extend).label(
                                        egui::RichText::new(t(PANE_TITLES[i]))
                                            .small()
                                            .color(theme.colors.text_muted),
                                    );
                                });
                            // Same-frame effect: the action flips the live flags before the
                            // body leaf is added (the body itself follows the pre-click strip
                            // decision for this transitional frame).
                            match action {
                                Some(HeaderAction::Collapse(collapsed)) => {
                                    self.apply_toggle(i, collapsed)
                                }
                                Some(HeaderAction::Pop) => self.apply_pop_toggle(i, true),
                                Some(HeaderAction::Dock) => self.apply_pop_toggle(i, false),
                                None => {}
                            }
                            tree.style(body_slot_style(inset_l, inset_r, metrics))
                                .id(pane_node_id(i).with("body"))
                                .add(|tree| {
                                    // Clip the pane content to the former body
                                    // content box (the body node's padding is
                                    // not part of the scroll/row area).
                                    let mut clip = tree.egui_ui().max_rect();
                                    clip.min.x += inset_l + CARD_PAD_X;
                                    clip.max.x -= inset_r + CARD_PAD_X;
                                    clip.max.y -= metrics.half_gap + CARD_PAD_Y;
                                    let clip = clip.intersect(tree.egui_ui().clip_rect());
                                    tree.egui_ui_mut().set_clip_rect(clip);
                                    add_contents(pane, tree);
                                });
                        }
                    },
                );
        }
        self.frame_pane_rects = frame_rects;
    }

    /// Registers splitter drag zones over `area` (the three-column band),
    /// after the tree so the zones win overlapping hit-tests (the standard
    /// "resize handle on top" behaviour). A drag is applied on the next frame.
    ///
    /// With the middle pane stripped, the outer panes still get both gaps
    /// around the dock strip as resize seams; either one trades width between
    /// the two open panes.
    pub fn splitters(&mut self, ui: &mut egui::Ui, area: Rect, avail_w: f32) {
        let stripped = self.effective_collapsed();
        let mut widths = pane_widths_with(avail_w, self.widths, stripped, self.compact);

        for (index, target) in splitter_targets(stripped).into_iter().enumerate() {
            // Recompute the seam for every target: a drag below may already
            // have changed this frame's live widths.
            let boundary = area.left() + widths[..=target.seam].iter().sum::<f32>();
            let zone = Rect::from_min_max(
                egui::pos2(boundary - Self::SPLITTER_W * 0.5, area.top()),
                egui::pos2(boundary + Self::SPLITTER_W * 0.5, area.bottom()),
            );

            let response = ui.interact(
                zone,
                Id::new("typ_edit_shell_splitter").with(index),
                Sense::drag(),
            );
            let active = response.hovered() || response.dragged();
            if active {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if response.dragged()
                && apply_splitter_delta(&mut widths, target, response.drag_delta().x)
            {
                // Persist every *open* pane's live width: they are scaled to the
                // current available width while the stored weights are not, so
                // mixing them would rescale the third pane. Stripped panes are
                // excluded because their live width is the dock slot, not their
                // expandable weight.
                self.widths = splitter_weights(widths, self.widths, stripped);
            }
            if active {
                let handle = Rect::from_center_size(zone.center(), egui::vec2(2.0, zone.height()));
                ui.painter().rect_filled(
                    handle,
                    CornerRadius::same(1),
                    ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.5),
                );
            }
        }
    }
}

/// Draws a collapsed pane's title vertically (one character per line) centered in its strip.
fn strip_title(ui: &egui::Ui, rect: Rect, title: &str) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let row_h = ui.text_style_height(&egui::TextStyle::Small);
    let color = Theme::from_ui(ui).colors.text_muted;
    let chars: Vec<char> = title.chars().collect();
    let mut y = rect.center().y - row_h * chars.len() as f32 * 0.5;
    for ch in chars {
        ui.painter().text(
            egui::pos2(rect.center().x, y),
            egui::Align2::CENTER_TOP,
            ch,
            font.clone(),
            color,
        );
        y += row_h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 0.05;
    const EPS_RATIO: f32 = 1e-4;

    fn sum(w: [f32; 3]) -> f32 {
        w.iter().sum()
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < EPS,
            "expected {expected}, got {actual}"
        );
    }

    /// Slot width of the collapsed pane at `index`: the visible strip plus
    /// that pane's outer/inner insets.
    fn collapsed_width(index: usize) -> f32 {
        strip_slot_width(index)
    }

    #[test]
    fn compact_metrics_remove_gaps_and_card_radii() {
        let metrics = CardMetrics::COMPACT;
        for i in 0..3 {
            assert_eq!(pane_insets_for(i, metrics), (0.0, 0.0));
            assert_close(strip_slot_width_for(i, metrics), ColumnLayout::STRIP_W);
        }
        assert_eq!(metrics.radius, 0);
        assert_eq!(metrics.strip_radius, 0);
        let widths = pane_widths_with(1000.0, [220.0, 560.0, 520.0], [false; 3], true);
        assert_close(sum(widths), 1000.0);
    }

    #[test]
    fn full_space_split_reproduces_the_weights() {
        let weights = [220.0, 560.0, 520.0];
        let w = pane_widths(sum(weights), weights, [false; 3]);
        assert_close(w[0], weights[0]);
        assert_close(w[1], weights[1]);
        assert_close(w[2], weights[2]);
        assert_close(sum(w), sum(weights));
    }

    #[test]
    fn half_space_halves_every_width_proportionally() {
        // Every pane keeps its share of the space, so halving the available width halves each
        // pane. All halves stay above MIN_OPEN_W, so the floor does not interfere.
        let weights = [260.0, 560.0, 480.0];
        let w = pane_widths(650.0, weights, [false; 3]);
        assert_close(w[0], 130.0);
        assert_close(w[1], 280.0);
        assert_close(w[2], 240.0);
        assert_close(sum(w), 650.0);
    }

    #[test]
    fn spare_space_grows_the_open_panes_proportionally() {
        let weights = [220.0, 560.0, 520.0];
        let w = pane_widths(2600.0, weights, [false; 3]);
        assert_close(sum(w), 2600.0);
        assert!((w[0] / w[1] - 220.0 / 560.0).abs() < EPS_RATIO, "{w:?}");
        assert!((w[1] / w[2] - 560.0 / 520.0).abs() < EPS_RATIO, "{w:?}");
    }

    #[test]
    fn collapse_keeps_the_open_panes_ratio() {
        let weights = [220.0, 560.0, 520.0];
        let w = pane_widths(1300.0, weights, [true, false, false]);
        assert_close(w[0], collapsed_width(0));
        assert_close(sum(w), 1300.0);
        assert!((w[1] / w[2] - 560.0 / 520.0).abs() < EPS_RATIO, "{w:?}");
        assert!(
            w[1] > 560.0 && w[2] > 520.0,
            "the open panes absorb the freed space: {w:?}"
        );
    }

    #[test]
    fn strip_toggle_is_centred_on_every_visible_card() {
        let btn = 18.0;
        for index in 0..3 {
            let (inset_l, inset_r) = pane_insets(index);
            let (pad_l, pad_r) = strip_toggle_padding(inset_l, inset_r, btn);
            let content_w = strip_slot_width(index) - pad_l - pad_r;
            assert_close(content_w, btn);
            let content_center = pad_l + content_w * 0.5;
            let card_center = inset_l + ColumnLayout::STRIP_W * 0.5;
            assert_close(content_center, card_center);
        }
    }

    #[test]
    fn frame_layout_snapshot_requests_one_frame_per_change() {
        let mut layout = ColumnLayout::default();
        assert!(!layout.observe_frame_layout());

        let first = Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(100.0, 100.0));
        layout.frame_pane_rects = [first; 3];
        assert!(layout.observe_frame_layout());
        assert!(
            !layout.observe_frame_layout(),
            "stable frames need no extra frame"
        );

        layout.frame_pane_rects[1].max.x += 1.0;
        assert!(layout.observe_frame_layout());
        assert!(!layout.observe_frame_layout());
    }

    #[test]
    fn strip_toggle_leaf_keeps_its_slot_box() {
        // Taffy treats the explicit `size` as the border box. This pins that
        // behaviour for the full-width strip leaf: if padding were added on top
        // of the 100% width, the leaf would overflow the slot and push the
        // toggle off the card.
        let btn = 18.0;
        for index in 0..3 {
            let (inset_l, inset_r) = pane_insets(index);
            let slot = strip_slot_width(index);
            let mut tree: TaffyTree<()> = TaffyTree::new();
            tree.disable_rounding();
            let child = tree
                .new_leaf(strip_toggle_style(
                    inset_l,
                    inset_r,
                    btn,
                    CardMetrics::NORMAL,
                ))
                .expect("valid strip leaf");
            let root = tree
                .new_with_children(
                    Style {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: Some(AlignItems::Stretch),
                        size: Size {
                            width: Dimension::length(slot),
                            height: Dimension::length(300.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .expect("valid strip root");
            tree.compute_layout(
                root,
                Size {
                    width: AvailableSpace::Definite(slot),
                    height: AvailableSpace::Definite(300.0),
                },
            )
            .expect("strip layout");
            let layout = tree.layout(child).expect("strip child layout");
            assert_close(layout.size.width, slot);
            assert_close(layout.size.height, PANE_TOP_GAP + ColumnLayout::HEADER_H);
        }
    }

    #[test]
    fn edge_strip_slots_leave_a_full_width_card_and_outer_gap() {
        // Each outer strip slot is the visible 32pt card plus the full outer
        // gap (10) and the inner seam (5); the middle strip only has two seams.
        assert_close(collapsed_width(0), 47.0);
        assert_close(collapsed_width(1), 42.0);
        assert_close(collapsed_width(2), 47.0);
        let (left_outer, left_inner) = pane_insets(0);
        assert_close(
            collapsed_width(0) - left_outer - left_inner,
            ColumnLayout::STRIP_W,
        );
        let (right_inner, right_outer) = pane_insets(2);
        assert_close(
            collapsed_width(2) - right_inner - right_outer,
            ColumnLayout::STRIP_W,
        );
    }

    #[test]
    fn middle_strip_exposes_both_resize_seams() {
        assert_eq!(
            splitter_targets([false; 3]),
            vec![
                SplitterTarget {
                    left: 0,
                    right: 1,
                    seam: 0,
                },
                SplitterTarget {
                    left: 1,
                    right: 2,
                    seam: 1,
                },
            ]
        );
        assert_eq!(
            splitter_targets([false, true, false]),
            vec![
                SplitterTarget {
                    left: 0,
                    right: 2,
                    seam: 0,
                },
                SplitterTarget {
                    left: 0,
                    right: 2,
                    seam: 1,
                },
            ]
        );
        assert_eq!(
            splitter_targets([true, false, false]),
            vec![SplitterTarget {
                left: 1,
                right: 2,
                seam: 1,
            }]
        );
        assert!(splitter_targets([true, true, false]).is_empty());
    }

    #[test]
    fn dragging_either_middle_seam_resizes_the_outer_panes_only() {
        let stripped = [false, true, false];
        let stored = [600.0, 560.0, 658.0];
        for target in splitter_targets(stripped) {
            let mut widths = [600.0, 42.0, 658.0];
            let before = widths;
            assert!(apply_splitter_delta(&mut widths, target, 40.0));
            assert_close(widths[0], before[0] + 40.0);
            assert_close(widths[1], before[1]);
            assert_close(widths[2], before[2] - 40.0);
            assert_close(sum(widths), sum(before));

            let weights = splitter_weights(widths, stored, stripped);
            assert_close(weights[0], widths[0]);
            assert_close(weights[1], stored[1]);
            assert_close(weights[2], widths[2]);
        }
    }

    #[test]
    fn splitter_drag_is_clamped_to_both_open_minimums() {
        let target = SplitterTarget {
            left: 0,
            right: 2,
            seam: 1,
        };
        let mut widths = [200.0, 42.0, 300.0];
        assert!(apply_splitter_delta(&mut widths, target, -500.0));
        assert_close(widths[0], ColumnLayout::MIN_OPEN_W);
        assert_close(widths[2], 380.0);
        assert!(apply_splitter_delta(&mut widths, target, 1_000.0));
        assert_close(widths[2], ColumnLayout::MIN_OPEN_W);
        assert_close(widths[0], 380.0);
        assert_close(widths[1], 42.0);
        assert_close(sum(widths), 542.0);

        // Both open panes are on the floor: no delta can satisfy both minimums.
        let mut saturated = [ColumnLayout::MIN_OPEN_W, 42.0, ColumnLayout::MIN_OPEN_W];
        assert!(!apply_splitter_delta(&mut saturated, target, 10.0));
        assert!(!apply_splitter_delta(&mut saturated, target, -10.0));
    }

    #[test]
    fn collapsed_middle_pane_splits_the_sides_proportionally() {
        let weights = [220.0, 560.0, 520.0];
        let w = pane_widths(1300.0, weights, [false, true, false]);
        assert_close(w[1], collapsed_width(1));
        assert_close(sum(w), 1300.0);
        assert!((w[0] / w[2] - 220.0 / 520.0).abs() < EPS_RATIO, "{w:?}");
    }

    #[test]
    fn expand_restores_the_pre_collapse_arrangement() {
        let weights = [220.0, 560.0, 520.0];
        let mut layout = ColumnLayout::new(weights, [false; 3]);
        let before = pane_widths(1300.0, layout.widths, layout.effective_collapsed());

        layout.apply_toggle(0, true);
        let collapsed = pane_widths(1300.0, layout.widths, layout.effective_collapsed());
        assert_close(collapsed[0], collapsed_width(0));

        layout.apply_toggle(0, false);
        let after = pane_widths(1300.0, layout.widths, layout.effective_collapsed());
        for i in 0..3 {
            assert_close(after[i], before[i]);
        }
    }

    #[test]
    fn toggling_never_changes_the_weights() {
        let weights = [220.0, 560.0, 520.0];
        let mut layout = ColumnLayout::new(weights, [false; 3]);
        layout.apply_toggle(1, true);
        assert_eq!(layout.widths, weights);
        layout.apply_toggle(1, false);
        assert_eq!(layout.widths, weights);
    }

    #[test]
    fn minimum_floor_raises_a_small_pane_and_redistributes_the_deficit() {
        // Tree's proportional share (50 of 1000) is below MIN_OPEN_W, and the open space is
        // large enough for every open pane to reach the minimum. The CSS freeze iteration
        // (taffy) is numerically identical to the former solver: 120 / 463.16 / 416.84.
        let w = pane_widths(1000.0, [50.0, 500.0, 450.0], [false; 3]);
        assert_close(w[0], ColumnLayout::MIN_OPEN_W);
        assert_close(sum(w), 1000.0);
        assert!(
            w[1] > ColumnLayout::MIN_OPEN_W && w[2] > ColumnLayout::MIN_OPEN_W,
            "{w:?}"
        );
        assert!(w[1] > w[2], "the weights still order the panes: {w:?}");
        assert_close(w[1], 463.16);
        assert_close(w[2], 416.84);
    }

    #[test]
    fn floor_is_skipped_when_the_open_space_is_too_small() {
        // 300 < 3 * MIN_OPEN_W: the shares are scaled down instead of floored.
        let w = pane_widths(300.0, [220.0, 560.0, 520.0], [false; 3]);
        assert_close(sum(w), 300.0);
        assert_close(w[0], 220.0 / 1300.0 * 300.0);
        assert_close(w[1], 560.0 / 1300.0 * 300.0);
        assert_close(w[2], 520.0 / 1300.0 * 300.0);
        assert!(w[0] < ColumnLayout::MIN_OPEN_W, "{w:?}");
    }

    #[test]
    fn equal_split_when_all_open_weights_are_zero() {
        let w = pane_widths(900.0, [0.0; 3], [false; 3]);
        for v in w {
            assert_close(v, 300.0);
        }
    }

    #[test]
    fn two_collapsed_still_fills_with_a_wide_last_pane() {
        let w = pane_widths(900.0, [220.0, 560.0, 520.0], [true, true, false]);
        assert_close(w[0], collapsed_width(0));
        assert_close(w[1], collapsed_width(1));
        assert_close(w[2], 900.0 - collapsed_width(0) - collapsed_width(1));
        assert_close(sum(w), 900.0);
    }

    #[test]
    fn tiny_available_width_still_fills_the_rect() {
        // Smaller than the sum of all minimums: the only way to fill exactly is to scale down.
        let w = pane_widths(50.0, [220.0, 560.0, 520.0], [false; 3]);
        assert_close(sum(w), 50.0);
        for v in w {
            assert!(v.is_finite() && v >= 0.0, "{w:?}");
        }
    }

    #[test]
    fn tiny_available_width_never_overflows_with_a_strip() {
        // 100 < strip slot + 2 * MIN_OPEN_W: with the floor skipped every pane
        // still fills exactly from the proportional shares.
        let w = pane_widths(100.0, [220.0, 560.0, 520.0], [false, true, false]);
        assert_close(sum(w), 100.0);
        assert_close(w[1], collapsed_width(1));
        assert!(w[0].is_finite() && w[2].is_finite(), "{w:?}");
    }

    #[test]
    fn all_stripped_fills_equally_without_panicking() {
        // All three popped out: every strip shares the width equally (former fill behaviour).
        // `add_panes` never reaches this state with zero *open* panes through the UI, but the
        // pure policy stays defined.
        let w = pane_widths(300.0, [220.0, 560.0, 520.0], [true; 3]);
        assert_close(sum(w), 300.0);
        for v in w {
            assert_close(v, 100.0);
        }
    }

    #[test]
    fn zero_available_width_yields_zeros() {
        assert_eq!(
            pane_widths(0.0, [220.0, 560.0, 520.0], [false; 3]),
            [0.0; 3]
        );
        assert_eq!(
            pane_widths(f32::NAN, [220.0, 560.0, 520.0], [false; 3]),
            [0.0; 3]
        );
    }

    #[test]
    fn can_collapse_requires_more_than_one_open_pane() {
        let mut layout = ColumnLayout::new([200.0; 3], [false; 3]);
        assert!(layout.can_collapse());
        layout.collapsed[0] = true;
        assert!(layout.can_collapse());
        layout.collapsed[2] = true;
        assert!(!layout.can_collapse());
        assert_eq!(layout.open_count(), 1);
    }

    #[test]
    fn collapse_is_rejected_while_one_pane_is_open() {
        let mut layout = ColumnLayout::new([200.0; 3], [true, true, false]);
        layout.apply_toggle(2, true);
        assert_eq!(layout.collapsed, [true, true, false]);
        assert_eq!(layout.widths, [200.0; 3]);
    }

    #[test]
    fn pane_title_keys_all_resolve() {
        for key in PANE_TITLES {
            assert_ne!(&*t(key), key, "unresolved i18n key: {key}");
        }
    }

    #[test]
    fn pop_out_strips_the_pane_and_preserves_the_other_ratios() {
        let weights = [220.0, 560.0, 520.0];
        let mut layout = ColumnLayout::new(weights, [false; 3]);
        layout.apply_pop_toggle(2, true);

        assert!(layout.popped[2], "the pane is marked popped");
        assert!(!layout.collapsed[2], "popping out never collapses");
        assert!(layout.is_stripped(2));
        assert_eq!(layout.effective_collapsed(), [false, false, true]);

        let w = pane_widths(1300.0, layout.widths, layout.effective_collapsed());
        assert_close(w[2], collapsed_width(2));
        assert!((w[0] / w[1] - 220.0 / 560.0).abs() < EPS_RATIO, "{w:?}");
        assert_close(sum(w), 1300.0);
    }

    #[test]
    fn pop_out_is_refused_while_the_pane_is_collapsed() {
        let mut layout = ColumnLayout::new([220.0, 560.0, 520.0], [true, false, false]);
        layout.apply_pop_toggle(0, true);
        assert!(!layout.popped[0], "a collapsed pane cannot pop out");
    }

    #[test]
    fn dock_returns_a_popped_pane_to_its_open_width() {
        let weights = [220.0, 560.0, 520.0];
        let mut layout = ColumnLayout::new(weights, [false; 3]);
        let before = pane_widths(1300.0, layout.widths, layout.effective_collapsed());

        layout.apply_pop_toggle(1, true);
        layout.apply_pop_toggle(1, false);
        assert!(!layout.popped[1], "docking clears the popped flag");

        let after = pane_widths(1300.0, layout.widths, layout.effective_collapsed());
        for i in 0..3 {
            assert_close(after[i], before[i]);
        }
    }

    #[test]
    fn toggling_pop_never_changes_the_weights() {
        let weights = [220.0, 560.0, 520.0];
        let mut layout = ColumnLayout::new(weights, [false; 3]);
        layout.apply_pop_toggle(0, true);
        assert_eq!(layout.widths, weights);
        layout.apply_pop_toggle(0, false);
        assert_eq!(layout.widths, weights);
    }

    #[test]
    fn popped_panes_still_count_as_open_for_collapse() {
        // A popped pane is visible (it lives in its own window), so like an open
        // pane it keeps another pane collapsible; only a collapsed pane is closed.
        let mut layout = ColumnLayout::new([200.0; 3], [false; 3]);
        layout.apply_pop_toggle(2, true);
        assert_eq!(layout.open_count(), 3);
        assert!(layout.can_collapse());

        layout.apply_toggle(0, true);
        assert_eq!(layout.open_count(), 2, "the popped pane counts as open");
        assert!(layout.can_collapse());
    }
}
