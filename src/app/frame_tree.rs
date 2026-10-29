//! Debug frame-tree inspector: a collapsible tree over each page's compiled
//! Typst [`Frame`](typst::layout::Frame), docked to the right of the preview
//! while enabled (the preview keeps rendering). The tree mirrors the layout
//! structure (group/text/shape/image/link/tag), shows configurable per-node
//! details, highlights the nodes under the editor cursor/selections, highlights
//! a hovered node's page rectangle in the preview and source span in the editor,
//! and jumps to a node's source on click.
//!
//! The row list is virtualized through [`egui::ScrollArea::show_rows`]: each
//! frame the flat list of currently visible rows is derived in O(visible) and
//! only the on-screen slice is painted, so a huge layout stays cheap.

use super::document::DocumentController;
use crate::settings::DebugFrameTreeOptions;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use typ_edit_engine::CompileResult;
use typ_edit_i18n::{t, t_with};
use typ_edit_widgets::button::{ButtonVariant, button};
use typ_edit_widgets::icons;
use typ_edit_widgets::list::option_row;
use typ_edit_widgets::theme::Theme;
use typst::foundations::Smart;
use typst::layout::{Abs, Frame, FrameItem, Point, Rect, Size, Transform};
use typst::visualize::{FixedStroke, ImageScaling, Paint};
use typst_layout::PagedDocument;

/// Height of one tree row, in points. Uniform: every row is forced onto a single
/// clipped line so the virtualized list stays consistent with this stride.
const ROW_H: f32 = 20.0;
/// Horizontal indent per depth level, in points.
const INDENT_W: f32 = 14.0;
/// Width of a row's reserved expand/collapse chevron cell, in points.
const CHEVRON_W: f32 = 16.0;
/// Maximum number of characters of a text node's content shown.
const TEXT_MAX: usize = 48;
/// Separator between the detail fragments of one node's cached detail line.
const DETAIL_SEP: &str = "  ·  ";

/// The subset of [`DebugFrameTreeOptions`] that changes the built arena. A
/// change here forces a rebuild; the enable toggle is deliberately absent (it
/// only decides whether the pane renders the tree at all).
#[derive(Clone, Copy, PartialEq, Eq)]
struct DisplayConfig {
    children: bool,
    text: bool,
    geometry: bool,
    style: bool,
    span: bool,
    group_nodes: bool,
    text_nodes: bool,
    shape_nodes: bool,
    image_nodes: bool,
    link_nodes: bool,
    tag_nodes: bool,
}

impl DisplayConfig {
    /// Reads the displayed-info flags from the persisted options.
    fn from_settings(options: &DebugFrameTreeOptions) -> Self {
        Self {
            children: options.show_children,
            text: options.show_text,
            geometry: options.show_geometry,
            style: options.show_style,
            span: options.show_span,
            group_nodes: options.show_group_nodes,
            text_nodes: options.show_text_nodes,
            shape_nodes: options.show_shape_nodes,
            image_nodes: options.show_image_nodes,
            link_nodes: options.show_link_nodes,
            tag_nodes: options.show_tag_nodes,
        }
    }

    /// Whether a node of `kind` is drawn. Page roots are structural and always
    /// remain; every other FrameItem variant can be filtered independently.
    fn shows(self, kind: NodeKind) -> bool {
        match kind {
            NodeKind::Page => true,
            NodeKind::Group => self.group_nodes,
            NodeKind::Text => self.text_nodes,
            NodeKind::Shape => self.shape_nodes,
            NodeKind::Image => self.image_nodes,
            NodeKind::Link => self.link_nodes,
            NodeKind::Tag => self.tag_nodes,
        }
    }
}

/// The layout kind of a tree node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NodeKind {
    Page,
    Group,
    Text,
    Shape,
    Image,
    Link,
    Tag,
}

/// One node of the flattened frame tree. Children are indices into the owning
/// arena, so the whole tree is a plain `Vec<Node>`.
///
/// `source` is ALWAYS populated when the item carries a span, independent of the
/// display config: cursor/selection matching and click-to-jump read it, so it
/// must not be coupled to the 源码范围 checkbox. Only the `span_label` fragment
/// of `detail_line` is gated by `show_span`.
struct Node {
    parent: Option<usize>,
    children: Vec<usize>,
    kind: NodeKind,
    label: String,
    /// The pre-joined detail fragments (built once, never per frame).
    detail_line: String,
    source: Option<(PathBuf, Range<usize>)>,
    /// Page index (0-based) this node was collected from.
    page: usize,
    /// The item's placement inside its containing frame.
    item_pos: Point,
    /// The item's own box, origin-relative (add `item_pos` for frame coords).
    /// `None` for items without a box (`Tag`).
    local_rect: Option<Rect>,
    /// `Some` for `Group` items: maps the group's frame into its containing frame.
    group_transform: Option<Transform>,
}

/// The flattened frame tree of one document generation plus the display config
/// it was built with. Rebuilt only when either changes.
struct FrameTreeCache {
    generation: u64,
    config: DisplayConfig,
    nodes: Vec<Node>,
    roots: Vec<usize>,
}

/// The editor-state half of the highlight key. Deliberately WITHOUT the document
/// generation: a recompile with an unmoved cursor must not re-scroll the tree.
#[derive(Clone, PartialEq, Eq)]
struct CursorKey {
    path: Option<PathBuf>,
    cursor_byte: usize,
    selection: Vec<Range<usize>>,
}

/// Computed highlight sets for one [`CursorKey`].
struct HighlightCache {
    cursor_key: CursorKey,
    cursor_nodes: Vec<usize>,
    selection_nodes: HashSet<usize>,
}

/// Everything the debug frame-tree pane caches between frames: the built arena,
/// the user's expansion set, the cursor/selection highlights, and a pending
/// scroll target consumed by the next render.
#[derive(Default)]
pub(super) struct FrameTreeState {
    cache: Option<FrameTreeCache>,
    open: HashSet<usize>,
    highlight: Option<HighlightCache>,
    pending_scroll: Option<usize>,
    pub(in crate::app) hover: Option<FrameTreeHover>,
    /// Last frame's dock decision. The first frame after a change can still
    /// carry the pre-dock child width through egui_taffy's sizing pass; asking
    /// for one more frame lets the nested Panel settle without pointer input.
    pub(in crate::app) docked: bool,
}

