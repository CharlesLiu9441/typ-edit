//! Left-pane 大纲 (outline) view.
//!
//! The outline renders the active file's lexical symbol tree
//! ([`ParseResult::symbols`](typ_edit_model::ParseResult)) behind a live
//! name filter. It is a pure view over state the parse worker already fills:
//! no engine or analysis work happens here. Every row is clickable and jumps
//! the caret to its symbol; the symbol containing the caret is highlighted, so
//! the outline follows the editor as the user moves.
//!
//! The recursive row walk lives in the free [`show_rows`] function so it can be
//! exercised headlessly; [`OutlineState`] is session-only and never persisted.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use typ_edit_i18n::t;
use typ_edit_model::{SymbolKind, SymbolNode};
use typ_edit_widgets::list::toggle_chip;
use typ_edit_widgets::text_input::inline_input;
use typ_edit_widgets::tui::{self, Tui, TuiBuilderLogic};

/// Stable id of the filter input, so focus and editing state survive across frames.
const OUTLINE_FILTER_ID: &str = "typ-edit-outline-filter";
/// Stable id of the outline scroll area, kept distinct from the tree's and the
/// search results' so switching views never carries a scroll offset across.
const OUTLINE_SCROLL_ID: &str = "typ-edit-outline";
/// Fixed row height, in points.
const OUTLINE_ROW_H: f32 = 22.0;
/// Horizontal indent per nesting level, in points.
const OUTLINE_INDENT: f32 = 12.0;
/// Width of the reserved expand/collapse chevron cell, in points.
const OUTLINE_CHEVRON_W: f32 = 14.0;
/// Width of the reserved kind-glyph cell, in points.
const OUTLINE_GLYPH_W: f32 = 22.0;
/// Gap between the kind-filter chips, in points.
const OUTLINE_CHIP_GAP: f32 = 4.0;
/// i18n keys of the kind-filter chips, in the same order as the [`OutlineKinds`]
/// fields (`heading`, `function`, `variable`, `parameter`, `label`).
const OUTLINE_KIND_KEYS: [&str; 5] = [
    "outline.kind.heading",
    "outline.kind.function",
    "outline.kind.variable",
    "outline.kind.parameter",
    "outline.kind.label",
];

/// Session-only outline state: the live filter, the kind-filter toggles, the
/// one-shot focus request set when the view is switched in, and the
/// collapsed-node set keyed by the node's `range.start` (stable across
/// re-parses of the same document).
#[derive(Default)]
pub(super) struct OutlineState {
    pub(super) filter: String,
    pub(super) focus_pending: bool,
    pub(super) kinds: OutlineKinds,
    /// `range.start` of every collapsed node.
    pub(super) collapsed: HashSet<usize>,
    /// Last filtered tree and the inputs that produced it.
    filter_cache: Option<OutlineFilterCache>,
}

/// Filtered-tree cache keyed by the parse generation, the symbol vector's
/// identity/length, the text filter and the kind toggles.
struct OutlineFilterCache {
    generation: u64,
    symbols_ptr: usize,
    symbols_len: usize,
    filter: String,
    kinds: OutlineKinds,
    tree: Arc<Vec<SymbolNode>>,
}

/// Which symbol kinds the outline displays. Every kind is on by default and the
/// choice is session-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OutlineKinds {
    pub(super) heading: bool,
    pub(super) function: bool,
    pub(super) variable: bool,
    pub(super) parameter: bool,
    pub(super) label: bool,
}

impl Default for OutlineKinds {
    fn default() -> Self {
        Self {
            heading: true,
            function: true,
            variable: true,
            parameter: true,
            label: true,
        }
    }
}

impl OutlineKinds {
    /// Whether `kind` passes the kind filter.
    fn enabled(self, kind: SymbolKind) -> bool {
        match kind {
            SymbolKind::Heading(_) => self.heading,
            SymbolKind::Function => self.function,
            SymbolKind::Variable => self.variable,
            SymbolKind::Parameter => self.parameter,
            SymbolKind::Label => self.label,
        }
    }
}

