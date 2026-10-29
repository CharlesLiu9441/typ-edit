//! Per-logical-line galley cache for the virtualized text path.
//!
//! Each entry holds the shaped [`egui::Galley`] for one logical line (soft-wrapped
//! into as many rows as the [`crate::WrapMode`] demands). Entries are keyed by line index and
//! validated per line:
//!
//! * A shaping-parameter change — font, theme, scale, wrap mode/width or line
//!   spacing ([`LayoutEpoch`]) clears the cache wholesale, because every line
//!   must re-shape.
//! * A text edit ([`LineData::text_rev`]) keeps every entry. Each line is
//!   revalidated lazily the first time it is queried under the new revision by
//!   hashing its shaped window's text, so content-unchanged lines are reused and
//!   only the edited line re-shapes.
//! * A new parse ([`LineData::parse_generation`]) likewise revalidates each line
//!   against a hash of its clipped formatting input, so lines whose highlights
//!   (or code/math/raw ranges) did not change are reused.
//! * A line-count change invalidates the height index in O(1) (stale deltas read
//!   as the fallback row height until the line is measured again) and drops
//!   entries past the new end; the entries themselves survive.
//!
//! A line longer than [`window::LONG_LINE_CHARS`] is shaped through the visible
//! window and its fingerprints cover that window. [`LineCache::prefix_height`]
//! and [`LineCache::total_height`] answer absolute-position queries in O(log N)
//! through a revision-stamped Fenwick tree over each measured line's height
//! delta relative to the fallback row height, so scrolling never walks (or
//! shapes) the lines above the viewport.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use typ_edit_model::HighlightKind;

use super::hit;
use super::window::{self, Window};
use crate::{WrapMode, highlight, wrap};

/// Everything that changes the shaped geometry of *every* line.
///
/// Content and parse revisions are deliberately absent: they invalidate per
/// line through the fingerprints stored on each [`CachedLine`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutEpoch {
    font_family: u64,
    font_size_bits: u32,
    dark: bool,
    wrap_mode: WrapMode,
    wrap_width_bits: u32,
    line_spacing_bits: u32,
    wrap_line_spacing_bits: u32,
    ppp_bits: u32,
}

/// Fenwick tree over per-line height deltas, relative to the fallback row
/// height, with a revision stamp on every node.
///
/// A global invalidation (layout epoch or line-count change) bumps `rev` in
/// O(1): nodes written for an older revision read as zero, so no O(N) clear
/// happens on edits. Measuring a line point-updates its delta; prefix sums are
/// O(log N). The fallback baseline is added by [`LineCache`] at query time.
struct HeightIndex {
    /// 1-based Fenwick storage; `tree[0]` is unused padding.
    tree: Vec<f32>,
    /// Revision each node's value belongs to; stale nodes read as zero.
    stamp: Vec<u64>,
    /// Revision all writes since the last invalidation share.
    rev: u64,
    /// Test-only count of Fenwick steps, to pin the sublinear query cost.
    #[cfg(test)]
    work: std::cell::Cell<usize>,
}

impl HeightIndex {
    const fn new() -> Self {
        Self {
            tree: Vec::new(),
            stamp: Vec::new(),
            rev: 1,
            #[cfg(test)]
            work: std::cell::Cell::new(0),
        }
    }

    /// Revision measured deltas must be stamped with to count.
    fn rev(&self) -> u64 {
        self.rev
    }

    /// Advances to a fresh revision; zero never repeats.
    fn next_rev(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        if self.rev == 0 {
            self.rev = 1;
        }
    }

    /// Drops every delta and starts a fresh revision.
    fn reset(&mut self) {
        self.tree.clear();
        self.stamp.clear();
        self.next_rev();
    }

    /// Makes every registered delta read as zero in O(1); used when the line
    /// count changed and index-keyed deltas can no longer be trusted.
    fn invalidate(&mut self) {
        self.next_rev();
    }

    /// Sizes the storage for `lines` lines. Called before any point update at
    /// the current line count: growing after writes would leave the newly
    /// allocated ancestors missing their children's aggregates.
    fn ensure_capacity(&mut self, lines: usize) {
        let needed = lines + 1;
        if self.tree.len() < needed {
            self.tree.resize(needed, 0.0);
            self.stamp.resize(needed, 0);
        }
    }

    /// Grows the storage so `line` can be written (a safety net; the cache
    /// presizes for the whole line count).
    fn ensure(&mut self, line: usize) {
        self.ensure_capacity(line + 1);
    }

    /// Sum of the deltas registered for lines `0..count`.
    fn prefix(&self, count: usize) -> f32 {
        let mut sum = 0.0f32;
        let mut i = count.min(self.tree.len().saturating_sub(1));
        while i > 0 {
            #[cfg(test)]
            self.work.set(self.work.get() + 1);
            if self.stamp[i] == self.rev {
                sum += self.tree[i];
            }
            i -= i.isolate_lowest_one();
        }
        sum
    }

    /// Registers `delta` for `line`, replacing any previous value.
    fn set(&mut self, line: usize, delta: f32) {
        self.ensure(line);
        let old = self.prefix(line + 1) - self.prefix(line);
        let mut i = line + 1;
        while i < self.tree.len() {
            #[cfg(test)]
            self.work.set(self.work.get() + 1);
            if self.stamp[i] != self.rev {
                self.tree[i] = 0.0;
                self.stamp[i] = self.rev;
            }
            self.tree[i] += delta - old;
            i += i.isolate_lowest_one();
        }
    }
}

/// Shaping inputs for one cache interrogation: the buffer plus the highlighted
/// spans (whole-buffer byte ranges) and the active font/wrap configuration.
#[derive(Clone, Copy)]
pub struct LineData<'a> {
    pub buffer: &'a typ_edit_buffer::TextBuffer,
    pub text_rev: u64,
    pub parse_generation: u64,
    pub highlights: &'a [(Range<usize>, HighlightKind)],
    /// Byte ranges of code/math/raw mode, sorted and disjoint. A Markup line
    /// wraps when it has at least one char outside these ranges.
    pub blocked_ranges: &'a [Range<usize>],
    /// Byte ranges of code mode; subtracted from `blocked_ranges` to find the
    /// raw spans (the other wrap atoms).
    pub code_ranges: &'a [Range<usize>],
    /// Byte ranges of math mode; also grants after-comma break opportunities.
    pub math_ranges: &'a [Range<usize>],
    /// Byte ranges of unbreakable math atoms (leaf-level: every math leaf is an
    /// atom; only a function name plus its `(` and a number plus its unit are
    /// glued). A break may fall before any atom.
    pub math_atoms: &'a [Range<usize>],
    pub font_family: &'a str,
    pub font_size: f32,
    pub wrap_mode: WrapMode,
    /// Multiplier of the base font row height for the gap between consecutive
    /// logical lines (`1.0` = no extra gap).
    pub line_spacing: f32,
    /// Multiplier of the base font row height for the gap between soft-wrap
    /// continuation rows of one logical line (`1.0` = no extra gap).
    pub wrap_line_spacing: f32,
    /// Wrapping width in points; `f32::INFINITY` when wrapping is off.
    pub wrap_width: f32,
}

/// Whether a Markup line's `[start_byte, end_byte)` span contains any char not
/// covered by `blocked_ranges` (markup body text, styles transparent). A line
/// fully covered by code/math/raw stays single-row and scrolls horizontally.
pub(crate) fn wraps_as_markup(data: &LineData, start_byte: usize, end_byte: usize) -> bool {
    let mut at = start_byte;
    while at < end_byte {
        match data
            .blocked_ranges
            .iter()
            .find(|range| range.start <= at && at < range.end)
        {
            Some(range) => at = range.end.min(end_byte),
            None => return true,
        }
    }
    false
}

/// Raw spans: `blocked` with `code` and `math` removed.
///
/// All inputs are sorted, disjoint, whole-buffer byte ranges. Raw text is
/// markup that must not run the markup helpers; as a wrap atom it never splits
/// while it fits a row.
fn raw_spans(
    blocked: &[Range<usize>],
    code: &[Range<usize>],
    math: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut excluded: Vec<Range<usize>> = code.iter().chain(math).cloned().collect();
    excluded.sort_by_key(|range| range.start);
    let mut raw = Vec::new();
    for block in blocked {
        let mut at = block.start;
        for cut in &excluded {
            if cut.end <= at || cut.start >= block.end {
                continue;
            }
            if cut.start > at {
                raw.push(at..cut.start.min(block.end));
            }
            at = at.max(cut.end);
            if at >= block.end {
                break;
            }
        }
        if at < block.end {
            raw.push(at..block.end);
        }
    }
    raw
}

/// Sorts ranges by start and merges those that **overlap**; merely adjacent
/// ranges stay separate, so a break may fall between two touching atoms (the
/// leaf-level math atoms rely on this).
fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Terminal CJK punctuation that must not begin a wrapped line (禁则 kinsoku).
///
/// The set is the closing/terminal punctuation `；：、，。！？—”）`. Opening
/// quotes and brackets (`“`, `（`) are deliberately absent: they glue forward,
/// so dragging the preceding char with them would be wrong.
fn is_kinsoku(chr: char) -> bool {
    matches!(
        chr,
        '\u{FF1B}'
            | '\u{FF1A}'
            | '\u{3001}'
            | '\u{FF0C}'
            | '\u{3002}'
            | '\u{FF01}'
            | '\u{FF1F}'
            | '\u{2014}'
            | '\u{201D}'
            | '\u{FF09}'
    )
}