/// The inputs a single frame feeds into [`FrameTreeState::ensure`].
struct EnsureInputs<'a> {
    document: &'a PagedDocument,
    result: &'a CompileResult,
    config: DisplayConfig,
    workspace_root: Option<&'a Path>,
    path: Option<&'a Path>,
    cursor_byte: usize,
    selection: &'a [Range<usize>],
}

/// The cursor/selection query used to compute highlight sets.
struct HighlightQuery<'a> {
    path: Option<&'a Path>,
    cursor_byte: usize,
    selection: &'a [Range<usize>],
}

impl FrameTreeState {
    /// Rebuilds the arena when the document generation or display config
    /// changed, and recomputes the highlight sets when the arena was rebuilt or
    /// the cursor/selection changed. Ancestor auto-expansion and the one-shot
    /// scroll are armed ONLY on a genuine cursor change, never on a recompile.
    fn ensure(&mut self, inputs: EnsureInputs<'_>) {
        let rebuilt =
            cache_needs_rebuild(self.cache.as_ref(), inputs.result.generation, inputs.config);
        if rebuilt {
            self.install_cache(build_cache(&inputs));
        }
        let cursor_key = CursorKey {
            path: inputs.path.map(Path::to_path_buf),
            cursor_byte: inputs.cursor_byte,
            selection: inputs.selection.to_vec(),
        };
        let cursor_changed = self
            .highlight
            .as_ref()
            .is_none_or(|cache| cache.cursor_key != cursor_key);
        if !rebuilt && !cursor_changed {
            return;
        }
        self.recompute_highlights(cursor_key, cursor_changed);
    }

    /// Installs a freshly built arena, keeping the user's expansion state.
    ///
    /// Node indices are stable while the layout structure is, so the set is
    /// pruned rather than cleared: a wholesale clear would collapse the entire
    /// tree on every debounced edit. Minor index drift after insertions is
    /// accepted for this debug view.
    fn install_cache(&mut self, cache: FrameTreeCache) {
        let len = cache.nodes.len();
        self.open.retain(|&index| index < len);
        self.cache = Some(cache);
    }

    /// Recomputes the highlight sets against the current arena. Scroll/expansion
    /// are re-armed only when `cursor_changed`.
    fn recompute_highlights(&mut self, cursor_key: CursorKey, cursor_changed: bool) {
        let query = HighlightQuery {
            path: cursor_key.path.as_deref(),
            cursor_byte: cursor_key.cursor_byte,
            selection: &cursor_key.selection,
        };
        let (cursor_nodes, selection_nodes) = match &self.cache {
            Some(cache) => compute_highlights(&cache.nodes, &query),
            None => (Vec::new(), HashSet::new()),
        };
        if cursor_changed {
            self.expand_to(cursor_nodes.first().copied());
        }
        self.highlight = Some(HighlightCache {
            cursor_key,
            cursor_nodes,
            selection_nodes,
        });
    }

    /// Opens every ancestor of `target` (never the target itself) and arms the
    /// one-shot scroll. A `None` target clears the pending scroll.
    fn expand_to(&mut self, target: Option<usize>) {
        let Some(target) = target else {
            self.pending_scroll = None;
            return;
        };
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let mut current = cache.nodes[target].parent;
        while let Some(parent) = current {
            self.open.insert(parent);
            current = cache.nodes[parent].parent;
        }
        self.pending_scroll = Some(target);
    }
}

/// Whether the arena must be rebuilt for this generation/config pair.
fn cache_needs_rebuild(
    cache: Option<&FrameTreeCache>,
    generation: u64,
    config: DisplayConfig,
) -> bool {
    cache.is_none_or(|cache| cache.generation != generation || cache.config != config)
}

/// Builds the flattened arena from the document's page frames.
fn build_cache(inputs: &EnsureInputs<'_>) -> FrameTreeCache {
    let mut builder = TreeBuilder {
        nodes: Vec::new(),
        result: inputs.result,
        config: inputs.config,
        workspace_root: inputs.workspace_root,
    };
    let mut roots = Vec::new();
    for (index, page) in inputs.document.pages().iter().enumerate() {
        let mut details = Vec::new();
        if inputs.config.geometry {
            details.push(size_pt(page.frame.size()));
        }
        let root = builder.nodes.len();
        builder.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            kind: NodeKind::Page,
            label: t_with("debug_tree.page", &[("n", &(index + 1).to_string())]),
            detail_line: details.join(DETAIL_SEP),
            source: None,
            page: index,
            item_pos: Point::zero(),
            local_rect: Some(Rect::from_pos_size(Point::zero(), page.frame.size())),
            group_transform: None,
        });
        builder.push_items(root, &page.frame, index);
        builder.push_child_count(root);
        roots.push(root);
    }
    FrameTreeCache {
        generation: inputs.result.generation,
        config: inputs.config,
        nodes: builder.nodes,
        roots,
    }
}

/// The recursive arena builder; carries the shared inputs as fields so the
/// per-item helpers stay small.
struct TreeBuilder<'a> {
    nodes: Vec<Node>,
    result: &'a CompileResult,
    config: DisplayConfig,
    workspace_root: Option<&'a Path>,
}