/// Case-insensitive substring match. An empty `filter` matches everything; a
/// multibyte `name`/`filter` is compared by Unicode lowercase, never by bytes.
pub(super) fn matches(name: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&filter.to_lowercase())
}

/// Clones the tree keeping only branches relevant to `query` and `kinds`.
///
/// The kind filter runs first, then the text filter. A disabled leaf kind
/// (function / variable / parameter / label) is removed; because parameters
/// belong to their function, a disabled function hides its whole subtree. A
/// disabled heading must NOT hide its section: its already-filtered children are
/// hoisted into its parent's list, recursively, so the key symbols beneath a
/// hidden heading stay visible at the nearest visible level.
///
/// The text filter then keeps a node that matches (with its whole subtree) or
/// that has a matching descendant; source order is preserved. An empty `query`
/// keeps every kind-surviving node.
pub(super) fn filter_tree(
    symbols: &[SymbolNode],
    query: &str,
    kinds: &OutlineKinds,
) -> Vec<SymbolNode> {
    let filtered = filter_kinds(symbols, kinds);
    if query.is_empty() {
        return filtered;
    }
    filtered
        .iter()
        .filter_map(|node| filter_node(node, query))
        .collect()
}

/// Applies `kinds` to `nodes`, splicing in the children of any disabled heading.
fn filter_kinds(nodes: &[SymbolNode], kinds: &OutlineKinds) -> Vec<SymbolNode> {
    let mut kept = Vec::new();
    for node in nodes {
        kept.extend(filter_kinds_node(node, kinds));
    }
    kept
}

/// The nodes that replace `node` in its parent's list once `kinds` is applied.
fn filter_kinds_node(node: &SymbolNode, kinds: &OutlineKinds) -> Vec<SymbolNode> {
    let children = filter_kinds(&node.children, kinds);
    if !kinds.enabled(node.kind) {
        // A disabled heading keeps its section by hoisting its children into its
        // place; any other disabled kind removes the node and its subtree.
        if matches!(node.kind, SymbolKind::Heading(_)) {
            return children;
        }
        return Vec::new();
    }
    vec![SymbolNode {
        name: node.name.clone(),
        kind: node.kind,
        range: node.range.clone(),
        children,
    }]
}

/// Recursive half of [`filter_tree`]: `Some` when `node` or a descendant matches.
fn filter_node(node: &SymbolNode, filter: &str) -> Option<SymbolNode> {
    if matches(&node.name, filter) {
        return Some(node.clone());
    }
    let children: Vec<SymbolNode> = node
        .children
        .iter()
        .filter_map(|child| filter_node(child, filter))
        .collect();
    if children.is_empty() {
        return None;
    }
    Some(SymbolNode {
        name: node.name.clone(),
        kind: node.kind,
        range: node.range.clone(),
        children,
    })
}

/// The short glyph shown ahead of a symbol's name.
pub(super) fn kind_glyph(kind: SymbolKind) -> String {
    match kind {
        SymbolKind::Heading(depth) => format!("H{depth}"),
        SymbolKind::Function => "ƒ".to_owned(),
        SymbolKind::Variable => "•".to_owned(),
        SymbolKind::Parameter => "P".to_owned(),
        SymbolKind::Label => "#".to_owned(),
    }
}

/// The deepest symbol whose byte range contains `byte`.
///
/// Ranges are half-open (`start..end`): `byte == start` is inside, `byte == end`
/// is outside. `None` when no symbol covers `byte` (or the tree is empty).
pub(super) fn innermost_at(symbols: &[SymbolNode], byte: usize) -> Option<&SymbolNode> {
    for node in symbols {
        if !(node.range.start.0 <= byte && byte < node.range.end.0) {
            continue;
        }
        return innermost_at(&node.children, byte).or(Some(node));
    }
    None
}