/// Kinsoku atoms: every terminal punctuation plus its immediately preceding
/// character, as line-local (window-local) char ranges.
///
/// The pair is one unbreakable unit, so a wrap leaves the punctuation with its
/// predecessor instead of starting a row. A punctuation at the very start of
/// the line has no preceding char and forms a one-char atom. A doubled em dash
/// (`——`) produces two overlapping atoms that [`merge_ranges`] fuses into one.
fn kinsoku_atoms(text: &str) -> Vec<Range<usize>> {
    let mut atoms = Vec::new();
    for (index, chr) in text.chars().enumerate() {
        if is_kinsoku(chr) {
            atoms.push(index.saturating_sub(1)..index + 1);
        }
    }
    atoms
}

/// Clips whole-buffer byte ranges to `[start_byte, end_byte)`, rebases them to
/// line-local char offsets from `start_char`, then sorts and merges.
fn line_char_ranges(
    buffer: &typ_edit_buffer::TextBuffer,
    ranges: impl IntoIterator<Item = Range<usize>>,
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
) -> Vec<Range<usize>> {
    let mapped: Vec<Range<usize>> = ranges
        .into_iter()
        .filter_map(|range| {
            let lo = range.start.max(start_byte);
            let hi = range.end.min(end_byte);
            (lo < hi)
                .then(|| buffer.byte_to_char(lo) - start_char..buffer.byte_to_char(hi) - start_char)
        })
        .collect();
    merge_ranges(mapped)
}

/// The line's wrap context: raw spans, leaf-level math atoms and CJK kinsoku
/// pairs as atoms, plus math mode for the after-comma break opportunities, all
/// line-local char ranges.
#[cfg(test)]
pub(crate) fn line_wrap_ranges(
    data: &LineData,
    buffer: &typ_edit_buffer::TextBuffer,
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
    text: &str,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let raw = raw_spans(data.blocked_ranges, data.code_ranges, data.math_ranges);
    line_wrap_ranges_with_raw(data, buffer, start_byte, end_byte, start_char, text, &raw)
}

/// Wrap ranges from precomputed raw spans, shared by all lines shaped from the
/// same parse input.
fn line_wrap_ranges_with_raw(
    data: &LineData,
    buffer: &typ_edit_buffer::TextBuffer,
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
    text: &str,
    raw: &[Range<usize>],
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut atoms = line_char_ranges(
        buffer,
        raw.iter().cloned().chain(data.math_atoms.iter().cloned()),
        start_byte,
        end_byte,
        start_char,
    );
    // Markup-only mode wraps the markup body, but code arguments must remain
    // intact. Treat code spans as atoms as well; Always mode keeps the old
    // behavior so long code lines can still soft-wrap.
    if data.wrap_mode == WrapMode::Markup {
        let code = data.code_ranges.iter().filter_map(|range| {
            let mut lo = range.start.max(start_byte);
            let mut hi = range.end.min(end_byte);
            if lo >= hi {
                return None;
            }
            if lo > start_byte && buffer.char_at(buffer.byte_to_char(lo - 1)) == Some('#') {
                lo -= 1;
            }
            // A code atom that follows only indentation must carry that
            // indentation with it; otherwise a width break can leave a
            // whitespace-only row before the code. Likewise, absorb trailing
            // line whitespace so it cannot become a blank row.
            if buffer
                .slice(start_byte..lo)
                .chars()
                .all(char::is_whitespace)
            {
                lo = start_byte;
            }
            if buffer.slice(hi..end_byte).chars().all(char::is_whitespace) {
                hi = end_byte;
            }
            Some(lo..hi)
        });
        atoms.extend(line_char_ranges(
            buffer, code, start_byte, end_byte, start_char,
        ));
    }
    atoms.extend(kinsoku_atoms(text));
    let atoms = merge_ranges(atoms);
    let math = line_char_ranges(
        buffer,
        data.math_ranges.iter().cloned(),
        start_byte,
        end_byte,
        start_char,
    );
    (atoms, math)
}

/// One cached logical line.
pub struct CachedLine {
    pub galley: Arc<egui::Galley>,
    /// Total painted height, including soft-wrap continuation rows.
    pub height: f32,
    /// Number of galley rows (>= 1 even for an empty line).
    pub rows: usize,
    /// Char offset of the shaped window within the logical line (0 for a
    /// whole-line shape).
    pub window_start: usize,
    /// Number of chars in the shaped window.
    pub window_len: usize,
    /// True when the shaped window is smaller than the whole logical line.
    pub windowed: bool,
    /// True when this entry is a budget placeholder (nothing shaped).
    pub placeholder: bool,
    /// Estimated monospace advance (points), for long-line placement.
    pub advance: f32,
    /// `text_rev` the shaped window's text was last verified against.
    content_rev: u64,
    /// `parse_generation` the clipped formatting input was last verified against.
    parse_rev: u64,
    /// Fingerprint of the shaped window's text.
    content_hash: u64,
    /// Fingerprint of the formatting input clipped to the shaped window.
    format_hash: u64,
    /// [`HeightIndex`] revision the entry's height delta is registered under.
    index_rev: u64,
}

/// One line's shape request, grouped to keep [`LineCache::shape_line`] small.
struct ShapeRequest {
    line: usize,
    line_len: usize,
    window: Window,
    advance: f32,
}

impl CachedLine {
    /// Whether the shaped window fully contains `want`.
    fn covers(&self, want: Window) -> bool {
        !self.placeholder
            && Window {
                start: self.window_start,
                len: self.window_len,
            }
            .covers(want)
    }

    /// Whether the entry can serve `desired` for `line` under `data`: the
    /// shaped window still covers the request and neither the content nor the
    /// clipping input changed.
    ///
    /// The hashes are only computed when the corresponding revision moved, so a
    /// steady frame reuses the entry without touching the text.
    fn matches(&self, data: &LineData, line: usize, desired: Window) -> bool {
        if self.placeholder || !self.covers(desired) {
            return false;
        }
        let start_char = data.buffer.line_start(line) + self.window_start;
        let end_char = start_char + self.window_len;
        if self.content_rev != data.text_rev
            && hash_buffer_range(data.buffer, start_char, end_char) != self.content_hash
        {
            return false;
        }
        if (self.content_rev != data.text_rev || self.parse_rev != data.parse_generation)
            && format_hash(data, start_char, end_char) != self.format_hash
        {
            return false;
        }
        true
    }

    /// Marks the entry as verified under `data`'s revisions.
    fn refresh_revs(&mut self, data: &LineData) {
        self.content_rev = data.text_rev;
        self.parse_rev = data.parse_generation;
    }
}

/// FNV-1a offset basis and prime, used for the entry fingerprints.
const HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_mix(hash: u64, value: u64) -> u64 {
    let mut hash = hash;
    for byte in value.to_le_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(HASH_PRIME);
    }
    hash
}

fn hash_text(text: &str) -> u64 {
    let mut hash = HASH_OFFSET;
    for chr in text.chars() {
        hash = hash_mix(hash, chr as u64);
    }
    hash
}

fn hash_buffer_range(buffer: &typ_edit_buffer::TextBuffer, start: usize, end: usize) -> u64 {
    let mut hash = HASH_OFFSET;
    for at in start..end {
        let Some(chr) = buffer.char_at(at) else {
            break;
        };
        hash = hash_mix(hash, chr as u64);
    }
    hash
}

/// Fingerprint of every parse-derived shaping input clipped to
/// `[start_byte, end_byte)`, with offsets rebased to the range start so a
/// uniform shift from an edit elsewhere leaves an unchanged line's hash equal.
///
/// The range lists are the parse worker's start-sorted, disjoint spans, so the
/// `partition_point` skips the spans that end before the range.
fn format_hash(data: &LineData, start_char: usize, end_char: usize) -> u64 {
    let start_byte = data.buffer.char_to_byte(start_char);
    let end_byte = data.buffer.char_to_byte(end_char);
    let mut hash = hash_mix(HASH_OFFSET, 1);
    hash = hash_highlight_spans(hash, data.highlights, start_byte, end_byte);
    for (tag, ranges) in [
        (2u64, data.blocked_ranges),
        (3, data.code_ranges),
        (4, data.math_ranges),
        (5, data.math_atoms),
    ] {
        hash = hash_mix(hash, tag);
        hash = hash_ranges(hash, ranges, start_byte, end_byte);
    }
    hash
}

fn hash_highlight_spans(
    mut hash: u64,
    spans: &[(Range<usize>, HighlightKind)],
    start_byte: usize,
    end_byte: usize,
) -> u64 {
    let first = spans.partition_point(|(range, _)| range.end <= start_byte);
    for (range, kind) in &spans[first..] {
        if range.start >= end_byte {
            break;
        }
        if let Some((lo, hi)) = clip(range, start_byte, end_byte) {
            hash = hash_mix(hash, *kind as u64);
            hash = hash_mix(hash, lo as u64);
            hash = hash_mix(hash, hi as u64);
        }
    }
    hash
}

fn hash_ranges(mut hash: u64, ranges: &[Range<usize>], start_byte: usize, end_byte: usize) -> u64 {
    let first = ranges.partition_point(|range| range.end <= start_byte);
    for range in &ranges[first..] {
        if range.start >= end_byte {
            break;
        }
        if let Some((lo, hi)) = clip(range, start_byte, end_byte) {
            hash = hash_mix(hash, lo as u64);
            hash = hash_mix(hash, hi as u64);
        }
    }
    hash
}