impl TreeBuilder<'_> {
    /// Appends one node per frame item at `parent`, recursing into groups.
    fn push_items(&mut self, parent: usize, frame: &Frame, page: usize) {
        for (pos, item) in frame.items() {
            self.push_item(parent, *pos, item, page);
        }
    }

    /// Appends a single frame item (and any group subtree) under `parent`.
    fn push_item(&mut self, parent: usize, pos: Point, item: &FrameItem, page: usize) {
        let kind = match item {
            FrameItem::Group(_) => NodeKind::Group,
            FrameItem::Text(_) => NodeKind::Text,
            FrameItem::Shape(..) => NodeKind::Shape,
            FrameItem::Image(..) => NodeKind::Image,
            FrameItem::Link(..) => NodeKind::Link,
            FrameItem::Tag(_) => NodeKind::Tag,
        };
        let mut details = Vec::new();
        // The source is resolved for every span-carrying item, regardless of
        // `show_span`: matching and clickability depend on it.
        let source = match item {
            FrameItem::Text(text) => text
                .glyphs
                .first()
                .and_then(|glyph| self.result.span_source(glyph.span.0)),
            FrameItem::Shape(_, span) | FrameItem::Image(_, _, span) => {
                self.result.span_source(*span)
            }
            _ => None,
        };
        match item {
            FrameItem::Group(group) => {
                if self.config.geometry {
                    details.push(format_geometry(pos, group.frame.size()));
                }
            }
            FrameItem::Text(text) => {
                if self.config.text {
                    details.push(display_text(&text.text));
                }
                if self.config.geometry {
                    details.push(format!("{} · {:.1} pt", point_pt(pos), text.size.to_pt()));
                }
                if self.config.style {
                    details.push(format!("fill {}", format_paint(&text.fill)));
                }
            }
            FrameItem::Shape(shape, _) => {
                if self.config.geometry {
                    details.push(format_geometry(pos, shape.bbox(false).size()));
                }
                if self.config.style {
                    details.push(format!(
                        "fill {} · stroke {}",
                        shape
                            .fill
                            .as_ref()
                            .map_or_else(|| "none".to_owned(), format_paint),
                        format_stroke(shape.stroke.as_ref())
                    ));
                }
            }
            FrameItem::Image(image, size, _) => {
                if self.config.geometry {
                    details.push(format_geometry(pos, *size));
                }
                if self.config.style {
                    details.push(format!(
                        "format {:?} · filter {}",
                        image.format(),
                        format_scaling(image.scaling())
                    ));
                }
            }
            FrameItem::Link(_, size) => {
                if self.config.geometry {
                    details.push(format_geometry(pos, *size));
                }
            }
            FrameItem::Tag(_) => {}
        }
        if let Some(detail) = span_detail(source.as_ref(), self.config, self.workspace_root) {
            details.push(detail);
        }

        let index = self.nodes.len();
        self.nodes.push(Node {
            parent: Some(parent),
            children: Vec::new(),
            kind,
            label: node_kind_label(kind).to_owned(),
            detail_line: details.join(DETAIL_SEP),
            source,
            page,
            item_pos: pos,
            local_rect: item_local_rect(item),
            group_transform: match item {
                FrameItem::Group(group) => Some(group.transform),
                _ => None,
            },
        });
        self.nodes[parent].children.push(index);
        if let FrameItem::Group(group) = item {
            self.push_items(index, &group.frame, page);
            self.push_child_count(index);
        }
    }

    /// Appends a container's child-count fragment when enabled.
    fn push_child_count(&mut self, index: usize) {
        if !self.config.children {
            return;
        }
        let count = self.nodes[index].children.len();
        let text = t_with("debug_tree.child_count", &[("n", &count.to_string())]);
        let node = &mut self.nodes[index];
        if node.detail_line.is_empty() {
            node.detail_line = text;
        } else {
            node.detail_line.push_str(DETAIL_SEP);
            node.detail_line.push_str(&text);
        }
    }
}

/// The origin-relative box of a frame item, mirroring the engine's cursor walk
/// (`find_in_text` for text; the shape/image/link/group areas elsewhere). `None`
/// for `Tag`, which carries no box.
fn item_local_rect(item: &FrameItem) -> Option<Rect> {
    match item {
        FrameItem::Text(text) => {
            let metrics = text.font.metrics();
            let top = -metrics.ascender.at(text.size);
            let bottom = -metrics.descender.at(text.size);
            let width = text
                .glyphs
                .iter()
                .map(|glyph| glyph.x_advance.at(text.size))
                .sum();
            Some(Rect::new(
                Point::new(Abs::zero(), top),
                Point::new(width, bottom),
            ))
        }
        FrameItem::Shape(shape, _) => Some(shape.bbox(false)),
        FrameItem::Image(_, size, _) | FrameItem::Link(_, size) => {
            Some(Rect::from_pos_size(Point::zero(), *size))
        }
        FrameItem::Group(group) => Some(Rect::from_pos_size(Point::zero(), group.frame.size())),
        FrameItem::Tag(_) => None,
    }
}

/// The node's page-local box: its own box at `item_pos`, then folded up the
/// ancestor chain through every ancestor Group's transform + position
/// (mirrors the engine cursor walk). `None` when the node has no box.
fn page_rect(nodes: &[Node], index: usize) -> Option<Rect> {
    let node = nodes.get(index)?;
    let local = node.local_rect?;
    let mut rect = Rect::new(node.item_pos + local.min, node.item_pos + local.max);
    let mut current = node.parent;
    while let Some(parent) = current {
        let ancestor = &nodes[parent];
        let Some(transform) = ancestor.group_transform else {
            break;
        };
        rect = typ_edit_engine::map_frame_rect(rect, transform, ancestor.item_pos);
        current = ancestor.parent;
    }
    Some(rect)
}

/// The `path:start..end` detail for a node, or `None` when 源码范围 is off or the
/// node has no source. The `source` itself is independent of this decision.
fn span_detail(
    source: Option<&(PathBuf, Range<usize>)>,
    config: DisplayConfig,
    root: Option<&Path>,
) -> Option<String> {
    if !config.span {
        return None;
    }
    source.map(|(path, range)| span_label(path, range, root))
}

/// The display name of a node kind.
fn node_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Page => "Page",
        NodeKind::Group => "Group",
        NodeKind::Text => "Text",
        NodeKind::Shape => "Shape",
        NodeKind::Image => "Image",
        NodeKind::Link => "Link",
        NodeKind::Tag => "Tag",
    }
}

/// The highlight sets for a query: the node under each caret (primary first,
/// then every collapsed selection range) and every node intersecting a
/// non-empty selection range.
fn compute_highlights(nodes: &[Node], query: &HighlightQuery<'_>) -> (Vec<usize>, HashSet<usize>) {
    let Some(path) = query.path else {
        return (Vec::new(), HashSet::new());
    };
    let mut cursor_nodes = Vec::new();
    if let Some(node) = deepest_cursor_node(nodes, path, query.cursor_byte) {
        cursor_nodes.push(node);
    }
    for range in query.selection {
        if range.start == range.end
            && let Some(node) = deepest_cursor_node(nodes, path, range.start)
            && !cursor_nodes.contains(&node)
        {
            cursor_nodes.push(node);
        }
    }
    let selection_nodes = nodes_intersecting(nodes, path, query.selection);
    (cursor_nodes, selection_nodes)
}