impl OutlineState {
    /// Requests one-shot focus of the filter input (set when the view is
    /// switched in).
    pub(super) fn request_focus(&mut self) {
        self.focus_pending = true;
    }

    /// The filtered symbol tree for the current filter/kinds and parse input.
    /// Reuses the previous tree when every key component is unchanged.
    fn filtered(&mut self, symbols: &[SymbolNode], generation: u64) -> Arc<Vec<SymbolNode>> {
        let symbols_ptr = symbols.as_ptr() as usize;
        let symbols_len = symbols.len();
        let fresh = self.filter_cache.as_ref().is_some_and(|cache| {
            cache.generation == generation
                && cache.symbols_ptr == symbols_ptr
                && cache.symbols_len == symbols_len
                && cache.filter == self.filter
                && cache.kinds == self.kinds
        });
        if !fresh {
            self.filter_cache = Some(OutlineFilterCache {
                generation,
                symbols_ptr,
                symbols_len,
                filter: self.filter.clone(),
                kinds: self.kinds,
                tree: Arc::new(filter_tree(symbols, &self.filter, &self.kinds)),
            });
        }
        Arc::clone(
            &self
                .filter_cache
                .as_ref()
                .expect("filter cache was just filled")
                .tree,
        )
    }

    /// The outline body: the filter input and the active file's symbol tree.
    ///
    /// Clicking a row (never its chevron) returns that symbol's byte range for
    /// the caller to select and focus the editor. The parse tree arrives as
    /// `symbols`; an empty tree (plain text, or before the first parse) shows a
    /// weak hint. `cursor_byte` drives the follow highlight.
    pub(super) fn view(
        &mut self,
        tree: &mut Tui,
        symbols: &[SymbolNode],
        generation: u64,
        cursor_byte: usize,
    ) -> Option<Range<usize>> {
        let id = egui::Id::new("typ_edit_outline_view");
        tree.id(id.with("filter")).ui(|ui| {
            let input_w = (ui.available_width() - 4.0).max(40.0);
            let query = inline_input(
                ui,
                &mut self.filter,
                &t("outline.filter_placeholder"),
                input_w,
                Some(egui::Id::new(OUTLINE_FILTER_ID)),
            );
            if self.focus_pending {
                self.focus_pending = false;
                query.request_focus();
            }
        });

        kind_chips(tree, &mut self.kinds);
        tree.style(tui::fixed(2.0))
            .id(id.with("chips_gap"))
            .add_empty();

        if symbols.is_empty() {
            hint_leaf(tree, id.with("empty"), t("outline.empty"));
            return None;
        }

        let filtered = self.filtered(symbols, generation);
        if filtered.is_empty() {
            hint_leaf(tree, id.with("filtered_empty"), t("outline.empty"));
            return None;
        }

        // The follow highlight is derived from the unfiltered tree once per
        // frame, so filtering never moves it; a filtered-out node simply paints
        // no band.
        let follow = innermost_at(symbols, cursor_byte).map(|node| node.range.start.0);

        let collapsed = &mut self.collapsed;
        tree.style(tui::flexible()).id(id.with("rows")).ui(|ui| {
            // tui-whitelist: ScrollArea 内的虚拟化行,不可嵌套 tui
            egui::ScrollArea::vertical()
                .id_salt(OUTLINE_SCROLL_ID)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    show_rows(ui, filtered.as_slice(), collapsed, follow, 0)
                })
                .inner
        })
    }
}

/// One wrapped hint label (outline empty / no match).
fn hint_leaf(tree: &mut Tui, id: egui::Id, text: impl Into<String>) {
    tree.id(id).ui(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(text.into()).color(
                    typ_edit_widgets::theme::Theme::from_ui(ui)
                        .colors
                        .text_muted,
                ),
            )
            .wrap(),
        );
    });
}

