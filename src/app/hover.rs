//! Interactive rich hover popup: diagnostics and semantic info rendered in a
//! foreground area with a height cap, scrolling, Alt-expansion, copyable text
//! and pointer pinning.
//!
//! Content priority is the diagnostics overlapping the hovered byte first, then
//! the semantic hover info (which keeps the old dwell behavior). The popup stays
//! while the pointer rests inside it, so scrolling or selecting text inside it
//! never makes it flicker, and a fresh glyph with content retargets it at once.

use super::document::DocumentController;
use super::editor_pane::{HOVER_DWELL, HOVER_MOVE_TOLERANCE, hover_due};
use super::state::HoverState;
use crate::theme;
use std::path::Path;
use std::time::Instant;
use typ_edit_engine::hover::HoverInfo;
use typ_edit_i18n::{t, t_with};
use typ_edit_model::{CompletionItem, Diag, Severity};
use typ_edit_widgets::tui::{self, TuiBuilderLogic};

/// Stable id of the hover popup area.
const HOVER_POPUP_ID: &str = "typ-edit-hover-popup";
/// Stable id of the popup's scroll area.
const HOVER_SCROLL_ID: &str = "typ-edit-hover-popup-scroll";
/// Popup width in points.
const HOVER_POPUP_WIDTH: f32 = 460.0;
/// Default popup height cap in points.
const HOVER_POPUP_CAP: f32 = 240.0;
/// Fraction of the screen height the cap lifts to while Alt is held.
const HOVER_POPUP_ALT_FRACTION: f32 = 0.6;
/// Offset from the pointer to the popup's top-left, in points.
const HOVER_POPUP_OFFSET: egui::Vec2 = egui::vec2(12.0, 18.0);
/// Pointer travel (points) that hides an open popup once the pointer is outside.
const HOVER_POPUP_HIDE_MOVE: f32 = 4.0;

/// Diagnostics whose range covers `byte`, in list order.
pub(super) fn diags_at(byte: usize, diags: &[Diag]) -> Vec<&Diag> {
    diags
        .iter()
        .filter(|diag| diag.range.start.0 <= byte && byte < diag.range.end.0)
        .collect()
}

/// Whether the open popup should close: the pointer is outside it, has moved
/// past the hide threshold, and no fresh content retargets it this frame.
///
/// While Alt is held the popup is expansion-locked: movement never closes it,
/// so the reader can scroll or select without the popup flickering away. It
/// closes only after Alt is released and the pointer then moves out.
pub(super) fn should_hide(
    pinned_inside: bool,
    alt_held: bool,
    moved_out: bool,
    has_new_content: bool,
) -> bool {
    !pinned_inside && !alt_held && moved_out && !has_new_content
}

/// Height cap of the popup: a fixed default, lifted to a fraction of the
/// available screen height while Alt is held.
pub(super) fn popup_max_height(alt: bool, available: f32) -> f32 {
    let available = available.max(0.0);
    if alt {
        available * HOVER_POPUP_ALT_FRACTION
    } else {
        HOVER_POPUP_CAP.min(available)
    }
}

/// Resolved content of an open popup.
pub(super) enum HoverContent {
    /// Diagnostics overlapping the hovered byte, in list order.
    Diagnostics(Vec<Diag>),
    /// The semantic hover info for the hovered byte.
    Semantic(HoverInfo),
}

impl HoverContent {
    /// Renders the content; every label wraps and is selectable so it can be
    /// copied.
    fn ui(&self, ui: &mut egui::Ui) {
        let base = ui.id().with(("typ_edit_hover_content", self.node_salt()));
        let width = ui.available_width();
        let mut root_style = tui::column_gap(ui.spacing().item_spacing.y);
        tui::fill_width(&mut root_style);
        tui::tui(ui, base)
            .reserve_width(width)
            .style(root_style)
            .show(|tree| match self {
                HoverContent::Diagnostics(diags) => {
                    let item_gap = tree.egui_ui().spacing().item_spacing.x;
                    for (index, diag) in diags.iter().enumerate() {
                        let row_id = base.with(("diag", index));
                        tree.id(row_id).style(tui::row_gap(item_gap)).add(|row| {
                            let (label, color) = match diag.severity {
                                Severity::Error => (
                                    t("hover.severity_error").into_owned(),
                                    theme::error(row.egui_ui()),
                                ),
                                Severity::Warning => (
                                    t("hover.severity_warning").into_owned(),
                                    theme::warn(row.egui_ui()),
                                ),
                            };
                            row.id(row_id.with("severity"))
                                .style(tui::label_leaf())
                                .wrap_mode(egui::TextWrapMode::Extend)
                                .ui(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(label).color(color).strong(),
                                        )
                                        .selectable(true),
                                    );
                                });
                            row.id(row_id.with("message"))
                                .style(tui::flexible())
                                .ui(|ui| {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(&diag.message))
                                            .selectable(true)
                                            .wrap(),
                                    );
                                });
                        });
                    }
                }
                HoverContent::Semantic(info) => {
                    if let Some(signature) = &info.signature {
                        tree.id(base.with("signature")).ui(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(signature).monospace().small(),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                    }
                    if let Some(accepts) = &info.accepts {
                        tree.id(base.with("accepts")).ui(|ui| {
                            ui.add(
                                egui::Label::new(t_with(
                                    "editor.accepts",
                                    &[("accepts", accepts.as_str())],
                                ))
                                .selectable(true)
                                .wrap(),
                            );
                        });
                    }
                    if let Some(docs) = &info.docs {
                        tree.id(base.with("docs")).ui(|ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(docs).weak())
                                    .selectable(true)
                                    .wrap(),
                            );
                        });
                    }
                }
            });
    }

    /// Stable id salt of the content tree: one tree per popup content kind.
    fn node_salt(&self) -> &'static str {
        match self {
            HoverContent::Diagnostics(_) => "diagnostics",
            HoverContent::Semantic(_) => "semantic",
        }
    }
}