/// The deepest node of `path` whose inclusive byte range contains `byte`.
///
/// Depth wins over a smaller range; ties break toward the smaller range, then
/// toward the first encountered node in arena order.
fn deepest_cursor_node(nodes: &[Node], path: &Path, byte: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_depth = 0usize;
    let mut best_len = usize::MAX;
    for (index, node) in nodes.iter().enumerate() {
        let Some((source_path, range)) = &node.source else {
            continue;
        };
        if source_path != path || range.start > byte || byte > range.end {
            continue;
        }
        let depth = node_depth(nodes, index);
        let len = range.end - range.start;
        if best.is_none() || depth > best_depth || (depth == best_depth && len < best_len) {
            best = Some(index);
            best_depth = depth;
            best_len = len;
        }
    }
    best
}

/// Every node of `path` whose range intersects one of the non-empty `ranges`.
fn nodes_intersecting(nodes: &[Node], path: &Path, ranges: &[Range<usize>]) -> HashSet<usize> {
    let mut set = HashSet::new();
    for (index, node) in nodes.iter().enumerate() {
        let Some((source_path, range)) = &node.source else {
            continue;
        };
        if source_path != path {
            continue;
        }
        let hit = ranges.iter().any(|selected| {
            selected.start < selected.end
                && range.start < selected.end
                && range.end > selected.start
        });
        if hit {
            set.insert(index);
        }
    }
    set
}

/// The number of ancestors of `index` in the arena.
fn node_depth(nodes: &[Node], index: usize) -> usize {
    let mut depth = 0;
    let mut current = nodes[index].parent;
    while let Some(parent) = current {
        depth += 1;
        current = nodes[parent].parent;
    }
    depth
}

/// A node's text content with newlines/tabs escaped and truncated to
/// [`TEXT_MAX`] characters (an ellipsis marks the cut).
fn display_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    if escaped.chars().count() > TEXT_MAX {
        let truncated: String = escaped.chars().take(TEXT_MAX).collect();
        format!("{truncated}…")
    } else {
        escaped
    }
}

/// A source path for display: relative to the workspace root when under it,
/// else just the file name, else the full path.
fn display_source_path(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .map_or_else(
            || {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            },
            |relative| relative.to_string_lossy().replace('\\', "/"),
        )
}

/// A `path:start..end` source-range detail.
fn span_label(path: &Path, range: &Range<usize>, root: Option<&Path>) -> String {
    format!(
        "{}:{}..{}",
        display_source_path(path, root),
        range.start,
        range.end
    )
}

/// A walk position in points: `(x, y) pt`.
fn point_pt(pos: Point) -> String {
    format!("({:.1}, {:.1}) pt", pos.x.to_pt(), pos.y.to_pt())
}

/// A `W × H pt` size.
fn size_pt(size: Size) -> String {
    format!("{:.1} × {:.1} pt", size.x.to_pt(), size.y.to_pt())
}

/// `(x, y) pt · W × H pt` geometry for one leaf/group.
fn format_geometry(pos: Point, size: Size) -> String {
    format!("{} · {}", point_pt(pos), size_pt(size))
}

/// A paint label: `Solid(r,g,b,a)` for a solid color, else the paint kind.
fn format_paint(paint: &Paint) -> String {
    match paint {
        Paint::Solid(color) => {
            let [r, g, b, a] = color.to_vec4_u8();
            format!("Solid({r},{g},{b},{a})")
        }
        Paint::Gradient(_) => "Gradient".to_owned(),
        Paint::Tiling(_) => "Tiling".to_owned(),
    }
}

/// A stroke label: `paint thickness` or `none`.
fn format_stroke(stroke: Option<&FixedStroke>) -> String {
    stroke.map_or_else(
        || "none".to_owned(),
        |stroke| {
            format!(
                "{} {:.1}pt",
                format_paint(&stroke.paint),
                stroke.thickness.to_pt()
            )
        },
    )
}

/// The image scaling label (`Smooth` / `Pixelated` / `Auto`).
fn format_scaling(scaling: Smart<ImageScaling>) -> &'static str {
    match scaling {
        Smart::Auto => "Auto",
        Smart::Custom(ImageScaling::Smooth) => "Smooth",
        Smart::Custom(ImageScaling::Pixelated) => "Pixelated",
    }
}

/// Stable id of the page-jump combo's remembered selection.
fn page_combo_id() -> egui::Id {
    egui::Id::new("debug_tree_page_selection")
}

/// One flat, visible tree row: a node's arena index plus its indent depth.
#[derive(Clone, Copy)]
struct Row {
    index: usize,
    depth: usize,
}

/// DFS over the page roots, descending only into open nodes. The result is the
/// flat row list the virtualized scroll area draws; O(visible).
fn flat_visible_rows(
    nodes: &[Node],
    roots: &[usize],
    open: &HashSet<usize>,
    config: DisplayConfig,
) -> Vec<Row> {
    let mut builder = RowsBuilder {
        nodes,
        open,
        config,
        rows: Vec::new(),
    };
    for &root in roots {
        builder.visit(root, 0);
    }
    builder.rows
}

/// Accumulates the flat visible rows, skipping filtered node kinds.
struct RowsBuilder<'a> {
    nodes: &'a [Node],
    open: &'a HashSet<usize>,
    config: DisplayConfig,
    rows: Vec<Row>,
}

impl RowsBuilder<'_> {
    /// Pushes `index` when its kind is visible; descends when it is open (or
    /// when it is a hidden Group, whose children are flattened into this
    /// level). Hidden groups still keep their transform in the arena, so the
    /// descendants' page rectangles stay correct.
    fn visit(&mut self, index: usize, depth: usize) {
        let node = &self.nodes[index];
        let visible = self.config.shows(node.kind);
        if visible {
            self.rows.push(Row { index, depth });
        }
        let descend = if visible {
            self.open.contains(&index)
        } else {
            node.kind == NodeKind::Group
        };
        if descend {
            let child_depth = if visible { depth + 1 } else { depth };
            for position in 0..node.children.len() {
                let child = node.children[position];
                self.visit(child, child_depth);
            }
        }
    }
}