/// Draws the kind-filter chips below the filter input, as a wrapping tui row.
fn kind_chips(tree: &mut Tui, kinds: &mut OutlineKinds) {
    let id = egui::Id::new("typ_edit_outline_chips");
    tree.style(tui::row_wrap(OUTLINE_CHIP_GAP))
        .id(id)
        .add(|tree| {
            tree.id(id.with("heading")).ui(|ui| {
                chip(ui, &mut kinds.heading, OUTLINE_KIND_KEYS[0]);
            });
            tree.id(id.with("function")).ui(|ui| {
                chip(ui, &mut kinds.function, OUTLINE_KIND_KEYS[1]);
            });
            tree.id(id.with("variable")).ui(|ui| {
                chip(ui, &mut kinds.variable, OUTLINE_KIND_KEYS[2]);
            });
            tree.id(id.with("parameter")).ui(|ui| {
                chip(ui, &mut kinds.parameter, OUTLINE_KIND_KEYS[3]);
            });
            tree.id(id.with("label")).ui(|ui| {
                chip(ui, &mut kinds.label, OUTLINE_KIND_KEYS[4]);
            });
        });
}

/// One toggle chip bound to `selected`; clicking flips it.
fn chip(ui: &mut egui::Ui, selected: &mut bool, label_key: &str) {
    if toggle_chip(ui, *selected, &t(label_key)).clicked() {
        *selected = !*selected;
    }
}

/// Draws the rows of `nodes` at `depth` and returns the byte range of the row
/// that was clicked, if any.
///
/// Children of a collapsed node are skipped. The chevron click toggles collapse
/// and never jumps; the row click jumps. Stable ids are keyed by each node's
/// `range.start`.
fn show_rows(
    ui: &mut egui::Ui,
    nodes: &[SymbolNode],
    collapsed: &mut HashSet<usize>,
    follow_start: Option<usize>,
    depth: usize,
) -> Option<Range<usize>> {
    let mut clicked = None;
    for node in nodes {
        let key = node.range.start.0;
        ui.push_id(("outline", key), |ui| {
            let has_children = !node.children.is_empty();
            let is_collapsed = collapsed.contains(&key);
            let row = outline_row(
                ui,
                node,
                depth,
                has_children,
                is_collapsed,
                follow_start == Some(key),
            );
            if row.chevron_clicked {
                if is_collapsed {
                    collapsed.remove(&key);
                } else {
                    collapsed.insert(key);
                }
            } else if row.row_clicked {
                clicked = Some(node.range.start.0..node.range.end.0);
            }
            if has_children
                && !is_collapsed
                && let Some(range) =
                    show_rows(ui, &node.children, collapsed, follow_start, depth + 1)
            {
                clicked = Some(range);
            }
        });
    }
    clicked
}

/// What a single outline row's click did.
struct RowClick {
    /// The row body (not the chevron) was clicked.
    row_clicked: bool,
    /// The expand/collapse chevron was clicked.
    chevron_clicked: bool,
}