/// Clipped `lo..hi` relative to `start_byte`, or `None` when disjoint.
fn clip(range: &Range<usize>, start_byte: usize, end_byte: usize) -> Option<(usize, usize)> {
    let lo = range.start.max(start_byte);
    let hi = range.end.min(end_byte);
    if lo < hi {
        Some((lo - start_byte, hi - start_byte))
    } else {
        None
    }
}

/// Identity of the parse ranges that decide raw spans. Slice pointers make
/// distinct inputs miss even when revisions and lengths coincide; the
/// revisions catch changed contents behind a recycled allocation.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RawSpanKey {
    blocked_ptr: usize,
    code_ptr: usize,
    math_ptr: usize,
    blocked_len: usize,
    code_len: usize,
    math_len: usize,
    parse_generation: u64,
    text_rev: u64,
}

struct RawSpanCache {
    key: RawSpanKey,
    spans: Arc<Vec<Range<usize>>>,
}

/// Lazy, per-line-validated cache of shaped lines.
pub struct LineCache {
    epoch: Option<LayoutEpoch>,
    entries: HashMap<usize, CachedLine>,
    /// Revision-stamped prefix sums over measured heights.
    heights: HeightIndex,
    /// Buffer line count at the last query, to detect line-count changes.
    line_count: Option<usize>,
    /// Horizontal scroll offset for the current frame's window choice.
    h_offset: f32,
    /// Viewport width for the current frame's window choice.
    viewport_width: f32,
    /// Chars shaped so far this frame (reset by [`Self::begin_frame`]).
    shaped_this_frame: usize,
    /// Per-frame shaping budget, in chars.
    budget: usize,
    /// Set when the budget skipped at least one line this frame.
    budget_hit: bool,
    /// Logical line ranges hidden by collapsed folds; they contribute zero
    /// height to the prefix sums and are never shaped or painted.
    hidden: Vec<Range<usize>>,
    /// Fallback row height the hidden-line deltas were registered with, reused
    /// when the height index is reset and the deltas must be re-applied.
    hidden_fallback: f32,
    /// [`HeightIndex`] revision [`Self::hidden`]'s deltas belong to; a mismatch
    /// means a reset/invalidation dropped them and they must be re-registered.
    hidden_rev: u64,
    /// Raw spans for the current parse input, shared by every shaped line.
    raw_cache: Option<RawSpanCache>,
}

impl LineCache {
    pub fn new() -> Self {
        Self {
            epoch: None,
            entries: HashMap::new(),
            heights: HeightIndex::new(),
            line_count: None,
            h_offset: 0.0,
            viewport_width: 0.0,
            shaped_this_frame: 0,
            budget: window::SHAPING_BUDGET_CHARS,
            budget_hit: false,
            hidden: Vec::new(),
            hidden_fallback: 0.0,
            hidden_rev: 0,
            raw_cache: None,
        }
    }

    /// Drops the shared raw-span scratch; a new buffer may reuse the old
    /// slices' addresses and revisions, so identity alone is not enough.
    pub(crate) fn clear_raw_spans(&mut self) {
        self.raw_cache = None;
    }

    /// Drops every shaped line so the next frame re-shapes with the current
    /// egui fonts. `Context::set_fonts` can change the face behind a family
    /// without changing the family/size epoch, so the cache must be told.
    pub fn invalidate_fonts(&mut self) {
        self.entries.clear();
        self.heights.reset();
        self.epoch = None;
        self.line_count = None;
        self.raw_cache = None;
    }

    /// Resets the per-frame shaping budget. Call once before the frame's
    /// shaping pass.
    pub fn begin_frame(&mut self) {
        self.shaped_this_frame = 0;
        self.budget_hit = false;
    }

    /// Sets the horizontal offset and viewport width that choose long-line
    /// windows for the current frame.
    pub fn set_horizontal(&mut self, h_offset: f32, viewport_width: f32) {
        self.h_offset = h_offset;
        self.viewport_width = viewport_width;
    }

    /// The logical lines collapsed folds hide.
    pub fn hidden(&self) -> &[Range<usize>] {
        &self.hidden
    }

    /// Whether `line` is hidden by a collapsed fold.
    fn hidden_line(&self, line: usize) -> bool {
        super::folds::is_hidden(&self.hidden, line)
    }

    /// Points the cache at a new set of hidden logical lines: lines that just
    /// became hidden register a `-fallback` height delta (zero painted height),
    /// and lines that just reappeared restore their measured delta (or a zero
    /// delta while they wait to be measured again).
    pub fn set_hidden(&mut self, hidden: Vec<Range<usize>>, fallback: f32) {
        self.restore_hidden();
        let fallback = fallback.max(0.0);
        let old = std::mem::replace(&mut self.hidden, hidden);
        self.hidden_fallback = fallback;
        for range in &self.hidden {
            for line in range.clone() {
                if !super::folds::is_hidden(&old, line) {
                    self.heights.set(line, -fallback);
                }
            }
        }
        for range in &old {
            for line in range.clone() {
                if !super::folds::is_hidden(&self.hidden, line) {
                    let delta = self
                        .entries
                        .get(&line)
                        .map_or(0.0, |entry| entry.height.max(0.0) - fallback);
                    self.heights.set(line, delta);
                }
            }
        }
        self.hidden_rev = self.heights.rev();
    }

    /// Re-registers the hidden-line deltas after a reset/invalidation dropped
    /// them, so hidden lines stay zero-height across font and line-count changes.
    fn restore_hidden(&mut self) {
        let rev = self.heights.rev();
        if self.hidden_rev == rev {
            return;
        }
        let fallback = self.hidden_fallback.max(0.0);
        for range in &self.hidden {
            for line in range.clone() {
                self.heights.set(line, -fallback);
            }
        }
        self.hidden_rev = rev;
    }

    /// Whether the shaping budget skipped a line this frame; the caller should
    /// request a repaint so the skipped lines can shape next frame.
    pub fn budget_hit(&self) -> bool {
        self.budget_hit
    }

    /// Chars shaped during the current frame (tests assert the window bound).
    #[cfg(test)]
    pub fn shaped_chars_this_frame(&self) -> usize {
        self.shaped_this_frame
    }

    /// Overrides the shaping budget, for a deterministic budget test.
    #[cfg(test)]
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    /// `(window_start, window_len, placeholder)` for a cached line, for tests.
    #[cfg(test)]
    pub fn cached_window(&self, line: usize) -> Option<(usize, usize, bool)> {
        self.entries
            .get(&line)
            .map(|cached| (cached.window_start, cached.window_len, cached.placeholder))
    }

    /// Number of cached lines (tests also use it to observe an epoch drop).
    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// The cached galley for `line`, if any (tests assert it is never mutated by
    /// selection painting).
    #[cfg(test)]
    pub fn cached_galley(&self, line: usize) -> Option<std::sync::Arc<egui::Galley>> {
        self.entries
            .get(&line)
            .map(|cached| std::sync::Arc::clone(&cached.galley))
    }

    /// Fenwick steps spent since [`Self::reset_height_work`], for tests that
    /// pin the sublinear query cost.
    #[cfg(test)]
    pub fn height_work(&self) -> usize {
        self.heights.work.get()
    }

    /// Zeroes the Fenwick step counter, for tests.
    #[cfg(test)]
    pub fn reset_height_work(&self) {
        self.heights.work.set(0);
    }

    /// The [`LayoutEpoch`] implied by `data` under `ui`'s theme and scale.
    fn epoch(ui: &egui::Ui, data: &LineData) -> LayoutEpoch {
        LayoutEpoch {
            font_family: hash_str(data.font_family),
            font_size_bits: data.font_size.to_bits(),
            dark: ui.visuals().dark_mode,
            wrap_mode: data.wrap_mode,
            wrap_width_bits: data.wrap_width.to_bits(),
            line_spacing_bits: data.line_spacing.to_bits(),
            wrap_line_spacing_bits: data.wrap_line_spacing.to_bits(),
            ppp_bits: ui.ctx().pixels_per_point().to_bits(),
        }
    }

    /// A single row's height for the configured font, used when a line has no
    /// rows (an empty line) and as the fallback for unmeasured lines.
    pub fn row_height(&self, ui: &egui::Ui, data: &LineData) -> f32 {
        let font_id = egui::FontId::new(
            data.font_size,
            egui::FontFamily::Name(Arc::from(data.font_family)),
        );
        ui.fonts_mut(|fonts| fonts.row_height(&font_id))
    }