/// What one row's click did.
struct RowClick {
    row_clicked: bool,
    chevron_clicked: bool,
    hovered: bool,
}

/// Immutable render inputs shared by every row.
struct RenderCtx<'a> {
    nodes: &'a [Node],
    open: &'a HashSet<usize>,
    cursor_nodes: &'a [usize],
    selection_nodes: &'a HashSet<usize>,
    cursor_fill: egui::Color32,
    selection_fill: egui::Color32,
}

/// The deferred effects of one render pass.
#[derive(Default)]
struct RenderOutcome {
    jump: Option<(PathBuf, usize)>,
    toggles: Vec<(usize, bool)>,
    /// The one hovered row this pass, if any.
    hovered: Option<usize>,
}

/// Paints the visible rows and collects the actions they produced.
struct TreeRenderer<'a> {
    ctx: &'a RenderCtx<'a>,
    out: RenderOutcome,
}

impl TreeRenderer<'_> {
    /// Paints one row and records its click action.
    fn render(&mut self, ui: &mut egui::Ui, row: Row) {
        let click = self.render_row(ui, row);
        if click.hovered {
            self.out.hovered = Some(row.index);
        }
        if click.chevron_clicked {
            self.out
                .toggles
                .push((row.index, !self.ctx.open.contains(&row.index)));
        } else if click.row_clicked
            && let Some((path, range)) = &self.ctx.nodes[row.index].source
        {
            self.out.jump = Some((path.clone(), range.start));
        }
    }

    /// Paints one full-width, single-line row: highlight band, chevron, label
    /// and cached detail line (clipped, never wrapped).
    fn render_row(&mut self, ui: &mut egui::Ui, row: Row) -> RowClick {
        let node = &self.ctx.nodes[row.index];
        let has_children = !node.children.is_empty();
        let clickable = node.source.is_some();
        let width = ui.available_width().max(0.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::click());

        let visuals = ui.visuals();
        if self.ctx.cursor_nodes.contains(&row.index) {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(2), self.ctx.cursor_fill);
        } else if self.ctx.selection_nodes.contains(&row.index) {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(2), self.ctx.selection_fill);
        } else if response.hovered() {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(2),
                visuals.widgets.hovered.bg_fill.gamma_multiply(0.5),
            );
        }
        if response.hovered() && (clickable || has_children) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let mut x = rect.left() + 4.0 + row.depth as f32 * INDENT_W;
        let chevron_clicked = if has_children {
            let chevron_rect = egui::Rect::from_min_size(
                egui::pos2(x, rect.top()),
                egui::vec2(CHEVRON_W, rect.height()),
            );
            let chevron = ui.interact(
                chevron_rect,
                egui::Id::new(("debug-tree-chevron", row.index)),
                egui::Sense::click(),
            );
            icons::chevron(
                ui,
                chevron_rect,
                self.ctx.open.contains(&row.index),
                visuals.weak_text_color(),
            );
            chevron.clicked()
        } else {
            false
        };
        x += CHEVRON_W;

        let painter = ui.painter().with_clip_rect(rect);
        let label_color = match node.kind {
            NodeKind::Page | NodeKind::Group => visuals.strong_text_color(),
            _ if clickable => visuals.text_color(),
            _ => visuals.weak_text_color(),
        };
        let label_rect = painter.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &node.label,
            egui::TextStyle::Body.resolve(ui.style()),
            label_color,
        );
        if !node.detail_line.is_empty() {
            painter.text(
                egui::pos2(label_rect.right() + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &node.detail_line,
                egui::TextStyle::Small.resolve(ui.style()),
                visuals.weak_text_color(),
            );
        }

        let response = match &node.source {
            Some((path, range)) => {
                response.on_hover_text(format!("{}:{}..{}", path.display(), range.start, range.end))
            }
            None => response,
        };

        RowClick {
            row_clicked: response.clicked(),
            chevron_clicked,
            hovered: response.hovered(),
        }
    }
}

/// A hovered tree node resolved for the inspector highlights.
#[derive(Clone, PartialEq)]
pub(in crate::app) struct FrameTreeHover {
    pub(in crate::app) page: usize,
    pub(in crate::app) rect: Rect,
    pub(in crate::app) source: Option<(PathBuf, Range<usize>)>,
}

/// App-only effects produced by rendering the debug frame-tree pane: a node row
/// click that asks the caller to open its source, move the caret and focus the
/// editor, plus the node the pointer currently hovers.
#[derive(Default)]
pub(in crate::app) struct FrameTreeOutcome {
    pub(in crate::app) jump: Option<(PathBuf, usize)>,
    pub(in crate::app) hover: Option<FrameTreeHover>,
}