/// One open hover popup: the byte it was resolved from, the rect it occupied on
/// the previous frame, its resolved content, and the pointer position it last
/// opened or retargeted at.
pub(super) struct HoverPopup {
    pub(super) byte: usize,
    pub(super) rect: egui::Rect,
    pub(super) content: HoverContent,
    pub(super) last_pointer: egui::Pos2,
    /// Popup top-left from the last unheld frame. While Alt is held this is the
    /// frozen position; the popup must not follow the pointer.
    pub(super) anchor: egui::Pos2,
}

/// Top-left of the popup for one frame. While Alt is held the stored anchor is
/// authoritative (no pointer tracking); otherwise the popup follows the pointer
/// with the standard offset.
fn popup_anchor(
    alt_held: bool,
    stored: egui::Pos2,
    pointer: Option<egui::Pos2>,
    fallback: egui::Pos2,
) -> egui::Pos2 {
    if alt_held {
        stored
    } else {
        pointer.unwrap_or(fallback) + HOVER_POPUP_OFFSET
    }
}

/// Inputs for one semantic/diagnostic hover lookup.
struct HoverRequest<'a> {
    byte: usize,
    pos: egui::Pos2,
    now: Instant,
    merged_diags: &'a [Diag],
    doc: &'a DocumentController,
    path: Option<&'a Path>,
}

impl HoverState {
    /// Draws the rich hover popup for `byte` (the editor's glyph hit-test byte).
    ///
    /// Diagnostics overlapping `byte` show at once; the semantic hover info
    /// still waits out [`HOVER_DWELL`]. A new glyph with content retargets the
    /// popup immediately; otherwise it stays until the pointer leaves it and
    /// moves on, so scrolling and selecting inside it never flicker.
    ///
    /// Foreign data arrives as parameters: the open buffer's merged diagnostics,
    /// the compile controller (for the document generation and hover query) and
    /// the active buffer's path.
    pub(super) fn show_hover_popup(
        &mut self,
        ui: &egui::Ui,
        byte: Option<usize>,
        merged_diags: &[Diag],
        doc: &DocumentController,
        path: Option<&Path>,
    ) {
        let ctx = ui.ctx();
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let alt_held = ctx.input(|i| i.modifiers.alt);
        let now = Instant::now();

        // A pointer resting inside the open popup pins it: no fresh content is
        // resolved, so the glyph under the pointer cannot retarget or close it.
        let pinned_inside = pointer.is_some_and(|pos| {
            self.hover_popup
                .as_ref()
                .is_some_and(|popup| popup.rect.contains(pos))
        });

        // While Alt holds an open popup it is expansion-locked too: movement
        // must not retarget or close it, so a held Alt resolves no fresh content
        // either. Without an open popup Alt changes nothing here.
        let hold_open = alt_held && self.hover_popup.is_some();

        // Fresh content for this frame's glyph. Diagnostics show at once;
        // semantic info waits out the dwell. Off the text (or over the popup)
        // there is nothing fresh, so a pinned popup persists.
        let fresh = if pinned_inside || hold_open {
            None
        } else {
            match (byte, pointer) {
                (Some(byte), Some(pos)) => self
                    .hover_content(
                        ctx,
                        HoverRequest {
                            byte,
                            pos,
                            now,
                            merged_diags,
                            doc,
                            path,
                        },
                    )
                    .map(|content| (byte, content)),
                _ => {
                    if self.hover_popup.is_none() {
                        self.hover_anchor = None;
                        self.hover_suppressed = false;
                    }
                    None
                }
            }
        };
        let has_new_content = fresh.is_some();

        // Escape closes an open popup (or the one that just became due).
        if (self.hover_popup.is_some() || has_new_content)
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.hover_popup = None;
            self.hover_suppressed = true;
            return;
        }