    /// Returns the shaped galley for logical `line`, shaping and caching it on
    /// first use. A layout-epoch change clears the cache; text and parse
    /// revisions revalidate per line instead (see the module docs).
    ///
    /// A line longer than [`window::LONG_LINE_CHARS`] is shaped through the
    /// window covering the visible horizontal range; the cached entry is reused
    /// while it still covers the requested window. Once the frame's shaping
    /// budget is spent the line becomes a placeholder and [`Self::budget_hit`]
    /// is set, so the caller repaints; an existing placeholder for the line is
    /// reused instead of being re-created.
    pub fn line_galley(&mut self, ui: &egui::Ui, data: &LineData, line: usize) -> &CachedLine {
        let epoch = Self::epoch(ui, data);
        if self.epoch.as_ref() != Some(&epoch) {
            self.entries.clear();
            self.heights.reset();
            self.epoch = Some(epoch);
            self.line_count = None;
            self.raw_cache = None;
        }
        self.observe_line_count(data.buffer.line_count());

        let line_len = hit::line_char_len(data.buffer, line);
        let long = line_len > window::LONG_LINE_CHARS;
        let advance = if long {
            window::advance_estimate(ui, data)
        } else {
            0.0
        };
        let desired = if long {
            window::window_for(line_len, self.h_offset, self.viewport_width, advance)
        } else {
            Window::whole(line_len)
        };
        let fallback = self.row_height(ui, data);
        let over_budget =
            desired.len > 0 && self.shaped_this_frame.saturating_add(desired.len) > self.budget;

        enum Decision {
            Reuse,
            Shape,
            Placeholder,
        }
        let decision = match self.entries.get(&line) {
            Some(cached) if cached.placeholder => {
                if over_budget {
                    Decision::Reuse
                } else {
                    Decision::Shape
                }
            }
            Some(cached) if cached.matches(data, line, desired) => Decision::Reuse,
            _ => {
                if over_budget {
                    Decision::Placeholder
                } else {
                    Decision::Shape
                }
            }
        };
        match decision {
            Decision::Reuse => {
                if self
                    .entries
                    .get(&line)
                    .is_some_and(|cached| cached.placeholder)
                {
                    self.budget_hit = true;
                }
                let rev = self.heights.rev();
                let hidden = self.hidden_line(line);
                let cached = self.entries.get_mut(&line).expect("entry just observed");
                if cached.index_rev != rev {
                    let delta = if hidden {
                        -fallback.max(0.0)
                    } else {
                        cached.height.max(0.0) - fallback.max(0.0)
                    };
                    cached.index_rev = rev;
                    self.heights.set(line, delta);
                }
                if !cached.placeholder {
                    cached.refresh_revs(data);
                }
            }
            Decision::Shape => {
                self.shaped_this_frame += desired.len;
                let cached = self.shape_line(
                    ui,
                    data,
                    ShapeRequest {
                        line,
                        line_len,
                        window: desired,
                        advance,
                    },
                );
                self.insert_entry(line, cached, fallback);
            }
            Decision::Placeholder => {
                self.budget_hit = true;
                if !self
                    .entries
                    .get(&line)
                    .is_some_and(|cached| cached.placeholder)
                {
                    let cached = self.placeholder_line(ui, data, advance);
                    self.insert_entry(line, cached, fallback);
                }
            }
        }
        &self.entries[&line]
    }

    /// Reacts to a line-count change: index-keyed height deltas can no longer
    /// be trusted, so they are invalidated in O(1), and entries past a
    /// shrunken end are dropped so a truncated document frees its cache.
    fn observe_line_count(&mut self, line_count: usize) {
        if self.line_count == Some(line_count) {
            return;
        }
        self.heights.ensure_capacity(line_count);
        if self.line_count.is_some_and(|prev| line_count < prev) {
            self.entries.retain(|index, _| *index < line_count);
        }
        self.heights.invalidate();
        self.restore_hidden();
        self.line_count = Some(line_count);
    }

    /// Stores `cached` and registers its height delta in the current revision.
    fn insert_entry(&mut self, line: usize, mut cached: CachedLine, fallback: f32) {
        cached.index_rev = self.heights.rev();
        let delta = if self.hidden_line(line) {
            -fallback.max(0.0)
        } else {
            cached.height.max(0.0) - fallback.max(0.0)
        };
        self.heights.set(line, delta);
        self.entries.insert(line, cached);
    }

    /// Cached height for `line`, shaping it if necessary. A hidden line always
    /// measures zero and is never shaped.
    pub fn line_height(&mut self, ui: &egui::Ui, data: &LineData, line: usize) -> f32 {
        if self.hidden_line(line) {
            return 0.0;
        }
        self.line_galley(ui, data, line).height
    }

    /// Whether `line` already holds a shaped (non-placeholder) galley, without
    /// shaping it.
    ///
    /// Vertical navigation consults this to keep the exact galley path for
    /// lines inside the viewport and fall back to a monospace estimate for
    /// off-screen lines, so a keypress never forces a shape for a line outside
    /// the viewport. A budget placeholder counts as absent: its galley is empty
    /// and carries no glyph geometry.
    pub fn has_galley(&self, line: usize) -> bool {
        self.entries
            .get(&line)
            .is_some_and(|cached| !cached.placeholder)
    }

    /// Walks forward from `anchor.top_line`, returning `(line, y, rows)` for each
    /// line whose top lies within `viewport_height + pad`. `y` is relative to the
    /// viewport top; the first line starts at `-anchor.offset`.
    pub fn visible_lines(
        &mut self,
        ui: &egui::Ui,
        data: &LineData,
        anchor: &super::scroll::ScrollAnchor,
        viewport_height: f32,
        pad: f32,
    ) -> Vec<(usize, f32, u32)> {
        let line_count = data.buffer.line_count();
        let limit = viewport_height + pad.max(0.0);
        let mut visible = Vec::new();
        let mut y = -anchor.offset;
        let mut line = super::folds::snap_visible(&self.hidden, anchor.top_line, line_count);
        while line < line_count && y <= limit {
            let cached = self.line_galley(ui, data, line);
            visible.push((line, y, cached.rows as u32));
            y += cached.height;
            line = super::folds::next_visible(&self.hidden, line + 1);
        }
        visible
    }

    /// Horizontal extent (points) of the visible single-row lines: the exact
    /// shaped width for a normal line, the full-length monospace estimate for a
    /// windowed long line (whose galley covers only the window). A soft-wrapped
    /// multi-row line contributes nothing.
    pub fn horizontal_extent(
        &mut self,
        ui: &egui::Ui,
        data: &LineData,
        visible: &[(usize, f32, u32)],
    ) -> f32 {
        let advance = window::advance_estimate(ui, data);
        let mut extent = 0.0f32;
        for (line, _, rows) in visible {
            let cached = self.line_galley(ui, data, *line);
            if cached.windowed {
                let len = hit::line_char_len(data.buffer, *line);
                extent = extent.max(len as f32 * advance);
            } else if *rows <= 1 {
                extent = extent.max(cached.galley.size().x);
            }
        }
        extent
    }

    /// Sum of the effective heights of lines `0..line` (clamped to
    /// `line_count`): every measured line's height plus one fallback row for
    /// each unmeasured line. O(log N), so far absolute positions never walk the
    /// document.
    pub fn prefix_height(&self, line: usize, line_count: usize, fallback_row_height: f32) -> f32 {
        let lines = line.min(line_count);
        lines as f32 * fallback_row_height.max(0.0) + self.heights.prefix(lines)
    }

    /// Estimated total text height: [`Self::prefix_height`] over every line.
    ///
    /// Measured lines contribute their exact height and unmeasured lines one
    /// fallback row, so the estimate converges as lines get measured — the
    /// custom scrollbar thumb may shift slightly during the first frames.
    pub fn total_height(&self, line_count: usize, fallback_row_height: f32) -> f32 {
        self.prefix_height(line_count, line_count, fallback_row_height)
    }

    /// Raw spans for `data`, computed once per distinct parse input. Every
    /// shaped line in a frame shares the returned handle.
    fn raw_spans_for(&mut self, data: &LineData) -> Arc<Vec<Range<usize>>> {
        let key = RawSpanKey {
            blocked_ptr: data.blocked_ranges.as_ptr() as usize,
            code_ptr: data.code_ranges.as_ptr() as usize,
            math_ptr: data.math_ranges.as_ptr() as usize,
            blocked_len: data.blocked_ranges.len(),
            code_len: data.code_ranges.len(),
            math_len: data.math_ranges.len(),
            parse_generation: data.parse_generation,
            text_rev: data.text_rev,
        };
        if self.raw_cache.as_ref().is_none_or(|cache| cache.key != key) {
            self.raw_cache = Some(RawSpanCache {
                key,
                spans: Arc::new(raw_spans(
                    data.blocked_ranges,
                    data.code_ranges,
                    data.math_ranges,
                )),
            });
        }
        Arc::clone(
            &self
                .raw_cache
                .as_ref()
                .expect("raw cache was just filled")
                .spans,
        )
    }