impl FrameTreeState {
    /// The debug frame-tree inspector: a right-docked, resizable panel holding
    /// the toolbar and the virtualized row list, alongside the preview. Returns
    /// the app-only effects the caller must apply (the jump a clicked node asked
    /// for and the node the pointer currently hovers).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pane(
        &mut self,
        ui: &mut egui::Ui,
        doc: &DocumentController,
        options: &DebugFrameTreeOptions,
        workspace_root: Option<&Path>,
        path: Option<&Path>,
        selection: &[Range<usize>],
        cursor_byte: usize,
    ) -> FrameTreeOutcome {
        let Some(resources) = doc.doc.as_ref() else {
            // No document: the preview owns every empty state, so the dock is
            // not rendered and any stale hover highlight must be cleared.
            self.hover = None;
            if workspace_root.is_none() {
                centered_weak(ui, &t("preview.open_to_start"));
            } else if doc.compile_in_flight {
                centered_weak(ui, &t("preview.compiling"));
            } else {
                centered_weak(ui, &t("preview.empty"));
            }
            return FrameTreeOutcome::default();
        };
        let (document, result) = (resources.document.clone(), resources.result.clone());
        let config = DisplayConfig::from_settings(options);
        self.ensure(EnsureInputs {
            document: document.as_ref(),
            result: result.as_ref(),
            config,
            workspace_root,
            path,
            cursor_byte,
            selection,
        });

        let Some(cache) = self.cache.take() else {
            return FrameTreeOutcome::default();
        };

        let cursor_nodes = self
            .highlight
            .as_ref()
            .map(|highlight| highlight.cursor_nodes.clone())
            .unwrap_or_default();
        let selection_nodes = self
            .highlight
            .as_ref()
            .map(|highlight| highlight.selection_nodes.clone())
            .unwrap_or_default();
        let selection_color = Theme::from_ui(ui).colors.selection;
        let row_stride = ROW_H + ui.spacing().item_spacing.y;

        let mut jump_page = None;
        let mut outcome = RenderOutcome::default();
        // Docked inspector; the preview keeps the rest of the pane.
        egui::Panel::right(egui::Id::new("debug_frame_tree_dock"))
            .resizable(true)
            .default_size(320.0)
            .size_range(200.0..=640.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if button(ui, &t("debug_tree.expand_all"), ButtonVariant::Quiet, true).clicked()
                    {
                        self.open = cache
                            .nodes
                            .iter()
                            .enumerate()
                            .filter(|(_, node)| !node.children.is_empty())
                            .map(|(index, _)| index)
                            .collect();
                    }
                    if button(
                        ui,
                        &t("debug_tree.collapse_all"),
                        ButtonVariant::Quiet,
                        true,
                    )
                    .clicked()
                    {
                        self.open.clear();
                    }

                    let mut selected: Option<usize> =
                        ui.ctx().data(|data| data.get_temp(page_combo_id()));
                    if selected.is_some_and(|index| index >= cache.roots.len()) {
                        selected = None;
                    }
                    let selected_text = selected
                        .and_then(|index| cache.roots.get(index))
                        .map(|&root| cache.nodes[root].label.clone())
                        .unwrap_or_else(|| t("debug_tree.goto_page").into_owned());
                    crate::theme::combo_box_scope(ui, |ui| {
                        egui::ComboBox::from_id_salt("debug_tree_page")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for (index, &root) in cache.roots.iter().enumerate() {
                                    if option_row(
                                        ui,
                                        selected == Some(index),
                                        &cache.nodes[root].label,
                                    )
                                    .clicked()
                                    {
                                        selected = Some(index);
                                        jump_page = Some(root);
                                        ui.close();
                                    }
                                }
                            });
                    });
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(page_combo_id(), selected));
                });

                if let Some(root) = jump_page {
                    self.pending_scroll = Some(root);
                }
                let scroll_target = self.pending_scroll.take();

                let rows = flat_visible_rows(&cache.nodes, &cache.roots, &self.open, config);
                let target_row = scroll_target.and_then(|node| {
                    rows.iter()
                        .position(|row| row.index == node)
                        .map(|position| position as f32)
                });

                let viewport_h = ui.available_height();
                outcome = {
                    let ctx = RenderCtx {
                        nodes: &cache.nodes,
                        open: &self.open,
                        cursor_nodes: &cursor_nodes,
                        selection_nodes: &selection_nodes,
                        cursor_fill: selection_color,
                        selection_fill: selection_color.gamma_multiply(0.5),
                    };
                    let mut renderer = TreeRenderer {
                        ctx: &ctx,
                        out: RenderOutcome::default(),
                    };
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("debug_frame_tree_scroll")
                        .auto_shrink([false, false]);
                    if let Some(position) = target_row {
                        let offset = (position * row_stride - (viewport_h - ROW_H) * 0.5).max(0.0);
                        scroll = scroll.vertical_scroll_offset(offset);
                    }
                    scroll.show_rows(ui, ROW_H, rows.len(), |ui, range| {
                        for index in range {
                            renderer.render(ui, rows[index]);
                        }
                    });
                    renderer.out
                };
                for (index, expanded) in std::mem::take(&mut outcome.toggles) {
                    if expanded {
                        self.open.insert(index);
                    } else {
                        self.open.remove(&index);
                    }
                }
            });

        let hover = outcome.hovered.and_then(|index| {
            let node = cache.nodes.get(index)?;
            Some(FrameTreeHover {
                page: node.page,
                rect: page_rect(&cache.nodes, index)?,
                source: node.source.clone(),
            })
        });

        self.cache = Some(cache);
        if hover != self.hover {
            ui.ctx().request_repaint();
        }
        self.hover = hover.clone();
        FrameTreeOutcome {
            jump: outcome.jump,
            hover,
        }
    }
}