/// Draws one full-width outline row: indent, optional chevron, kind glyph, name.
///
/// The whole row senses clicks; the chevron is a visually reserved sub-cell that
/// senses its own click so the two actions never conflict. `follow` paints the
/// subtle "the caret is inside this symbol" band.
fn outline_row(
    ui: &mut egui::Ui,
    node: &SymbolNode,
    depth: usize,
    has_children: bool,
    collapsed: bool,
    follow: bool,
) -> RowClick {
    let width = ui.available_width().max(0.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, OUTLINE_ROW_H), egui::Sense::click());

    let visuals = ui.visuals();
    if follow {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(2),
            egui::Stroke::new(1.0, visuals.hyperlink_color),
            egui::StrokeKind::Inside,
        );
    }
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(2),
            visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let mut x = rect.left() + depth as f32 * OUTLINE_INDENT;
    let chevron_clicked = if has_children {
        let chevron_rect = egui::Rect::from_min_size(
            egui::pos2(x, rect.top()),
            egui::vec2(OUTLINE_CHEVRON_W, rect.height()),
        );
        let chevron = ui.interact(
            chevron_rect,
            egui::Id::new(("outline-chevron", node.range.start)),
            egui::Sense::click(),
        );
        typ_edit_widgets::icons::chevron(
            ui,
            chevron_rect,
            !collapsed,
            typ_edit_widgets::theme::Theme::from_ui(ui)
                .colors
                .text_muted,
        );
        if chevron.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        chevron.clicked()
    } else {
        false
    };
    x += OUTLINE_CHEVRON_W;

    let painter = ui.painter().with_clip_rect(rect);
    let glyph = kind_glyph(node.kind);
    painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &glyph,
        egui::TextStyle::Small.resolve(ui.style()),
        typ_edit_widgets::theme::Theme::from_ui(ui)
            .colors
            .text_muted,
    );
    x += OUTLINE_GLYPH_W;

    let name_color = if matches!(node.kind, SymbolKind::Heading(_)) {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &node.name,
        egui::TextStyle::Body.resolve(ui.style()),
        name_color,
    );

    RowClick {
        row_clicked: response.clicked(),
        chevron_clicked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        name: &str,
        kind: SymbolKind,
        range: Range<usize>,
        children: Vec<SymbolNode>,
    ) -> SymbolNode {
        SymbolNode {
            name: name.to_owned(),
            kind,
            range: typ_edit_model::BytePos(range.start)..typ_edit_model::BytePos(range.end),
            children,
        }
    }

    /// The single symbol named `name`, panicking otherwise.
    #[track_caller]
    fn named<'a>(nodes: &'a [SymbolNode], name: &str) -> &'a SymbolNode {
        nodes
            .iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name:?} in {nodes:?}"))
    }

    /// A document with a heading, a nested variable and function (that owns a
    /// parameter), and a label sibling, used across the filter/tree tests.
    fn sample_tree() -> Vec<SymbolNode> {
        vec![
            node(
                "Introduction",
                SymbolKind::Heading(1),
                0..10,
                vec![node("greeting", SymbolKind::Variable, 2..5, vec![])],
            ),
            node(
                "Setup",
                SymbolKind::Heading(2),
                20..40,
                vec![
                    node(
                        "build",
                        SymbolKind::Function,
                        22..30,
                        vec![node("arg", SymbolKind::Parameter, 23..24, vec![])],
                    ),
                    node("config", SymbolKind::Variable, 31..38, vec![]),
                ],
            ),
            node("intro-label", SymbolKind::Label, 50..60, vec![]),
        ]
    }

    /// Nested headings (`H1 > H2`) around a single variable leaf, for hoisting.
    fn nested_tree() -> Vec<SymbolNode> {
        vec![node(
            "Top",
            SymbolKind::Heading(1),
            0..100,
            vec![node(
                "Sec",
                SymbolKind::Heading(2),
                10..50,
                vec![node("deep", SymbolKind::Variable, 20..25, vec![])],
            )],
        )]
    }

    #[test]
    fn filtered_tree_cache_reuses_and_invalidates() {
        let symbols = sample_tree();
        let mut state = OutlineState::default();
        let first = state.filtered(&symbols, 1);
        let second = state.filtered(&symbols, 1);
        assert!(
            Arc::ptr_eq(&first, &second),
            "the same inputs reuse the tree"
        );
        assert_eq!(
            *first,
            filter_tree(&symbols, "", &state.kinds),
            "the cache stores the same filtered tree"
        );

        state.filter = "setup".to_owned();
        let third = state.filtered(&symbols, 1);
        assert!(
            !Arc::ptr_eq(&first, &third),
            "a filter change recomputes the tree"
        );

        let fourth = state.filtered(&symbols, 2);
        assert!(
            !Arc::ptr_eq(&third, &fourth),
            "a generation change recomputes the tree"
        );
    }

    #[test]
    fn matches_is_case_insensitive_and_empty_matches_all() {
        assert!(matches("Introduction", "intro"));
        assert!(matches("Introduction", "INTRO"));
        assert!(matches("Introduction", ""));
        assert!(!matches("Setup", "intro"));
    }

    #[test]
    fn matches_handles_multibyte_names() {
        assert!(matches("中文章节", "文章"));
        assert!(matches("Überschrift", "übers"));
        assert!(!matches("章节", "setup"));
    }

    #[test]
    fn filter_tree_keeps_matching_node_with_its_whole_subtree() {
        let filtered = filter_tree(&sample_tree(), "setup", &OutlineKinds::default());
        assert_eq!(filtered.len(), 1, "only the matching branch survives");
        assert_eq!(filtered[0].name, "Setup");
        assert_eq!(
            filtered[0].children.len(),
            2,
            "a matching node keeps its entire subtree"
        );
    }

    #[test]
    fn filter_tree_keeps_ancestors_of_matches_and_prunes_the_rest() {
        let filtered = filter_tree(&sample_tree(), "build", &OutlineKinds::default());
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].name, "Setup",
            "the ancestor is kept for context"
        );
        assert_eq!(filtered[0].children.len(), 1);
        assert_eq!(filtered[0].children[0].name, "build");
    }

    #[test]
    fn filter_tree_preserves_source_order() {
        let filtered = filter_tree(&sample_tree(), "i", &OutlineKinds::default());
        let names: Vec<&str> = filtered.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["Introduction", "Setup", "intro-label"]);
    }

    #[test]
    fn filter_tree_empty_query_keeps_everything() {
        let filtered = filter_tree(&sample_tree(), "", &OutlineKinds::default());
        assert_eq!(filtered, sample_tree());
    }

    #[test]
    fn filter_tree_drops_every_branch_when_nothing_matches() {
        assert!(filter_tree(&sample_tree(), "zzz", &OutlineKinds::default()).is_empty());
        assert!(filter_tree(&[], "anything", &OutlineKinds::default()).is_empty());
    }

    #[test]
    fn outline_kinds_default_enables_every_kind() {
        let kinds = OutlineKinds::default();
        assert!(kinds.enabled(SymbolKind::Heading(1)));
        assert!(kinds.enabled(SymbolKind::Function));
        assert!(kinds.enabled(SymbolKind::Variable));
        assert!(kinds.enabled(SymbolKind::Parameter));
        assert!(kinds.enabled(SymbolKind::Label));
    }

    #[test]
    fn filter_tree_disabled_variable_removes_only_variables() {
        let kinds = OutlineKinds {
            variable: false,
            ..OutlineKinds::default()
        };
        let filtered = filter_tree(&sample_tree(), "", &kinds);
        let names: Vec<&str> = filtered.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["Introduction", "Setup", "intro-label"]);
        assert!(filtered[0].children.is_empty(), "greeting is removed");
        let setup = named(&filtered, "Setup");
        let setup_names: Vec<&str> = setup.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(setup_names, vec!["build"], "only config is removed");
    }

    #[test]
    fn filter_tree_disabled_function_hides_its_subtree() {
        let kinds = OutlineKinds {
            function: false,
            ..OutlineKinds::default()
        };
        let filtered = filter_tree(&sample_tree(), "", &kinds);
        let setup = named(&filtered, "Setup");
        let setup_names: Vec<&str> = setup.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            setup_names,
            vec!["config"],
            "the function and its parameter are hidden"
        );
    }

    #[test]
    fn filter_tree_disabled_parameter_removes_parameter_children() {
        let kinds = OutlineKinds {
            parameter: false,
            ..OutlineKinds::default()
        };
        let filtered = filter_tree(&sample_tree(), "", &kinds);
        let build = named(&named(&filtered, "Setup").children, "build");
        assert!(
            build.children.is_empty(),
            "the function stays and its parameter goes"
        );
    }

    #[test]
    fn filter_tree_disabled_label_removes_labels() {
        let kinds = OutlineKinds {
            label: false,
            ..OutlineKinds::default()
        };
        let filtered = filter_tree(&sample_tree(), "", &kinds);
        assert!(filtered.iter().all(|node| node.name != "intro-label"));
    }

    #[test]
    fn filter_tree_disabled_heading_hoists_its_section_recursively() {
        let kinds = OutlineKinds {
            heading: false,
            ..OutlineKinds::default()
        };
        let filtered = filter_tree(&nested_tree(), "", &kinds);
        let names: Vec<&str> = filtered.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["deep"],
            "both headings hoist away, the leaf stays"
        );
    }

    #[test]
    fn filter_tree_combines_kind_and_text_filter() {
        let kinds = OutlineKinds {
            heading: false,
            variable: false,
            ..OutlineKinds::default()
        };
        assert!(filter_tree(&nested_tree(), "", &kinds).is_empty());

        let filtered = filter_tree(&sample_tree(), "build", &kinds);
        let names: Vec<&str> = filtered.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["build"], "hoisted section plus text match");
    }

    #[test]
    fn kind_glyph_maps_every_kind() {
        assert_eq!(kind_glyph(SymbolKind::Heading(1)), "H1");
        assert_eq!(kind_glyph(SymbolKind::Heading(3)), "H3");
        assert_eq!(kind_glyph(SymbolKind::Function), "ƒ");
        assert_eq!(kind_glyph(SymbolKind::Variable), "•");
        assert_eq!(kind_glyph(SymbolKind::Parameter), "P");
        assert_eq!(kind_glyph(SymbolKind::Label), "#");
    }

    #[test]
    fn kind_chips_render_without_panicking_and_stay_put_without_a_press() {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 60.0),
            )),
            ..Default::default()
        };
        let mut kinds = OutlineKinds::default();
        ctx.run_ui(raw, |ui| {
            let mut style = tui::column();
            tui::fill_width(&mut style);
            tui::tui(ui, ui.id().with("outline-chips-test"))
                .reserve_width(320.0)
                .style(style)
                .show(|tree| kind_chips(tree, &mut kinds));
        })
        .drop_without_applying_deltas();
        assert_eq!(
            kinds,
            OutlineKinds::default(),
            "no pointer press leaves the chips unchanged"
        );
    }

    #[test]
    fn innermost_at_picks_the_deepest_containing_symbol() {
        let tree = sample_tree();
        assert_eq!(
            innermost_at(&tree, 3).map(|n| n.name.as_str()),
            Some("greeting")
        );
        assert_eq!(
            innermost_at(&tree, 0).map(|n| n.name.as_str()),
            Some("Introduction")
        );
        assert_eq!(
            innermost_at(&tree, 25).map(|n| n.name.as_str()),
            Some("build")
        );
    }

    #[test]
    fn innermost_at_is_half_open_and_rejects_gaps() {
        let tree = sample_tree();
        // `0..10` contains 0 (start) but not 10 (end).
        assert_eq!(
            innermost_at(&tree, 0).map(|n| n.name.as_str()),
            Some("Introduction")
        );
        assert!(
            innermost_at(&tree, 10).is_none(),
            "the end boundary is excluded"
        );
        // Bytes between top-level ranges belong to no symbol.
        assert!(innermost_at(&tree, 15).is_none());
        assert!(innermost_at(&[], 0).is_none());
    }

    #[test]
    fn outline_rows_render_without_panicking() {
        let ctx = egui::Context::default();
        let tree = sample_tree();
        let mut collapsed = HashSet::new();
        collapsed.insert(20);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300.0, 400.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            // No pointer press means no row click; the walk must still visit the
            // expanded root and every sibling, skipping Setup's collapsed children.
            let clicked = show_rows(ui, &tree, &mut collapsed, Some(3), 0);
            assert!(clicked.is_none(), "no pointer press means no click");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn outline_kind_chip_keys_all_resolve() {
        for key in OUTLINE_KIND_KEYS {
            assert_ne!(&*t(key), key, "unresolved i18n key: {key}");
        }
    }
}