        // While Alt holds the popup open, track the pointer every frame so the
        // release-time position becomes the move baseline. The move test below
        // then measures travel only from that baseline, after Alt is released.
        if alt_held && let (Some(popup), Some(pos)) = (self.hover_popup.as_mut(), pointer) {
            popup.last_pointer = pos;
        }

        // Hide only once the pointer has left the popup and moved on.
        if let Some(popup) = self.hover_popup.as_ref() {
            let moved_out = pointer.is_some_and(|pos| {
                !popup.rect.contains(pos)
                    && pos.distance(popup.last_pointer) > HOVER_POPUP_HIDE_MOVE
            });
            if should_hide(pinned_inside, alt_held, moved_out, has_new_content) {
                self.hover_popup = None;
            }
        }

        // Open or retarget on fresh content. The same byte keeps the existing
        // popup untouched, so its rect and move baseline stay stable.
        if let (Some((byte, content)), Some(pos)) = (fresh, pointer) {
            let retarget = self
                .hover_popup
                .as_ref()
                .is_none_or(|popup| popup.byte != byte);
            if retarget {
                self.hover_popup = Some(HoverPopup {
                    byte,
                    rect: egui::Rect::NOTHING,
                    content,
                    last_pointer: pos,
                    anchor: pos + HOVER_POPUP_OFFSET,
                });
            }
        }