/// A centered weak label for the pane's empty states.
fn centered_weak(ui: &mut egui::Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new(text).weak());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(parent: Option<usize>, source: Option<(&str, Range<usize>)>) -> Node {
        Node {
            parent,
            children: Vec::new(),
            kind: NodeKind::Text,
            label: String::new(),
            detail_line: String::new(),
            source: source.map(|(path, range)| (PathBuf::from(path), range)),
            page: 0,
            item_pos: Point::zero(),
            local_rect: None,
            group_transform: None,
        }
    }

    /// A node with a box `size` at `item_pos` and an optional group transform.
    fn boxed_node(
        parent: Option<usize>,
        item_pos: Point,
        size: Size,
        group_transform: Option<Transform>,
    ) -> Node {
        Node {
            item_pos,
            local_rect: Some(Rect::from_pos_size(Point::zero(), size)),
            group_transform,
            ..node(parent, None)
        }
    }

    fn all_display() -> DisplayConfig {
        DisplayConfig {
            children: true,
            text: true,
            geometry: true,
            style: true,
            span: true,
            group_nodes: true,
            text_nodes: true,
            shape_nodes: true,
            image_nodes: true,
            link_nodes: true,
            tag_nodes: true,
        }
    }

    fn cache(nodes: Vec<Node>, generation: u64, config: DisplayConfig) -> FrameTreeCache {
        let roots = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.parent.is_none())
            .map(|(index, _)| index)
            .collect();
        FrameTreeCache {
            generation,
            config,
            nodes,
            roots,
        }
    }

    #[test]
    fn span_detail_is_gated_by_show_span_not_by_the_source() {
        let source = (PathBuf::from("a.typ"), 1..2);
        let hidden = DisplayConfig {
            span: false,
            ..all_display()
        };
        assert_eq!(
            span_detail(Some(&source), hidden, None),
            None,
            "the source exists but its detail is gated off"
        );

        let shown = DisplayConfig {
            span: true,
            ..all_display()
        };
        assert_eq!(
            span_detail(Some(&source), shown, None).as_deref(),
            Some("a.typ:1..2")
        );
        assert_eq!(span_detail(None, shown, None), None);
    }

    #[test]
    fn matching_uses_source_even_without_a_span_detail() {
        // A node built with 源码范围 off carries an empty detail_line but keeps
        // its source, so cursor matching still finds it.
        let nodes = vec![
            Node {
                source: Some((PathBuf::from("main.typ"), 2..4)),
                ..node(None, None)
            },
            node(None, None),
        ];
        assert_eq!(
            deepest_cursor_node(&nodes, Path::new("main.typ"), 3),
            Some(0)
        );
    }

    #[test]
    fn deepest_cursor_node_prefers_depth_and_matches_inclusive_end() {
        let nodes = vec![
            node(None, Some(("main.typ", 0..10))),
            node(Some(0), Some(("main.typ", 0..10))),
            node(Some(1), Some(("main.typ", 2..4))),
            node(Some(1), Some(("other.typ", 2..4))),
        ];
        assert_eq!(
            deepest_cursor_node(&nodes, Path::new("main.typ"), 3),
            Some(2),
            "the deepest node wins"
        );
        assert_eq!(
            deepest_cursor_node(&nodes, Path::new("main.typ"), 4),
            Some(2),
            "the inclusive end byte matches the node"
        );
        assert_eq!(
            deepest_cursor_node(&nodes, Path::new("main.typ"), 7),
            Some(1),
            "a byte outside the deepest node falls back to the parent"
        );
        assert_eq!(
            deepest_cursor_node(&nodes, Path::new("missing.typ"), 3),
            None,
            "a different path never matches"
        );
    }

    #[test]
    fn nodes_intersecting_uses_non_empty_selection_ranges() {
        let nodes = vec![
            node(None, Some(("main.typ", 0..3))),
            node(None, Some(("main.typ", 5..9))),
            node(None, Some(("main.typ", 20..30))),
            node(None, Some(("other.typ", 5..9))),
        ];
        assert_eq!(
            nodes_intersecting(&nodes, Path::new("main.typ"), std::slice::from_ref(&(6..8))),
            HashSet::from([1])
        );
        assert!(
            nodes_intersecting(&nodes, Path::new("main.typ"), std::slice::from_ref(&(7..7)))
                .is_empty(),
            "a collapsed range selects nothing"
        );
        assert_eq!(
            nodes_intersecting(&nodes, Path::new("main.typ"), &[1..2, 25..26]),
            HashSet::from([0, 2])
        );
    }

    #[test]
    fn compute_highlights_collects_every_caret_node() {
        let nodes = vec![
            node(None, Some(("main.typ", 0..10))),
            node(Some(0), Some(("main.typ", 1..2))),
            node(Some(0), Some(("main.typ", 5..6))),
        ];
        let query = HighlightQuery {
            path: Some(Path::new("main.typ")),
            cursor_byte: 1,
            selection: &[1..1, 5..5, 7..9],
        };
        let (cursor_nodes, selection_nodes) = compute_highlights(&nodes, &query);
        assert_eq!(cursor_nodes, vec![1, 2], "one node per caret");
        assert_eq!(selection_nodes, HashSet::from([0]), "7..9 overlaps node 0");
    }

    #[test]
    fn cache_rebuilds_on_generation_or_config_change() {
        let built = cache(vec![node(None, None)], 1, all_display());
        assert!(!cache_needs_rebuild(Some(&built), 1, all_display()));
        assert!(cache_needs_rebuild(Some(&built), 2, all_display()));
        let other = DisplayConfig {
            span: false,
            ..all_display()
        };
        assert!(cache_needs_rebuild(Some(&built), 1, other));
        assert!(cache_needs_rebuild(None, 1, all_display()));
    }

    #[test]
    fn install_cache_retains_only_in_range_open_indices() {
        let mut state = FrameTreeState {
            open: HashSet::from([0, 1, 5, 9]),
            ..Default::default()
        };
        let nodes = (0..3).map(|_| node(None, None)).collect();
        state.install_cache(cache(nodes, 1, all_display()));
        assert_eq!(
            state.open,
            HashSet::from([0, 1]),
            "out-of-range indices are pruned, not the whole set"
        );
    }

    #[test]
    fn recompute_highlights_scrolls_only_on_a_cursor_change() {
        let nodes = vec![
            node(None, Some(("main.typ", 0..10))),
            node(Some(0), Some(("main.typ", 2..4))),
        ];
        let mut state = FrameTreeState {
            cache: Some(cache(nodes, 1, all_display())),
            ..Default::default()
        };
        let cursor = |byte: usize, selection: Vec<Range<usize>>| CursorKey {
            path: Some(PathBuf::from("main.typ")),
            cursor_byte: byte,
            selection,
        };

        state.recompute_highlights(cursor(3, Vec::new()), true);
        assert_eq!(
            state.highlight.as_ref().unwrap().cursor_nodes,
            vec![1],
            "the deepest node is highlighted"
        );
        assert_eq!(
            state.pending_scroll,
            Some(1),
            "a cursor change arms a scroll"
        );
        assert!(state.open.contains(&0), "ancestors are auto-expanded");

        state.pending_scroll = None;
        state.open.clear();
        state.recompute_highlights(cursor(3, Vec::new()), false);
        assert_eq!(
            state.pending_scroll, None,
            "an unchanged cursor never re-arms a scroll"
        );
        assert!(
            state.open.is_empty(),
            "an unchanged cursor never re-expands ancestors"
        );

        state.recompute_highlights(
            cursor(3, std::iter::once(20..25).collect::<Vec<_>>()),
            false,
        );
        assert!(
            state.highlight.as_ref().unwrap().selection_nodes.is_empty(),
            "highlights still recompute for a selection-only change"
        );
        assert_eq!(
            state.pending_scroll, None,
            "no scroll without a cursor move"
        );
    }

    #[test]
    fn flat_visible_rows_descends_only_into_open_nodes() {
        let mut nodes = vec![
            node(None, None),
            node(Some(0), None),
            node(Some(0), None),
            node(Some(1), None),
        ];
        nodes[0].children = vec![1, 2];
        nodes[1].children = vec![3];
        let roots = vec![0];

        let closed = flat_visible_rows(&nodes, &roots, &HashSet::new(), all_display());
        let closed_pairs: Vec<(usize, usize)> =
            closed.iter().map(|row| (row.index, row.depth)).collect();
        assert_eq!(
            closed_pairs,
            vec![(0, 0)],
            "a closed root hides its subtree"
        );

        let open = HashSet::from([0, 1]);
        let rows = flat_visible_rows(&nodes, &roots, &open, all_display());
        let pairs: Vec<(usize, usize)> = rows.iter().map(|row| (row.index, row.depth)).collect();
        assert_eq!(
            pairs,
            vec![(0, 0), (1, 1), (3, 2), (2, 1)],
            "DFS order and depth for the open subtree"
        );
    }

    #[test]
    fn hidden_group_nodes_are_flattened_but_other_kinds_are_dropped() {
        let mut nodes = vec![
            node(None, None),
            node(Some(0), None),
            node(Some(0), None),
            node(Some(1), None),
        ];
        nodes[0].kind = NodeKind::Page;
        nodes[0].children = vec![1, 2];
        nodes[1].kind = NodeKind::Group;
        nodes[1].children = vec![3];
        nodes[2].kind = NodeKind::Text;
        nodes[3].kind = NodeKind::Shape;
        let roots = vec![0];
        let open = HashSet::from([0, 1]);

        let no_groups = flat_visible_rows(
            &nodes,
            &roots,
            &open,
            DisplayConfig {
                group_nodes: false,
                ..all_display()
            },
        );
        let pairs: Vec<(usize, usize)> =
            no_groups.iter().map(|row| (row.index, row.depth)).collect();
        assert_eq!(
            pairs,
            vec![(0, 0), (3, 1), (2, 1)],
            "a hidden group disappears but its child is promoted to the group's depth"
        );

        let no_text = flat_visible_rows(
            &nodes,
            &roots,
            &open,
            DisplayConfig {
                text_nodes: false,
                ..all_display()
            },
        );
        let no_text_pairs: Vec<(usize, usize)> =
            no_text.iter().map(|row| (row.index, row.depth)).collect();
        assert!(
            no_text.iter().all(|row| row.index != 2),
            "a hidden leaf is omitted: {no_text_pairs:?}"
        );
        assert!(
            no_text.iter().any(|row| row.index == 3),
            "other kinds remain: {no_text_pairs:?}"
        );
    }

    #[test]
    fn display_text_escapes_newlines_and_truncates() {
        assert_eq!(display_text("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(display_text("short"), "short");

        let long = "x".repeat(TEXT_MAX + 12);
        let shown = display_text(&long);
        assert_eq!(shown.chars().count(), TEXT_MAX + 1, "48 chars plus the cut");
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn display_source_path_prefers_the_workspace_relative_form() {
        let root = Path::new("C:/proj");
        assert_eq!(
            display_source_path(Path::new("C:/proj/sub/a.typ"), Some(root)),
            "sub/a.typ"
        );
        assert_eq!(
            display_source_path(Path::new("C:/elsewhere/a.typ"), Some(root)),
            "a.typ",
            "outside the root only the file name is shown"
        );
        assert_eq!(
            display_source_path(Path::new("C:/elsewhere/a.typ"), None),
            "a.typ"
        );
    }

    #[test]
    fn format_paint_labels_a_solid_color_as_rgba() {
        let black = Paint::Solid(typst::visualize::Color::BLACK);
        assert_eq!(format_paint(&black), "Solid(0,0,0,255)");
    }

    #[test]
    fn page_rect_offsets_by_item_pos_under_the_page_root() {
        let root = boxed_node(
            None,
            Point::zero(),
            Size::new(Abs::pt(100.0), Abs::pt(200.0)),
            None,
        );
        let leaf = boxed_node(
            Some(0),
            Point::new(Abs::pt(10.0), Abs::pt(20.0)),
            Size::new(Abs::pt(5.0), Abs::pt(6.0)),
            None,
        );
        let nodes = vec![root, leaf];
        assert_eq!(
            page_rect(&nodes, 1),
            Some(Rect::new(
                Point::new(Abs::pt(10.0), Abs::pt(20.0)),
                Point::new(Abs::pt(15.0), Abs::pt(26.0)),
            )),
            "the leaf box is item_pos + local under the page root"
        );
    }

    #[test]
    fn page_rect_folds_through_an_ancestor_group_transform() {
        let root = boxed_node(
            None,
            Point::zero(),
            Size::new(Abs::pt(1.0), Abs::pt(1.0)),
            None,
        );
        let group = boxed_node(
            Some(0),
            Point::new(Abs::pt(30.0), Abs::pt(40.0)),
            Size::new(Abs::pt(10.0), Abs::pt(10.0)),
            Some(Transform::translate(Abs::pt(5.0), Abs::pt(7.0))),
        );
        let leaf = boxed_node(
            Some(1),
            Point::new(Abs::pt(1.0), Abs::pt(2.0)),
            Size::new(Abs::pt(4.0), Abs::pt(4.0)),
            None,
        );
        let nodes = vec![root, group, leaf];
        // Leaf local in the group frame is (1,2)-(5,6); translate (5,7) gives
        // (6,9)-(10,13); the group's own item_pos (30,40) then gives (36,49)-(40,53).
        assert_eq!(
            page_rect(&nodes, 2),
            Some(Rect::new(
                Point::new(Abs::pt(36.0), Abs::pt(49.0)),
                Point::new(Abs::pt(40.0), Abs::pt(53.0)),
            )),
            "the ancestor transform folds the leaf into page coordinates"
        );
    }

    #[test]
    fn page_rect_is_none_for_a_boxless_node() {
        let root = boxed_node(
            None,
            Point::zero(),
            Size::new(Abs::pt(1.0), Abs::pt(1.0)),
            None,
        );
        let tag = node(Some(0), None);
        let nodes = vec![root, tag];
        assert_eq!(
            page_rect(&nodes, 1),
            None,
            "a node without a box has no page rect"
        );
        assert_eq!(page_rect(&nodes, 99), None, "an out-of-range index is None");
    }
}