    /// Shapes one logical line's `window`: its text (newline excluded),
    /// highlights clipped and rebased to window-local byte offsets, then
    /// soft-wrapped only when the window is the whole line.
    fn shape_line(&mut self, ui: &egui::Ui, data: &LineData, request: ShapeRequest) -> CachedLine {
        let ShapeRequest {
            line,
            line_len,
            window,
            advance,
        } = request;
        let buffer = data.buffer;
        let line_start = buffer.line_start(line);
        let start_char = line_start + window.start;
        let end_char = line_start + window.end();
        let start_byte = buffer.char_to_byte(start_char);
        let end_byte = buffer.char_to_byte(end_char);
        let text = buffer.slice(start_char..end_char);

        let spans = highlight::clip_rebase_spans(data.highlights, start_byte, end_byte);
        let unwrapped = highlight::highlighted_galley(
            ui,
            &text,
            &spans,
            data.font_family,
            data.font_size,
            f32::INFINITY,
        );
        // Long lines always stay a single row and scroll horizontally, even
        // when soft wrapping is enabled, so their height never depends on the
        // full (unshaped) text. Otherwise the mode decides per line.
        let wraps = match data.wrap_mode {
            WrapMode::Off => false,
            WrapMode::Always => true,
            WrapMode::Markup => wraps_as_markup(data, start_byte, end_byte),
        };
        let galley = if window.len < line_len || !wraps {
            unwrapped
        } else {
            let raw = self.raw_spans_for(data);
            let (atoms, math) = line_wrap_ranges_with_raw(
                data,
                buffer,
                start_byte,
                end_byte,
                start_char,
                &text,
                raw.as_slice(),
            );
            let code = if data.wrap_mode == WrapMode::Markup {
                line_char_ranges(
                    buffer,
                    data.code_ranges.iter().cloned(),
                    start_byte,
                    end_byte,
                    start_char,
                )
            } else {
                Vec::new()
            };
            let context = wrap::WrapContext {
                atoms: &atoms,
                math: &math,
                code: &code,
                wrap_gap: self.wrap_gap(ui, data),
            };
            wrap::wrap_lines(
                &unwrapped,
                data.wrap_width,
                ui.ctx().pixels_per_point(),
                context,
            )
        };

        let rows = galley.rows.len();
        let painted = galley.size().y;
        let base = if painted > 0.0 {
            painted
        } else {
            // An empty line's galley has one zero-height row, so fall back to
            // the font's row height. Round it to a physical pixel exactly as
            // epaint rounds a painted row, otherwise the empty line is a hair
            // taller than its neighbours and every following line drifts.
            self.rounded_row_height(ui, data)
        };
        let height = base + self.line_gap(ui, data);
        CachedLine {
            galley,
            height,
            rows: rows.max(1),
            window_start: window.start,
            window_len: window.len,
            windowed: window.len < line_len,
            placeholder: false,
            advance,
            content_rev: data.text_rev,
            parse_rev: data.parse_generation,
            content_hash: hash_text(&text),
            format_hash: format_hash(data, start_char, end_char),
            index_rev: 0,
        }
    }

    /// A budget placeholder: no glyphs, one row tall, so vertical layout stays
    /// sane while the line waits for next frame's shaping budget. It carries no
    /// fingerprints: its height is content-independent and it stays reusable
    /// until the budget allows shaping.
    fn placeholder_line(&self, ui: &egui::Ui, data: &LineData, advance: f32) -> CachedLine {
        let mut job = highlight::build_layout_job_with_theme(
            "",
            &[],
            data.font_family,
            data.font_size,
            ui.visuals().dark_mode,
        );
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        CachedLine {
            galley,
            height: self.rounded_row_height(ui, data) + self.line_gap(ui, data),
            rows: 1,
            window_start: 0,
            window_len: 0,
            windowed: true,
            placeholder: true,
            advance,
            content_rev: data.text_rev,
            parse_rev: data.parse_generation,
            content_hash: 0,
            format_hash: 0,
            index_rev: 0,
        }
    }

    /// The font's row height rounded to a physical pixel, matching how epaint
    /// rounds a painted row.
    fn rounded_row_height(&self, ui: &egui::Ui, data: &LineData) -> f32 {
        let row = self.row_height(ui, data);
        let ppp = ui.ctx().pixels_per_point();
        (row * ppp).round() / ppp
    }

    /// Extra gap after a logical line: `(line_spacing - 1) × base row height`.
    fn line_gap(&self, ui: &egui::Ui, data: &LineData) -> f32 {
        (data.line_spacing - 1.0) * self.row_height(ui, data)
    }

    /// Extra gap between soft-wrap continuation rows:
    /// `(wrap_line_spacing - 1) × base row height`.
    fn wrap_gap(&self, ui: &egui::Ui, data: &LineData) -> f32 {
        (data.wrap_line_spacing - 1.0) * self.row_height(ui, data)
    }
}

impl Default for LineCache {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_str(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let chain = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .insert(egui::FontFamily::Name("sans".into()), chain);
        ctx.set_fonts(fonts);
        ctx
    }