        let Some(popup) = self.hover_popup.as_mut() else {
            return;
        };
        let anchor = popup_anchor(alt_held, popup.anchor, pointer, popup.last_pointer);
        if !alt_held {
            popup.anchor = anchor;
        }
        let cap = popup_max_height(alt_held, ctx.viewport_rect().height());
        let area = egui::Area::new(egui::Id::new(HOVER_POPUP_ID))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .constrain_to(ctx.content_rect())
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(HOVER_POPUP_WIDTH);
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt(HOVER_SCROLL_ID)
                        .max_height(cap)
                        .show(ui, |ui| {
                            ui.set_width(HOVER_POPUP_WIDTH);
                            popup.content.ui(ui);
                        });
                    if scroll.content_size.y > cap + 0.5 {
                        ui.label(egui::RichText::new(t("hover.expand_hint")).weak().small());
                    }
                });
            });
        // Remember the rendered rect for next frame's pinned-inside test.
        if let Some(popup) = self.hover_popup.as_mut() {
            popup.rect = area.response.rect;
        }
    }

    /// Resolves what to show for `byte`, or `None` when there is nothing (or the
    /// semantic hover has not dwelled yet).
    ///
    /// Diagnostics overlapping the byte win over semantic info and need no
    /// dwell; the semantic path keeps the original dwell anchor, move tolerance
    /// and Escape suppression.
    fn hover_content(
        &mut self,
        ctx: &egui::Context,
        request: HoverRequest<'_>,
    ) -> Option<HoverContent> {
        let HoverRequest {
            byte,
            pos,
            now,
            merged_diags,
            doc,
            path,
        } = request;
        let overlapping: Vec<Diag> = diags_at(byte, merged_diags).into_iter().cloned().collect();
        if !overlapping.is_empty() {
            return Some(HoverContent::Diagnostics(overlapping));
        }

        if !self
            .hover_anchor
            .is_some_and(|(anchor_byte, _, _)| anchor_byte == byte)
        {
            self.hover_anchor = Some((byte, pos, now));
            self.hover_suppressed = false;
            return None;
        }
        let (_, anchor_pos, anchor_at) = self.hover_anchor?;
        if pos.distance(anchor_pos) > HOVER_MOVE_TOLERANCE {
            self.hover_anchor = Some((byte, pos, now));
            return None;
        }
        let since = now.saturating_duration_since(anchor_at);
        if !hover_due(since, pos.distance(anchor_pos), self.hover_suppressed) {
            if !self.hover_suppressed {
                ctx.request_repaint_after(HOVER_DWELL.saturating_sub(since));
            }
            return None;
        }
        self.hover_info_at(byte, doc, path)
            .filter(|info| !info.is_empty())
            .map(HoverContent::Semantic)
    }

    /// World-based hover analysis for `byte`, memoized by compile generation.
    fn hover_info_at(
        &mut self,
        byte: usize,
        doc: &DocumentController,
        path: Option<&Path>,
    ) -> Option<HoverInfo> {
        let generation = doc.doc.as_ref()?.generation;
        if let Some((cached_byte, cached_generation, cached)) = &self.hover_cache
            && *cached_byte == byte
            && *cached_generation == generation
        {
            return cached.clone();
        }

        let info = {
            let resources = doc.doc.as_ref()?;
            let path = path?;
            resources.result.hover_info(path, byte)
        };
        self.hover_cache = Some((byte, generation, info.clone()));
        info
    }

    /// Drops the stored world-completion list (the buffer changed, or the caret
    /// moved off the byte it was requested at).
    pub(super) fn clear_world_completions(&mut self) {
        self.world_completions = None;
    }

    /// Stores a world-completion list for the caret byte it was requested at.
    pub(super) fn set_world_completions(&mut self, byte: usize, items: Vec<CompletionItem>) {
        self.world_completions = Some((byte, items));
    }

    /// Clears the tooltip's dwell anchor and suppression flag and closes any open
    /// popup, so hiding the editor pane leaves no stale hover state.
    pub(super) fn reset_hover(&mut self) {
        self.hover_anchor = None;
        self.hover_suppressed = false;
        self.hover_popup = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn diag(start: usize, end: usize, message: &str, severity: Severity) -> Diag {
        Diag {
            path: PathBuf::from("/w/a.typ"),
            range: typ_edit_model::BytePos(start)..typ_edit_model::BytePos(end),
            message: message.to_owned(),
            severity,
        }
    }

    #[test]
    fn diags_at_selects_only_covering_ranges() {
        let diags = vec![
            diag(0, 5, "a", Severity::Error),
            diag(5, 9, "b", Severity::Warning),
            diag(8, 12, "c", Severity::Error),
        ];
        assert_eq!(diags_at(0, &diags).len(), 1);
        assert_eq!(diags_at(4, &diags)[0].message, "a");
        assert_eq!(
            diags_at(5, &diags).len(),
            1,
            "the range end is excluded from the previous range"
        );
        assert_eq!(diags_at(5, &diags)[0].message, "b");
        assert_eq!(
            diags_at(8, &diags).len(),
            2,
            "overlapping ranges are both listed"
        );
        assert_eq!(diags_at(11, &diags)[0].message, "c");
        assert!(diags_at(12, &diags).is_empty());
        assert!(diags_at(0, &[]).is_empty());
    }

    #[test]
    fn alt_freezes_the_popup_at_its_previous_anchor() {
        let stored = egui::pos2(120.0, 80.0);
        let fallback = egui::pos2(10.0, 10.0);
        assert_eq!(
            popup_anchor(true, stored, Some(egui::pos2(500.0, 500.0)), fallback),
            stored,
            "held Alt ignores pointer movement"
        );
        assert_eq!(
            popup_anchor(false, stored, Some(egui::pos2(30.0, 40.0)), fallback),
            egui::pos2(30.0, 40.0) + HOVER_POPUP_OFFSET
        );
        assert_eq!(
            popup_anchor(false, stored, None, fallback),
            fallback + HOVER_POPUP_OFFSET
        );
    }

    #[test]
    fn should_hide_requires_outside_move_and_no_fresh_content() {
        assert!(should_hide(false, false, true, false));
        assert!(
            !should_hide(true, false, true, false),
            "pinned inside stays"
        );
        assert!(
            !should_hide(false, false, false, false),
            "no move out stays"
        );
        assert!(
            !should_hide(false, false, true, true),
            "fresh content retargets"
        );
        assert!(!should_hide(true, false, false, true));
    }

    #[test]
    fn should_hide_ignores_movement_while_alt_is_held() {
        assert!(
            !should_hide(false, true, true, false),
            "Alt held ignores even a huge outside move"
        );
        assert!(
            should_hide(false, false, true, false),
            "released with outside move hides"
        );
        assert!(
            !should_hide(false, false, false, false),
            "released but not yet moved stays"
        );
    }

    #[test]
    fn popup_max_height_caps_and_lifts_for_alt() {
        assert_eq!(popup_max_height(false, 800.0), HOVER_POPUP_CAP);
        assert_eq!(popup_max_height(false, 100.0), 100.0);
        let lifted = popup_max_height(true, 800.0);
        assert!(
            (lifted - 480.0).abs() < 0.01,
            "Alt lifts to 60%: got {lifted}"
        );
        assert!(lifted > HOVER_POPUP_CAP, "Alt expands past the default cap");
        assert_eq!(popup_max_height(true, 0.0), 0.0);
        assert_eq!(popup_max_height(false, -5.0), 0.0);
    }

    #[test]
    fn hover_content_renders_without_panicking() {
        let ctx = egui::Context::default();
        let diagnostics = HoverContent::Diagnostics(vec![
            diag(0, 5, "boom", Severity::Error),
            diag(0, 5, "careful", Severity::Warning),
        ]);
        let semantic = HoverContent::Semantic(HoverInfo {
            signature: Some("f(x: int)".to_owned()),
            accepts: Some("integer".to_owned()),
            docs: Some("docs".to_owned()),
        });
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            diagnostics.ui(ui);
            semantic.ui(ui);
        })
        .drop_without_applying_deltas();
    }
}