    fn data<'a>(buffer: &'a typ_edit_buffer::TextBuffer) -> LineData<'a> {
        LineData {
            buffer,
            text_rev: 1,
            parse_generation: 0,
            highlights: &[],
            blocked_ranges: &[],
            code_ranges: &[],
            math_ranges: &[],
            math_atoms: &[],
            font_family: "sans",
            font_size: 14.0,
            wrap_mode: WrapMode::Off,
            line_spacing: 1.0,
            wrap_line_spacing: 1.0,
            wrap_width: f32::INFINITY,
        }
    }

    #[test]
    fn raw_span_cache_reuses_one_input_and_invalidates_on_change() {
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaaa");
        let one = 0..4;
        let blocked = std::slice::from_ref(&one);
        let first = LineData {
            blocked_ranges: blocked,
            ..data(&buffer)
        };
        let mut cache = LineCache::new();
        let a = cache.raw_spans_for(&first);
        let b = cache.raw_spans_for(&first);
        assert!(
            Arc::ptr_eq(&a, &b),
            "the same input reuses the cached spans"
        );
        assert_eq!(*a, vec![0..4]);

        let second = LineData {
            text_rev: 2,
            ..first
        };
        let c = cache.raw_spans_for(&second);
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a changed input recomputes the raw spans"
        );
    }

    #[test]
    fn layout_epoch_change_drops_every_entry() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaa\nbbb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let first = data(&buffer);
            cache.line_galley(ui, &first, 0);
            cache.line_galley(ui, &first, 1);
            assert_eq!(cache.entry_count(), 2);

            let resized = LineData {
                font_size: 15.0,
                ..first
            };
            cache.line_galley(ui, &resized, 0);
            assert_eq!(
                cache.entry_count(),
                1,
                "a font-size change drops the whole cache"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn font_invalidation_drops_every_shaped_line() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaa\nbbb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.line_galley(ui, &data, 0);
            cache.line_galley(ui, &data, 1);
            assert_eq!(cache.entry_count(), 2);

            // The cached galleys were shaped with the previous font data. A
            // font install must force every line to re-shape even though the
            // family/size epoch is unchanged.
            cache.invalidate_fonts();
            assert_eq!(
                cache.entry_count(),
                0,
                "font invalidation drops every shaped line"
            );
            cache.line_galley(ui, &data, 0);
            assert_eq!(
                cache.entry_count(),
                1,
                "the next query shapes the line with the new fonts"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn text_rev_change_keeps_counts_and_reuses_unchanged_lines() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaa\nbbb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let first = data(&buffer);
            let before = Arc::clone(&cache.line_galley(ui, &first, 0).galley);
            cache.line_galley(ui, &first, 1);
            assert_eq!(cache.entry_count(), 2);

            let second = LineData {
                text_rev: 2,
                ..first
            };
            let after = Arc::clone(&cache.line_galley(ui, &second, 0).galley);
            assert_eq!(
                cache.entry_count(),
                2,
                "a text revision must not drop the entries"
            );
            assert!(
                Arc::ptr_eq(&before, &after),
                "unchanged text reuses the cached galley across revisions"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn spacing_changes_invalidate_the_cache() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaa\nbbb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = data(&buffer);
            cache.line_galley(ui, &base, 0);
            cache.line_galley(ui, &base, 1);
            assert_eq!(cache.entry_count(), 2);

            let spaced = LineData {
                line_spacing: 1.5,
                ..base
            };
            cache.line_galley(ui, &spaced, 0);
            assert_eq!(
                cache.entry_count(),
                1,
                "a line_spacing change drops the cache"
            );

            let wrapped = LineData {
                wrap_line_spacing: 1.5,
                ..base
            };
            cache.line_galley(ui, &wrapped, 0);
            assert_eq!(
                cache.entry_count(),
                1,
                "a wrap_line_spacing change drops the cache"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn line_spacing_adds_gap_after_each_logical_line() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\nb\nc");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = data(&buffer);
            let row = cache.row_height(ui, &base);
            let baseline = cache.line_height(ui, &base, 0);

            let spaced = LineData {
                line_spacing: 1.5,
                ..base
            };
            let grown = cache.line_height(ui, &spaced, 0);
            assert!(
                (grown - baseline - 0.5 * row).abs() < 0.01,
                "height {grown} should be baseline {baseline} + {}",
                0.5 * row
            );

            // The second logical line starts one grown height below the top.
            let visible = cache.visible_lines(
                ui,
                &spaced,
                &super::super::scroll::ScrollAnchor::new(),
                400.0,
                0.0,
            );
            let second_y = visible
                .iter()
                .find(|(line, _, _)| *line == 1)
                .map(|(_, y, _)| *y)
                .expect("line 1 is visible");
            assert!(
                (second_y - grown).abs() < 0.01,
                "second line y {second_y} should advance by the grown height {grown}"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn wrap_line_spacing_stretches_only_between_continuation_rows() {
        let ctx = mono_ctx();
        let buffer =
            typ_edit_buffer::TextBuffer::from_text("aaaa bbbb cccc dddd eeee ffff gggg hhhh");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = LineData {
                wrap_mode: WrapMode::Always,
                wrap_width: mono_char_width(ui) * 12.0,
                ..data(&buffer)
            };
            let row = cache.row_height(ui, &base);
            let baseline = cache.line_height(ui, &base, 0);
            let rows = cache.line_galley(ui, &base, 0).rows;
            assert!(rows >= 2, "the fixture must wrap, got {rows} rows");

            let spaced = LineData {
                wrap_line_spacing: 1.5,
                ..base
            };
            let grown = cache.line_height(ui, &spaced, 0);
            let expected = 0.5 * row * (rows - 1) as f32;
            assert!(
                (grown - baseline - expected).abs() < 1.01,
                "block grew by {} instead of {expected} (rows {rows})",
                grown - baseline
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn default_spacing_adds_no_gap() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\n\nb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let d = data(&buffer);
            let painted = cache.line_galley(ui, &d, 0);
            assert_eq!(
                painted.height,
                painted.galley.size().y,
                "with unit spacing a painted line's height is its galley height"
            );
            let empty = cache.line_galley(ui, &d, 1);
            assert_eq!(empty.rows, 1, "an empty line occupies exactly one row");
            let empty_height = empty.height;
            let row = cache.row_height(ui, &d);
            let ppp = ui.ctx().pixels_per_point();
            let rounded = (row * ppp).round() / ppp;
            assert!(
                (empty_height - rounded).abs() < 0.001,
                "with unit spacing an empty line is one rounded row"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn line_galley_reuses_the_cached_galley_for_the_same_epoch() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aaa\nbbb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let first = Arc::clone(&cache.line_galley(ui, &data, 0).galley);
            let second = Arc::clone(&cache.line_galley(ui, &data, 0).galley);
            assert!(
                Arc::ptr_eq(&first, &second),
                "an unchanged epoch must reuse the cached galley"
            );
            assert_eq!(cache.entry_count(), 1);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn wrapping_produces_multiple_rows() {
        let ctx = test_ctx();
        let buffer =
            typ_edit_buffer::TextBuffer::from_text("aaaa bbbb cccc dddd eeee ffff gggg hhhh");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let wrapped = LineData {
                wrap_mode: WrapMode::Always,
                wrap_width: 40.0,
                ..data(&buffer)
            };
            let rows = cache.line_galley(ui, &wrapped, 0).rows;
            assert!(rows >= 2, "a long line at 40pt must soft-wrap, got {rows}");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn line_text_matches_the_logical_line_without_the_newline() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("你好世界\n第二行\nthird");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let first = cache.line_galley(ui, &data, 0).galley.job.text.clone();
            let second = cache.line_galley(ui, &data, 1).galley.job.text.clone();
            let third = cache.line_galley(ui, &data, 2).galley.job.text.clone();
            assert_eq!(first, "你好世界");
            assert_eq!(second, "第二行");
            assert_eq!(third, "third");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn crlf_line_text_excludes_the_cr_and_newline() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("abc\r\n");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            assert_eq!(
                cache.line_galley(ui, &data, 0).galley.job.text,
                "abc",
                "a CRLF must not leave a CR glyph to wrap"
            );
            assert_eq!(cache.line_galley(ui, &data, 1).galley.job.text, "");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn crlf_code_line_does_not_create_a_blank_wrap_row() {
        let ctx = mono_ctx();
        let text = "#let x = 1\r\n#let y = 2";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let code = [1..10, 12..22];
        let blocked = code.clone();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: &blocked,
                code_ranges: &code,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 8.0,
                ..data(&buffer)
            };
            let mut cache = LineCache::new();
            for line in [0usize, 1] {
                assert_eq!(
                    cache.line_galley(ui, &data, line).rows,
                    1,
                    "CRLF must not add a blank wrapped row on line {line}"
                );
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn trailing_newline_leaves_an_empty_last_line() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("abc\n");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            assert_eq!(cache.line_galley(ui, &data, 0).galley.job.text, "abc");
            let last = cache.line_galley(ui, &data, 1);
            assert_eq!(last.galley.job.text, "");
            assert_eq!(last.rows, 1, "an empty line still occupies one row");
            assert!(last.height > 0.0);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn empty_line_occupies_the_same_height_as_a_painted_row() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\n\nb");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let normal = cache.line_galley(ui, &data, 0).height;
            let empty = cache.line_galley(ui, &data, 1).height;
            let after = cache.line_galley(ui, &data, 2).height;
            assert!(empty > 0.0);
            assert_eq!(
                empty, normal,
                "an empty line must not be a hair taller than a painted one"
            );
            assert_eq!(after, normal);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn visible_lines_advance_by_the_cached_heights_even_across_empty_lines() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("a\n\nb\n\nc");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let visible = cache.visible_lines(
                ui,
                &data,
                &super::super::scroll::ScrollAnchor::new(),
                400.0,
                0.0,
            );
            let mut expected = 0.0f32;
            for (line, y, _) in &visible {
                assert!(
                    (y - expected).abs() < 0.001,
                    "line {line} y {y} != cumulative {expected}"
                );
                expected += cache.line_height(ui, &data, *line);
            }
            assert_eq!(visible.len(), 5, "every line fits in the tall viewport");
        })
        .drop_without_applying_deltas();
    }

    /// Sum over `0..line` of the heights the test measured, falling back to
    /// `fallback` for every line it never measured.
    fn naive_prefix(
        measured: &HashMap<usize, f32>,
        line: usize,
        line_count: usize,
        fallback: f32,
    ) -> f32 {
        (0..line.min(line_count))
            .map(|index| measured.get(&index).copied().unwrap_or(fallback))
            .sum()
    }

    #[test]
    fn prefix_and_total_match_a_naive_sum_of_measured_and_fallback_heights() {
        let ctx = test_ctx();
        let first =
            typ_edit_buffer::TextBuffer::from_text("aaa\nbbbb\ncc\ndddddd\ne\nff\nggggggg\nhh");
        let second = typ_edit_buffer::TextBuffer::from_text(
            "aaa\nbbbb\ncc\ndddddd eeeeee ffffff gggggg\ne\nff\nggggggg\nhh",
        );
        let count = first.line_count();
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = LineData {
                wrap_mode: WrapMode::Always,
                wrap_width: 40.0,
                ..data(&first)
            };
            let fallback = cache.row_height(ui, &base);
            let mut measured = HashMap::new();
            for line in [0usize, 2, 5] {
                measured.insert(line, cache.line_height(ui, &base, line));
            }
            for line in 0..=count {
                let expected = naive_prefix(&measured, line, count, fallback);
                let got = cache.prefix_height(line, count, fallback);
                assert!(
                    (got - expected).abs() < 0.01,
                    "prefix({line}) {got} != naive {expected}"
                );
            }
            let total = cache.total_height(count, fallback);
            let expected = naive_prefix(&measured, count, count, fallback);
            assert!(
                (total - expected).abs() < 0.01,
                "total {total} != {expected}"
            );

            // A content-only edit: line 2 now wraps into more rows, the other
            // measured lines keep their fingerprints and their heights.
            let edited = LineData {
                text_rev: 2,
                wrap_mode: WrapMode::Always,
                wrap_width: 40.0,
                ..data(&second)
            };
            measured.insert(2, cache.line_height(ui, &edited, 2));
            for line in 0..=count {
                let expected = naive_prefix(&measured, line, count, fallback);
                let got = cache.prefix_height(line, count, fallback);
                assert!(
                    (got - expected).abs() < 0.01,
                    "after the height update prefix({line}) {got} != naive {expected}"
                );
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn prefix_tracks_line_count_changes_without_a_whole_clear() {
        let ctx = test_ctx();
        let small = typ_edit_buffer::TextBuffer::from_text("aa\nbb");
        let large = typ_edit_buffer::TextBuffer::from_text("aa\nbb\ncc\ndd\nee\nff");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let first = data(&small);
            let fallback = cache.row_height(ui, &first);
            let mut measured = HashMap::new();
            measured.insert(0, cache.line_height(ui, &first, 0));

            // Growing invalidates the index in O(1); every line measured under
            // the new count registers again, unchanged entries included.
            let grown = data(&large);
            measured.insert(0, cache.line_height(ui, &grown, 0));
            measured.insert(3, cache.line_height(ui, &grown, 3));
            for line in 0..=large.line_count() {
                let expected = naive_prefix(&measured, line, large.line_count(), fallback);
                let got = cache.prefix_height(line, large.line_count(), fallback);
                assert!(
                    (got - expected).abs() < 0.01,
                    "grown prefix({line}) {got} != naive {expected}"
                );
            }

            // Shrinking drops the entries past the new end.
            let shrunk = data(&small);
            let mut after = HashMap::new();
            for line in 0..small.line_count() {
                after.insert(line, cache.line_height(ui, &shrunk, line));
            }
            assert_eq!(
                cache.entry_count(),
                small.line_count(),
                "entries past the shrunken end are dropped"
            );
            for line in 0..=small.line_count() {
                let expected = naive_prefix(&after, line, small.line_count(), fallback);
                let got = cache.prefix_height(line, small.line_count(), fallback);
                assert!(
                    (got - expected).abs() < 0.01,
                    "shrunk prefix({line}) {got} != naive {expected}"
                );
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn far_prefix_queries_do_logarithmic_height_work() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text(&"a\n".repeat(200_000));
        let count = buffer.line_count();
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            let fallback = cache.row_height(ui, &data);
            let _ = cache.line_height(ui, &data, 0);

            cache.reset_height_work();
            let far = cache.prefix_height(150_000, count, fallback);
            assert!(far > 0.0);
            let steps = cache.height_work();
            assert!(
                steps <= 32,
                "a prefix over {count} lines took {steps} tree steps"
            );

            // The scroll model's inverse is the same query plus a binary search,
            // so a far jump also stays logarithmic.
            let mut anchor = super::super::scroll::ScrollAnchor::new();
            cache.reset_height_work();
            let target = 750_000.0;
            anchor.set_abs_y(
                target,
                |line| cache.prefix_height(line, count, fallback),
                count,
                &[],
            );
            let steps = cache.height_work();
            assert!(
                steps <= 1_024,
                "a far set_abs_y took {steps} tree steps for {count} lines"
            );
            let expected = (target / fallback) as usize;
            assert!(
                anchor.top_line.abs_diff(expected) <= 1,
                "landed on line {} instead of {expected}",
                anchor.top_line
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn editing_one_line_keeps_a_distant_entry_and_reshapes_the_edited_line() {
        let ctx = test_ctx();
        let first = typ_edit_buffer::TextBuffer::from_text("hello\nworld\nfoo\nbar\nbaz\nqux");
        let second = typ_edit_buffer::TextBuffer::from_text("HELLO!\nworld\nfoo\nbar\nbaz\nqux");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let before = data(&first);
            let before_edit = Arc::clone(&cache.line_galley(ui, &before, 0).galley);
            let before_far = Arc::clone(&cache.line_galley(ui, &before, 5).galley);

            let edited = LineData {
                text_rev: 2,
                ..data(&second)
            };
            let after_far = Arc::clone(&cache.line_galley(ui, &edited, 5).galley);
            assert!(
                Arc::ptr_eq(&before_far, &after_far),
                "a content-unchanged distant line is reused"
            );
            let after_edit = Arc::clone(&cache.line_galley(ui, &edited, 0).galley);
            assert!(
                !Arc::ptr_eq(&before_edit, &after_edit),
                "the edited line re-shapes"
            );
            assert_eq!(after_edit.job.text, "HELLO!");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn a_parse_change_keeps_lines_whose_formatting_input_is_unchanged() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("aa\nbb");
        let strong = [(0..2, HighlightKind::Strong)];
        let shifted = [(0..1, HighlightKind::Strong)];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = LineData {
                highlights: &strong,
                parse_generation: 1,
                ..data(&buffer)
            };
            let before_far = Arc::clone(&cache.line_galley(ui, &base, 1).galley);
            let before_edit = Arc::clone(&cache.line_galley(ui, &base, 0).galley);

            let next = LineData {
                highlights: &shifted,
                parse_generation: 2,
                ..base
            };
            let after_far = Arc::clone(&cache.line_galley(ui, &next, 1).galley);
            assert!(
                Arc::ptr_eq(&before_far, &after_far),
                "line 1's clipped formatting input did not change"
            );
            let after_edit = Arc::clone(&cache.line_galley(ui, &next, 0).galley);
            assert!(
                !Arc::ptr_eq(&before_edit, &after_edit),
                "line 0's highlight input changed"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn an_over_budget_placeholder_is_reused_not_recreated() {
        let ctx = test_ctx();
        let buffer =
            typ_edit_buffer::TextBuffer::from_text(&format!("{}\nsecond", "a".repeat(50_000)));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.set_budget(1);
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            let first = Arc::clone(&cache.line_galley(ui, &data, 0).galley);
            assert!(cache.line_galley(ui, &data, 0).placeholder);
            assert!(cache.budget_hit());
            let second = Arc::clone(&cache.line_galley(ui, &data, 0).galley);
            assert!(
                Arc::ptr_eq(&first, &second),
                "an over-budget placeholder is reused, not re-created"
            );

            cache.set_budget(usize::MAX);
            cache.begin_frame();
            assert!(
                !cache.line_galley(ui, &data, 0).placeholder,
                "with budget available the placeholder shapes"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn long_line_shapes_only_a_bounded_window() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text(&"a".repeat(1_000_000));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            let cached = cache.line_galley(ui, &data, 0);
            assert!(cached.windowed, "a 1 MB line must be windowed");
            assert_eq!(cached.window_start, 0);
            assert!(
                cached.window_len < 10_000,
                "the window must be bounded, got {}",
                cached.window_len
            );
            assert!(cached.height > 0.0);
            assert!(cache.shaped_chars_this_frame() < 10_000);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn scrolling_past_the_window_reshapes_a_bounded_window() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text(&"a".repeat(1_000_000));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            let first = cache.line_galley(ui, &data, 0).window_start;

            cache.begin_frame();
            cache.set_horizontal(4_000_000.0, 800.0);
            let cached = cache.line_galley(ui, &data, 0);
            assert!(
                cached.window_start > first,
                "the window moved with h_offset"
            );
            assert!(cached.window_len < 10_000);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn wrap_mode_still_renders_a_long_line_as_one_row() {
        let ctx = test_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text(&"a".repeat(1_000_000));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                wrap_mode: WrapMode::Always,
                wrap_width: 40.0,
                ..data(&buffer)
            };
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            let cached = cache.line_galley(ui, &data, 0);
            assert_eq!(cached.rows, 1, "long lines never wrap");
            assert!(cached.windowed);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_wraps_an_all_markup_line() {
        let ctx = test_ctx();
        let buffer =
            typ_edit_buffer::TextBuffer::from_text("aaaa bbbb cccc dddd eeee ffff gggg hhhh");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                wrap_mode: WrapMode::Markup,
                wrap_width: 40.0,
                ..data(&buffer)
            };
            let rows = cache.line_galley(ui, &data, 0).rows;
            assert!(rows >= 2, "an all-markup line wraps, got {rows} rows");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_keeps_a_blocked_line_single_row() {
        let ctx = test_ctx();
        let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let blocked_range = 0..text.len();
        let blocked = std::slice::from_ref(&blocked_range);
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: blocked,
                wrap_mode: WrapMode::Markup,
                wrap_width: 40.0,
                ..data(&buffer)
            };
            assert_eq!(
                cache.line_galley(ui, &data, 0).rows,
                1,
                "a code/math/raw line stays one row under Markup"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_wraps_only_lines_with_a_markup_body() {
        let ctx = test_ctx();
        let text = "aaaa bbbb cccc dddd eeee\nffff gggg hhhh iiii jjjj";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let newline = text.find('\n').expect("two lines");
        let blocked_range = 0..newline;
        let blocked = std::slice::from_ref(&blocked_range);
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = markup_data(&buffer, blocked);
            assert_eq!(
                cache.line_galley(ui, &data, 0).rows,
                1,
                "line 0 is pure code"
            );
            assert!(
                cache.line_galley(ui, &data, 1).rows >= 2,
                "the markup line wraps"
            );
        })
        .drop_without_applying_deltas();
    }

    fn markup_data<'a>(
        buffer: &'a typ_edit_buffer::TextBuffer,
        blocked: &'a [Range<usize>],
    ) -> LineData<'a> {
        LineData {
            blocked_ranges: blocked,
            wrap_mode: WrapMode::Markup,
            wrap_width: 40.0,
            ..data(buffer)
        }
    }

    #[test]
    fn markup_mode_wraps_a_fully_bold_line() {
        let ctx = test_ctx();
        let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let strong = [(0..text.len(), HighlightKind::Strong)];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                highlights: &strong,
                ..markup_data(&buffer, &[])
            };
            assert!(
                cache.line_galley(ui, &data, 0).rows >= 2,
                "styled spans are transparent to wrapping"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_wraps_a_content_block_line() {
        let ctx = test_ctx();
        let text = "#figure([aaaa bbbb cccc dddd eeee ffff gggg hhhh])";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let blocked = [0..8, text.len() - 1..text.len()];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = markup_data(&buffer, &blocked);
            let rows = cache.line_galley(ui, &data, 0).rows;
            assert!(rows >= 2, "the content block wraps, got {rows} rows");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_keeps_code_arguments_whole_while_wrapping_content() {
        let ctx = mono_ctx();
        let text = "#foo(argument_one, argument_two, [aaaa bbbb cccc dddd eeee])";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let code = [1..33, 59..60];
        let blocked = code.clone();
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: &blocked,
                code_ranges: &code,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 20.0,
                ..data(&buffer)
            };
            let galley = cache.line_galley(ui, &data, 0).galley.clone();
            assert!(galley.rows.len() >= 2, "the content block wraps");
            let mut boundary = 0usize;
            for row in &galley.rows {
                assert_ne!(boundary, 1, "the leading # must not be stranded");
                assert!(
                    boundary == 0
                        || code
                            .iter()
                            .all(|range| boundary <= range.start || boundary >= range.end),
                    "the function arguments stay whole: row starts at {boundary:?}"
                );
                boundary += row.glyphs.len();
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_never_wraps_a_pure_code_line() {
        let ctx = test_ctx();
        let text = "#set text(size: 10pt) aaaa bbbb cccc dddd eeee ffff";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let all_range = 0..text.len();
        let all = std::slice::from_ref(&all_range);
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = markup_data(&buffer, all);
            assert_eq!(
                cache.line_galley(ui, &data, 0).rows,
                1,
                "a pure code line never wraps"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_does_not_leave_indentation_before_code_on_its_own_row() {
        let ctx = mono_ctx();
        let text = "    #foo(argument_one, argument_two)    ";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let code_range = 5..text.trim_end().len();
        let code = std::slice::from_ref(&code_range);
        let blocked = code;
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: blocked,
                code_ranges: code,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 8.0,
                ..data(&buffer)
            };
            assert_eq!(
                LineCache::new().line_galley(ui, &data, 0).rows,
                1,
                "leading indentation/trailing spaces must travel with the code atom"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_keeps_a_real_code_line_single_row() {
        let ctx = mono_ctx();
        let text = "#foo(argument_one, argument_two, argument_three, argument_four)";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let code_range = 1..text.len();
        let code = std::slice::from_ref(&code_range);
        let blocked = code;
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: blocked,
                code_ranges: code,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 20.0,
                ..data(&buffer)
            };
            assert_eq!(
                LineCache::new().line_galley(ui, &data, 0).rows,
                1,
                "a code-only function line must not wrap its arguments"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_wraps_a_mixed_line_and_may_split_math() {
        let ctx = test_ctx();
        let text = "aaa $xxxxxxxxxxxxxxxxxxxx$ bbb";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let math = text.find('$').expect("math")..text.rfind('$').expect("math") + 1;
        let blocked = [math.clone()];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = markup_data(&buffer, &blocked);
            let galley = cache.line_galley(ui, &data, 0).galley.clone();
            assert!(galley.rows.len() >= 2, "the mixed line wraps");
            let mut boundary = 0usize;
            let split_inside_math = galley.rows.iter().any(|row| {
                boundary += row.glyphs.len();
                math.start < boundary && boundary < math.end
            });
            assert!(
                split_inside_math,
                "math may be split when the width forces it"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn budget_skips_lines_and_flags_a_repaint() {
        let ctx = test_ctx();
        let buffer =
            typ_edit_buffer::TextBuffer::from_text(&format!("{}\nsecond", "a".repeat(50_000)));
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = data(&buffer);
            cache.set_budget(1);
            cache.begin_frame();
            cache.set_horizontal(0.0, 800.0);
            assert!(cache.line_galley(ui, &data, 0).placeholder);
            assert!(cache.budget_hit(), "the caller must repaint");
            assert_eq!(cache.shaped_chars_this_frame(), 0);

            cache.set_budget(usize::MAX);
            cache.begin_frame();
            assert!(!cache.line_galley(ui, &data, 0).placeholder);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn raw_spans_subtract_code_and_math() {
        assert_eq!(
            raw_spans(
                std::slice::from_ref(&(0..100)),
                std::slice::from_ref(&(10..20)),
                std::slice::from_ref(&(30..40))
            ),
            vec![0..10, 20..30, 40..100]
        );
        assert_eq!(
            raw_spans(
                std::slice::from_ref(&(0..50)),
                &[],
                std::slice::from_ref(&(0..50))
            ),
            Vec::<Range<usize>>::new()
        );
        assert_eq!(
            raw_spans(
                std::slice::from_ref(&(0..50)),
                std::slice::from_ref(&(0..10)),
                std::slice::from_ref(&(40..60))
            ),
            vec![(10..40)]
        );
        assert_eq!(
            raw_spans(&[], std::slice::from_ref(&(0..10)), &[]),
            Vec::<Range<usize>>::new()
        );
        assert_eq!(
            raw_spans(&[0..20, 30..50], std::slice::from_ref(&(5..35)), &[]),
            vec![0..5, 35..50]
        );
        assert_eq!(
            raw_spans(
                std::slice::from_ref(&(0..100)),
                &[10..40, 50..60],
                std::slice::from_ref(&(20..70))
            ),
            vec![0..10, 70..100]
        );
    }

    /// A context whose "sans" family is monospace, for exact wrap widths.
    fn mono_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let chain = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
        fonts
            .families
            .insert(egui::FontFamily::Name("sans".into()), chain);
        ctx.set_fonts(fonts);
        ctx
    }

    /// Advance width of one test-font char, used to pick exact row widths.
    fn mono_char_width(ui: &egui::Ui) -> f32 {
        let mut job = egui::text::LayoutJob::simple(
            "a".to_owned(),
            egui::FontId::new(14.0, egui::FontFamily::Name("sans".into())),
            egui::Color32::WHITE,
            f32::INFINITY,
        );
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        galley.rows[0].glyphs[0].advance_width
    }

    fn row_starts(galley: &Arc<egui::Galley>) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut at = 0;
        for row in &galley.rows {
            starts.push(at);
            at += row.glyphs.len();
        }
        starts
    }

    #[test]
    fn markup_mode_moves_a_raw_atom_whole_to_the_next_row() {
        let ctx = mono_ctx();
        let text = "aaaa `a b c d e` tail";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let raw = text.find('`').expect("raw")..text.rfind('`').expect("raw") + 1;
        let blocked = [raw.clone()];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: &blocked,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 14.0,
                ..data(&buffer)
            };
            let galley = cache.line_galley(ui, &data, 0).galley.clone();
            assert!(galley.rows.len() >= 2, "the line wraps");
            let starts = row_starts(&galley);
            assert_eq!(starts[1], raw.start, "the atom starts the second row");
            assert!(
                starts
                    .iter()
                    .all(|start| !(raw.start < *start && *start < raw.end)),
                "no row starts inside the raw atom: {starts:?} vs {raw:?}"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_moves_a_math_atom_whole_to_the_next_row() {
        let ctx = mono_ctx();
        let text = "aa $sqrt(x)$ tail tail";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let math = text.find('$').expect("math")..text.rfind('$').expect("math") + 1;
        let blocked = [math.clone()];
        let atoms = [math.clone()];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: &blocked,
                math_ranges: &blocked,
                math_atoms: &atoms,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 10.0,
                ..data(&buffer)
            };
            let galley = cache.line_galley(ui, &data, 0).galley.clone();
            assert!(galley.rows.len() >= 2, "the line wraps");
            let starts = row_starts(&galley);
            assert_eq!(starts[1], math.start, "the atom starts the second row");
            assert!(
                starts
                    .iter()
                    .all(|start| !(math.start < *start && *start < math.end)),
                "no row starts inside the math atom: {starts:?} vs {math:?}"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn markup_mode_degrades_an_oversized_raw_to_char_splits() {
        let ctx = mono_ctx();
        let text = format!("x `{}` y", "a".repeat(40));
        let buffer = typ_edit_buffer::TextBuffer::from_text(&text);
        let raw = text.find('`').expect("raw")..text.rfind('`').expect("raw") + 1;
        let blocked = [raw];
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let data = LineData {
                blocked_ranges: &blocked,
                wrap_mode: WrapMode::Markup,
                wrap_width: mono_char_width(ui) * 8.0,
                ..data(&buffer)
            };
            let galley = cache.line_galley(ui, &data, 0).galley.clone();
            assert!(galley.rows.len() >= 2, "an oversized raw still wraps");
            let joined: String = galley
                .rows
                .iter()
                .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
                .collect();
            assert_eq!(joined, text, "no glyph is lost");
            assert!(
                galley.rows.iter().all(|row| !row.glyphs.is_empty()),
                "every row makes progress"
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn merge_ranges_keeps_adjacent_atoms_separate() {
        assert_eq!(merge_ranges(vec![0..2, 2..4, 3..5]), vec![0..2, 2..5]);
        assert_eq!(merge_ranges(vec![1..3, 3..4]), vec![1..3, 3..4]);
    }

    #[test]
    fn kinsoku_atoms_cover_the_terminal_set() {
        for punct in ['；', '：', '、', '，', '。', '！', '？', '—', '”', '）'] {
            let text = format!("甲{punct}乙");
            assert_eq!(kinsoku_atoms(&text), vec![0..2], "{punct}");
        }
    }

    #[test]
    fn kinsoku_atoms_exclude_opening_quotes_and_brackets() {
        assert!(kinsoku_atoms("甲“乙（丙").is_empty());
    }

    #[test]
    fn kinsoku_atoms_merge_a_double_em_dash() {
        assert_eq!(merge_ranges(kinsoku_atoms("甲——乙")), vec![0..3]);
    }

    #[test]
    fn line_wrap_ranges_add_kinsoku_atoms() {
        let buffer = typ_edit_buffer::TextBuffer::from_text("你好，世界");
        let text = buffer.slice(0..5);
        let (atoms, _) =
            line_wrap_ranges(&data(&buffer), &buffer, 0, buffer.char_to_byte(5), 0, &text);
        assert_eq!(atoms, vec![1..3], "好 and ， are one atom");
    }

    #[test]
    fn line_wrap_ranges_preserve_adjacent_math_atoms() {
        let text = "sqrt(2)";
        let buffer = typ_edit_buffer::TextBuffer::from_text(text);
        let math_atoms = [0..5, 5..6, 6..7];
        let data = LineData {
            math_atoms: &math_atoms,
            ..data(&buffer)
        };
        let (atoms, _) = line_wrap_ranges(&data, &buffer, 0, buffer.char_to_byte(7), 0, text);
        assert_eq!(
            atoms,
            vec![0..5, 5..6, 6..7],
            "adjacent math atoms must not merge"
        );
    }

    #[test]
    fn kinsoku_keeps_punctuation_off_the_row_start() {
        let ctx = mono_ctx();
        let buffer = typ_edit_buffer::TextBuffer::from_text("你好，世界");
        let mut cache = LineCache::new();
        ctx.run_ui(Default::default(), |ui| {
            let base = data(&buffer);
            let unwrapped = cache.line_galley(ui, &base, 0).galley.clone();
            let glyphs = &unwrapped.rows[0].glyphs;
            let width = (glyphs[1].max_x() + glyphs[2].max_x()) / 2.0;
            let wrapped = LineData {
                wrap_mode: WrapMode::Always,
                wrap_width: width,
                ..base
            };
            let galley = cache.line_galley(ui, &wrapped, 0).galley.clone();
            let rows: Vec<String> = galley
                .rows
                .iter()
                .map(|row| row.glyphs.iter().map(|glyph| glyph.chr).collect())
                .collect();
            assert!(rows.len() >= 2, "the line wraps: {rows:?}");
            assert!(!rows[0].ends_with('好'), "the pair moved: {rows:?}");
            assert!(
                !rows.iter().any(|row| row.starts_with('，')),
                "no row starts with kinsoku: {rows:?}"
            );
        })
        .drop_without_applying_deltas();
    }
}
