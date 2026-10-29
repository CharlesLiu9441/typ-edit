//! Spell worker: prose spell checking over the parse worker's segments.
//!
//! The parse worker extracts one [`ProseSegment`] per contiguous run of markup
//! prose; this worker lints each segment with a custom harper `Parser`
//! ([`SegmentParser`]) that turns prose letters into words and marks inline
//! non-prose (`holes`: math, raw, refs, `#expr`) plus CJK runs as `Unlintable`.
//!
//! Results are cached **per tab, keyed by segment content hash**. An edit that
//! shifts a paragraph without changing it leaves the hash intact, so the cached
//! issues are reused and rebased to the segment's new start; only edited or new
//! segments are re-linted. This gives the same outcome as shifting the cache by
//! an edit offset — untouched paragraphs keep their results, edited ones
//! refresh — while making stale-offset reuse impossible: a cache entry can only
//! ever be served for exactly the content it was computed from.
//!
//! The worker coalesces to the latest [`SpellRequest`], mirroring the parse
//! worker, so a burst of keystrokes costs at most one spell pass.

// allow: SIZE_OK — one cohesive spell worker (segment parser, content-hash
// cache and single-thread spawn are inseparable here); the file is a single
// public surface `spawn` and its private helpers.

use std::thread::JoinHandle;

use crate::latest::LatestReceiver;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crossbeam_channel::Sender;
use harper_core::linting::{LintGroup, LintKind, Linter, Suggestion};
use harper_core::parsers::Parser;
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document, Number, Punctuation, Span, Token, TokenKind};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::ops::Range;
use typ_edit_model::{BytePos, ProseSegment, SpellIssue, SpellRequest, SpellResult, is_cjk};

/// One segment's cached lint: the content hash that keys it and its issues with
/// ranges relative to the segment start, so reuse just adds the current start.
pub(crate) struct CachedSegment {
    hash: u64,
    issues: Vec<CachedIssue>,
}

/// A cached issue in segment-relative bytes.
struct CachedIssue {
    rel: Range<usize>,
    word: String,
    suggestions: Vec<String>,
}

/// Builds the spell-only linter: every rule off but `SpellCheck`, over the
/// curated dictionary. Built once per worker thread and reused for every
/// segment; rebuilding it per segment would reload the dictionary each time.
fn build_linter() -> LintGroup {
    let mut linter = LintGroup::new_curated(FstDictionary::curated(), Dialect::American);
    linter.set_all_rules_to(Some(false));
    linter.config.set_rule_enabled("SpellCheck", true);
    linter
}

/// Classifies one prose segment into harper tokens so the spell linter sees
/// prose words and nothing else. `holes` are CHAR indices into the segment
/// slice covering inline non-prose.
struct SegmentParser {
    holes: Vec<Range<usize>>,
}

impl Parser for SegmentParser {
    fn parse(&self, source: &[char]) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut i = 0usize;
        let mut hole = 0usize;
        while i < source.len() {
            // A hole spanning `i` wins over any character classification. Holes
            // are sorted and merged, so one cursor suffices.
            while hole < self.holes.len() && self.holes[hole].end <= i {
                hole += 1;
            }
            if let Some(h) = self.holes.get(hole)
                && h.start <= i
                && i < h.end
            {
                tokens.push(Token::new(Span::new(h.start, h.end), TokenKind::Unlintable));
                i = h.end;
                continue;
            }
            let c = source[i];
            if is_cjk(c) {
                let start = i;
                while i < source.len() && is_cjk(source[i]) {
                    i += 1;
                }
                tokens.push(Token::new(Span::new(start, i), TokenKind::Unlintable));
            } else if c.is_alphabetic() || is_apostrophe(c) {
                let start = i;
                while i < source.len() && (source[i].is_alphabetic() || is_apostrophe(source[i])) {
                    i += 1;
                }
                let mut end = i;
                while end > start && is_apostrophe(source[end - 1]) {
                    end -= 1;
                }
                if end > start {
                    tokens.push(Token::new(Span::new(start, end), TokenKind::Word(None)));
                    i = end;
                } else {
                    // A run of only apostrophes is punctuation, not a word.
                    tokens.push(Token::new(
                        Span::new(start, start + 1),
                        punctuation_token(source[start]),
                    ));
                    i = start + 1;
                }
            } else if c.is_ascii_digit() {
                let start = i;
                while i < source.len() && source[i].is_ascii_digit() {
                    i += 1;
                }
                tokens.push(Token::new(
                    Span::new(start, i),
                    TokenKind::Number(Number::default()),
                ));
            } else if c == '\n' {
                let start = i;
                while i < source.len() && source[i] == '\n' {
                    i += 1;
                }
                tokens.push(Token::new(
                    Span::new(start, i),
                    TokenKind::Newline(i - start),
                ));
            } else if c.is_whitespace() {
                let start = i;
                while i < source.len() && source[i].is_whitespace() && source[i] != '\n' {
                    i += 1;
                }
                tokens.push(Token::new(Span::new(start, i), TokenKind::Space(i - start)));
            } else {
                tokens.push(Token::new(Span::new(i, i + 1), punctuation_token(c)));
                i += 1;
            }
        }
        tokens
    }
}

/// A punctuation token for `c`; characters harper does not classify (stray
/// markers) fall back to `Unlintable`. Both are ignored by `SpellCheck`.
fn punctuation_token(c: char) -> TokenKind {
    match Punctuation::from_char(c) {
        Some(punct) => TokenKind::Punctuation(punct),
        None => TokenKind::Unlintable,
    }
}

/// Both the ASCII and typographic apostrophe are inner word characters.
fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}'
}

/// Upper bound on the per-tab caches the worker retains. When a new tab's cache
/// would push the map past this, the smallest (oldest, monotonically assigned)
/// tab id is evicted.
const MAX_CACHED_TABS: usize = 64;

/// Drops the smallest (oldest) tab caches until at most `cap` remain. Ids are
/// monotonic and never reused, so the smallest id is the oldest tab.
fn evict_oldest_tabs(caches: &mut HashMap<u64, Vec<CachedSegment>>, cap: usize) {
    while caches.len() > cap {
        let Some(oldest) = caches.keys().min().copied() else {
            break;
        };
        caches.remove(&oldest);
    }
}

/// Drops `tab`'s cached lint results.
fn forget_tab(caches: &mut HashMap<u64, Vec<CachedSegment>>, tab: u64) {
    caches.remove(&tab);
}

/// A merged spell mailbox slot: the latest `Check` to run plus every `Forget`
/// accumulated since the worker last drained the slot, in send order.
#[derive(Default)]
pub(crate) struct SpellMsg {
    check: Option<SpellRequest>,
    forgets: Vec<u64>,
}

/// Folds one request into the pending [`SpellMsg`] in send order.
///
/// `Check` replaces the pending check (latest wins). `Forget{tab}` is recorded,
/// and when it arrives *after* a pending check for the same tab it also cancels
/// that check, so a tab closed in the same burst is never spell-checked. A
/// `Forget` arriving *before* a newer check does not cancel it, but is still
/// applied to the worker's cache ahead of that check.
pub(crate) fn merge_spell_request(msg: &mut SpellMsg, request: SpellRequest) {
    match request {
        SpellRequest::Forget { tab } => {
            msg.forgets.push(tab);
            let cancels = matches!(
                &msg.check,
                Some(SpellRequest::Check { tab: pending, .. }) if *pending == tab
            );
            if cancels {
                msg.check = None;
            }
        }
        check @ SpellRequest::Check { .. } => {
            msg.check = Some(check);
        }
    }
}

/// Applies one request to the worker's cache and linter state. A `Check` returns
/// the result to publish; a `Forget` drops the tab's cache and returns `None`.
fn apply_request(
    request: SpellRequest,
    caches: &mut HashMap<u64, Vec<CachedSegment>>,
    linter: &mut LintGroup,
) -> Option<SpellResult> {
    let (generation, tab, text, segments) = match request {
        SpellRequest::Forget { tab } => {
            forget_tab(caches, tab);
            return None;
        }
        SpellRequest::Check {
            generation,
            tab,
            text,
            segments,
        } => (generation, tab, text, segments),
    };

    let issues = {
        // Materialize on this worker thread; the UI thread only shipped a handle.
        let text = text.materialize();
        let cache = caches.entry(tab).or_default();
        run(text.as_str(), segments.as_ref(), cache, linter).0
    };
    evict_oldest_tabs(caches, MAX_CACHED_TABS);
    Some(SpellResult {
        generation,
        tab,
        issues,
    })
}

/// Spawns the spell worker thread. Keeps a per-tab content-hash cache plus one
/// shared linter across restarts (both are content-keyed and therefore safe to
/// retain), and consumes the already-merged [`SpellMsg`] stream.
pub fn spawn(
    req: LatestReceiver<SpellMsg>,
    res_tx: Sender<SpellResult>,
    waker: Waker,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-spell".to_owned())
        .spawn(move || {
            let mut linter = build_linter();
            let mut caches: HashMap<u64, Vec<CachedSegment>> = HashMap::new();
            supervise(Pipeline::Spell, &health, || {
                while let Ok(msg) = req.recv() {
                    // Forgets apply first, in send order; the cancellation of a
                    // same-tab pending check already happened at merge time.
                    for tab in msg.forgets {
                        forget_tab(&mut caches, tab);
                    }
                    let Some(check) = msg.check else {
                        continue;
                    };
                    let Some(result) = apply_request(check, &mut caches, &mut linter) else {
                        continue;
                    };
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    waker.wake();
                }
            });
        })
        .expect("spawn spell worker")
}

/// Lints every segment, reusing cached results for segments whose content hash
/// is unchanged. Returns the absolute-ranged issues (sorted by start) and how
/// many segments were actually re-linted.
///
/// An edit that shifts a paragraph without changing it leaves its hash intact,
/// so the cached issues are reused and rebased to the new segment start.
/// Duplicate segments consume one cache entry each, so an extra copy is
/// re-linted rather than sharing a slot. Entries not consumed this request are
/// dropped.
pub fn run(
    text: &str,
    segments: &[ProseSegment],
    cache: &mut Vec<CachedSegment>,
    linter: &mut LintGroup,
) -> (Vec<SpellIssue>, usize) {
    let mut pool: Vec<Option<CachedSegment>> =
        std::mem::take(cache).into_iter().map(Some).collect();
    let mut fresh: Vec<CachedSegment> = Vec::with_capacity(segments.len());
    let mut issues: Vec<SpellIssue> = Vec::new();
    let mut rechecked = 0usize;

    for segment in segments {
        let Some(slice) = segment_slice(text, segment) else {
            continue;
        };
        let hash = hash_slice(slice);
        let taken = pool.iter_mut().find_map(|entry| {
            if entry.as_ref().is_some_and(|cached| cached.hash == hash) {
                entry.take()
            } else {
                None
            }
        });

        if let Some(entry) = taken {
            for cached in &entry.issues {
                issues.push(SpellIssue {
                    range: BytePos(segment.range.start.0 + cached.rel.start)
                        ..BytePos(segment.range.start.0 + cached.rel.end),
                    word: cached.word.clone(),
                    suggestions: cached.suggestions.clone(),
                });
            }
            fresh.push(entry);
        } else {
            rechecked += 1;
            let linted = lint_segment(text, segment, linter);
            let base = segment.range.start.0;
            fresh.push(CachedSegment {
                hash,
                issues: linted
                    .iter()
                    .map(|issue| CachedIssue {
                        rel: issue.range.start.0 - base..issue.range.end.0 - base,
                        word: issue.word.clone(),
                        suggestions: issue.suggestions.clone(),
                    })
                    .collect(),
            });
            issues.extend(linted);
        }
    }

    *cache = fresh;
    issues.sort_by_key(|issue| issue.range.start);
    (issues, rechecked)
}

/// Lints `text`'s segments with a throwaway linter and no cache. Test-only:
/// the worker always routes through the cached [`run`].
#[cfg(test)]
pub fn run_stateless(text: &str, segments: &[ProseSegment]) -> Vec<SpellIssue> {
    let mut linter = build_linter();
    run(text, segments, &mut Vec::new(), &mut linter).0
}

/// Lints one prose segment, mapping harper's char spans back to absolute byte
/// ranges in `text`. Invalid segments yield no issues rather than panicking.
fn lint_segment(text: &str, segment: &ProseSegment, linter: &mut LintGroup) -> Vec<SpellIssue> {
    let Some(slice) = segment_slice(text, segment) else {
        return Vec::new();
    };
    let table = char_to_byte_table(slice);
    let char_count = table.len().saturating_sub(1);
    let holes = relative_char_holes(segment, &table, char_count);
    let document = Document::new_curated(slice, &SegmentParser { holes });
    let chars = document.get_source();

    let mut issues = Vec::new();
    for lint in linter.lint(&document) {
        if lint.lint_kind != LintKind::Spelling {
            continue;
        }
        let (Some(start), Some(end)) = (
            table.get(lint.span.start).copied(),
            table.get(lint.span.end).copied(),
        ) else {
            continue;
        };
        if start >= end || end > slice.len() {
            continue;
        }
        let word = lint.get_str(chars);
        let suggestions = lint
            .suggestions
            .iter()
            .filter_map(|suggestion| match suggestion {
                Suggestion::ReplaceWith(replacement) => {
                    Some(replacement.iter().collect::<String>())
                }
                _ => None,
            })
            .collect();
        issues.push(SpellIssue {
            range: BytePos(segment.range.start.0 + start)..BytePos(segment.range.start.0 + end),
            word,
            suggestions,
        });
    }
    issues
}

/// Borrows the segment's slice, or `None` when its byte range is out of bounds
/// or lands off a char boundary. Invalid segments are skipped, never allowed to
/// panic the worker.
fn segment_slice<'a>(text: &'a str, segment: &ProseSegment) -> Option<&'a str> {
    let start = segment.range.start.0;
    let end = segment.range.end.0;
    if start > end || end > text.len() {
        return None;
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some(&text[start..end])
}

/// Converts the segment's absolute byte holes to char-index holes relative to
/// the slice, dropping any that fall outside or off a char boundary, then
/// sorting and merging so the parser sees sorted, non-overlapping holes.
fn relative_char_holes(
    segment: &ProseSegment,
    table: &[usize],
    char_count: usize,
) -> Vec<Range<usize>> {
    let base = segment.range.start.0;
    let mut converted: Vec<Range<usize>> = Vec::new();
    for hole in &segment.holes {
        if hole.start.0 < base || hole.end.0 > segment.range.end.0 || hole.start >= hole.end {
            continue;
        }
        let (Some(start), Some(end)) = (
            byte_to_char(table, hole.start.0 - base),
            byte_to_char(table, hole.end.0 - base),
        ) else {
            continue;
        };
        let start = start.min(char_count);
        let end = end.min(char_count);
        if start < end {
            converted.push(start..end);
        }
    }
    converted.sort_by_key(|hole| hole.start);

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(converted.len());
    for hole in converted {
        match merged.last_mut() {
            Some(last) if hole.start <= last.end => last.end = last.end.max(hole.end),
            _ => merged.push(hole),
        }
    }
    merged
}

/// Maps a byte offset within the slice to its char index; `None` when the byte
/// falls inside a multi-byte char. The table is strictly increasing (one entry
/// per char plus a `len` sentinel), so a boundary offset resolves exactly.
fn byte_to_char(table: &[usize], byte: usize) -> Option<usize> {
    table.binary_search(&byte).ok()
}

/// Content identity of a segment: a hash of its slice bytes. Identical content
/// hashes equal wherever the segment sits, so an untouched paragraph hits the
/// cache after an edit shifts it.
fn hash_slice(slice: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(slice.as_bytes());
    hasher.finish()
}

/// Maps every char index of `text` to its byte offset, plus a final sentinel at
/// `text.len()` so an end-exclusive char offset resolves without a clamp.
fn char_to_byte_table(text: &str) -> Vec<usize> {
    let mut table: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    table.push(text.len());
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use typ_edit_model::TextSnapshot;

    fn segment(range: Range<usize>, holes: Vec<Range<usize>>) -> ProseSegment {
        ProseSegment {
            range: BytePos(range.start)..BytePos(range.end),
            holes: holes
                .into_iter()
                .map(|hole| BytePos(hole.start)..BytePos(hole.end))
                .collect(),
        }
    }

    fn check_request(tab: u64, text: &str, segments: Arc<Vec<ProseSegment>>) -> SpellRequest {
        SpellRequest::Check {
            generation: 1,
            tab,
            text: TextSnapshot::from_owned(text, 1),
            segments,
        }
    }

    #[test]
    fn misspelling_in_a_plain_segment_is_reported_with_suggestions() {
        let text = "This is teh best.";
        let segments = vec![segment(0..text.len(), Vec::new())];
        let issues = run_stateless(text, &segments);
        assert_eq!(issues.len(), 1, "exactly one misspelling: {issues:?}");
        let issue = &issues[0];
        assert_eq!(issue.word, "teh");
        let start = text.find("teh").unwrap();
        assert_eq!(issue.range, BytePos(start)..BytePos(start + 3));
        assert!(
            issue.suggestions.iter().any(|s| s == "the"),
            "suggestions must contain `the`: {:?}",
            issue.suggestions
        );
    }

    #[test]
    fn holes_are_unlintable_and_never_flagged() {
        let text = "Text with teh $wrld$ and `code mispelled` done.";
        let math = text.find("$wrld$").unwrap()..text.find("$wrld$").unwrap() + "$wrld$".len();
        let raw_start = text.find('`').unwrap();
        let raw = raw_start..text.rfind('`').unwrap() + 1;
        let segments = vec![segment(0..text.len(), vec![math, raw])];
        let issues = run_stateless(text, &segments);
        assert_eq!(
            issues.len(),
            1,
            "only the prose typo is flagged: {issues:?}"
        );
        assert_eq!(issues[0].word, "teh");
        assert!(
            issues
                .iter()
                .all(|issue| issue.word != "wrld" && issue.word != "mispelled"),
            "hole contents must never be flagged: {issues:?}"
        );
    }

    #[test]
    fn cjk_runs_are_unlintable_and_only_latin_typos_are_reported() {
        let text = "测试 teh 结果";
        let segments = vec![segment(0..text.len(), Vec::new())];
        let issues = run_stateless(text, &segments);
        assert_eq!(issues.len(), 1, "only `teh` is misspelled: {issues:?}");
        let start = text.find("teh").unwrap();
        assert_eq!(issues[0].range, BytePos(start)..BytePos(start + 3));
        assert_eq!(issues[0].word, "teh");
    }

    #[test]
    fn pure_cjk_segment_yields_no_issues() {
        let text = "你好世界";
        let segments = vec![segment(0..text.len(), Vec::new())];
        assert!(run_stateless(text, &segments).is_empty());
    }

    #[test]
    fn numbers_are_not_spelled_as_words() {
        let text = "The value is 42 and 3.14.";
        let segments = vec![segment(0..text.len(), Vec::new())];
        assert!(run_stateless(text, &segments).is_empty());
    }

    #[test]
    fn empty_and_clean_segments_yield_no_issues() {
        let empty = vec![segment(0..0, Vec::new())];
        assert!(run_stateless("", &empty).is_empty());
        let text = "This is the best.";
        let segments = vec![segment(0..text.len(), Vec::new())];
        assert!(run_stateless(text, &segments).is_empty());
    }

    #[test]
    fn rerunning_identical_segments_rechecks_nothing() {
        let text = "This is teh best.";
        let segments = vec![segment(0..text.len(), Vec::new())];
        let mut cache = Vec::new();
        let mut linter = build_linter();
        let (first, rechecked) = run(text, &segments, &mut cache, &mut linter);
        assert_eq!(first.len(), 1);
        assert_eq!(rechecked, 1, "the first run lints the only segment");
        let (second, rechecked) = run(text, &segments, &mut cache, &mut linter);
        assert_eq!(rechecked, 0, "identical content is served from cache");
        assert_eq!(second, first, "cached issues rebase identically");
    }

    #[test]
    fn editing_one_segment_rechecks_only_that_segment() {
        let text = "teh cat sat.\n\nthe dog wrold.";
        let split = text.find("\n\n").unwrap() + 2;
        let segments = vec![
            segment(0..split - 2, Vec::new()),
            segment(split..text.len(), Vec::new()),
        ];
        let mut cache = Vec::new();
        let mut linter = build_linter();
        let (first, rechecked) = run(text, &segments, &mut cache, &mut linter);
        assert_eq!(rechecked, 2);
        assert_eq!(first.len(), 2, "one typo per segment: {first:?}");

        // Same byte length, so the second segment's start is unchanged; only its
        // neighbour's content differs and only that neighbour must re-lint.
        let text2 = "the cat sat.\n\nthe dog wrold.";
        let (second, rechecked) = run(text2, &segments, &mut cache, &mut linter);
        assert_eq!(rechecked, 1, "only the edited segment re-lints");
        assert_eq!(
            second.len(),
            1,
            "only the surviving typo remains: {second:?}"
        );
        assert_eq!(second[0].word, "wrold");
        let start = text2.find("wrold").unwrap();
        assert_eq!(second[0].range, BytePos(start)..BytePos(start + 5));
    }

    #[test]
    fn inserting_before_a_segment_reuses_it_with_rebased_ranges() {
        let body = "teh cat.\n\nthe dog wrold.";
        let body_split = body.find("\n\n").unwrap() + 2;
        let base_segments = vec![
            segment(0..body_split - 2, Vec::new()),
            segment(body_split..body.len(), Vec::new()),
        ];
        let mut cache = Vec::new();
        let mut linter = build_linter();
        let (first, rechecked) = run(body, &base_segments, &mut cache, &mut linter);
        assert_eq!(rechecked, 2);
        assert_eq!(first.len(), 2);

        // Prepend an unrelated paragraph: both original segment contents are
        // unchanged, so they hit the cache and only the new segment re-lints.
        let prefix = "Extra line here.\n\n";
        let text = format!("{prefix}{body}");
        let shifted = prefix.len();
        let segments = vec![
            segment(0..prefix.len() - 2, Vec::new()),
            segment(shifted..shifted + 8, Vec::new()),
            segment(shifted + 10..text.len(), Vec::new()),
        ];
        let (second, rechecked) = run(&text, &segments, &mut cache, &mut linter);
        assert_eq!(rechecked, 1, "only the prepended segment re-lints");
        assert_eq!(second.len(), 2);
        let teh = text.find("teh").unwrap();
        assert_eq!(second[0].range, BytePos(teh)..BytePos(teh + 3));
        let wrold = text.find("wrold").unwrap();
        assert_eq!(second[1].range, BytePos(wrold)..BytePos(wrold + 5));
    }

    #[test]
    fn out_of_bounds_segments_are_skipped_not_panicking() {
        let text = "short";
        let segments = vec![
            segment(2..100, Vec::new()),
            segment(0..text.len(), Vec::new()),
        ];
        let issues = run_stateless(text, &segments);
        assert!(issues.is_empty(), "clean text yields nothing: {issues:?}");
    }

    #[test]
    fn char_to_byte_table_maps_multibyte_offsets_with_a_sentinel() {
        let text = "a你b";
        assert_eq!(char_to_byte_table(text), vec![0, 1, 4, 5]);
    }

    #[test]
    fn forget_drops_the_tab_cache_so_its_content_relints() {
        let text = "This is teh best.";
        let segments = Arc::new(vec![segment(0..text.len(), Vec::new())]);
        let mut caches: HashMap<u64, Vec<CachedSegment>> = HashMap::new();
        let mut linter = build_linter();

        let cache = caches.entry(7).or_default();
        let (_, first) = run(text, segments.as_ref(), cache, &mut linter);
        let (_, second) = run(text, segments.as_ref(), cache, &mut linter);
        assert_eq!(first, 1, "the first pass lints the only segment");
        assert_eq!(second, 0, "the second pass is served from the cache");

        let result = apply_request(SpellRequest::Forget { tab: 7 }, &mut caches, &mut linter);
        assert!(result.is_none(), "forget publishes nothing");
        assert!(
            !caches.contains_key(&7),
            "the closed tab's cache is dropped"
        );

        // The cache is gone, so an identical content check lints from cold again.
        let cache = caches.entry(7).or_default();
        let (_, rechecked) = run(text, segments.as_ref(), cache, &mut linter);
        assert_eq!(
            rechecked, 1,
            "after forget, identical content is linted again"
        );
    }

    #[test]
    fn check_after_forget_rebuilds_the_tab_cache() {
        let text = "This is teh best.";
        let segments = Arc::new(vec![segment(0..text.len(), Vec::new())]);
        let mut caches: HashMap<u64, Vec<CachedSegment>> = HashMap::new();
        let mut linter = build_linter();

        let first = apply_request(
            check_request(3, text, Arc::clone(&segments)),
            &mut caches,
            &mut linter,
        )
        .expect("a check publishes a result");
        apply_request(
            check_request(3, text, Arc::clone(&segments)),
            &mut caches,
            &mut linter,
        );
        assert_eq!(
            caches.get(&3).map(Vec::len),
            Some(1),
            "check fills the cache"
        );

        apply_request(SpellRequest::Forget { tab: 3 }, &mut caches, &mut linter);
        assert!(!caches.contains_key(&3), "forget drops the cache");

        let second = apply_request(check_request(3, text, segments), &mut caches, &mut linter)
            .expect("a check publishes a result");
        assert_eq!(
            caches.get(&3).map(Vec::len),
            Some(1),
            "check rebuilds the cache"
        );
        assert_eq!(
            second.issues, first.issues,
            "identical content yields identical issues"
        );
    }

    #[test]
    fn eviction_drops_the_oldest_tab_caches_beyond_the_cap() {
        let mut caches: HashMap<u64, Vec<CachedSegment>> = HashMap::new();
        for tab in 1..=3u64 {
            caches.insert(tab, Vec::new());
        }

        evict_oldest_tabs(&mut caches, 2);
        assert!(
            !caches.contains_key(&1),
            "the smallest (oldest) tab is evicted"
        );
        assert!(caches.contains_key(&2) && caches.contains_key(&3));
        assert_eq!(caches.len(), 2, "the map is back within the cap");

        evict_oldest_tabs(&mut caches, MAX_CACHED_TABS);
        assert_eq!(caches.len(), 2, "no eviction below the cap");
    }

    #[test]
    fn merge_cancels_a_check_that_a_following_same_tab_forget_supersedes() {
        let mut msg = SpellMsg::default();
        let segments = Arc::new(vec![segment(0..4, Vec::new())]);
        merge_spell_request(&mut msg, check_request(7, "text", segments));
        merge_spell_request(&mut msg, SpellRequest::Forget { tab: 7 });
        assert!(
            msg.check.is_none(),
            "a Forget after a same-tab Check cancels the check"
        );
        assert_eq!(msg.forgets, vec![7]);
    }

    #[test]
    fn merge_keeps_a_check_that_follows_its_forget() {
        let mut msg = SpellMsg::default();
        let segments = Arc::new(vec![segment(0..4, Vec::new())]);
        merge_spell_request(&mut msg, SpellRequest::Forget { tab: 7 });
        merge_spell_request(&mut msg, check_request(7, "text", segments));
        assert!(
            matches!(msg.check, Some(SpellRequest::Check { tab: 7, .. })),
            "a newer Check survives an earlier Forget"
        );
        assert_eq!(msg.forgets, vec![7], "the earlier Forget is still applied");
    }

    #[test]
    fn merge_keeps_a_check_for_a_different_tab_when_forgetting() {
        let mut msg = SpellMsg::default();
        let segments = Arc::new(vec![segment(0..4, Vec::new())]);
        merge_spell_request(&mut msg, check_request(1, "text", segments));
        merge_spell_request(&mut msg, SpellRequest::Forget { tab: 2 });
        assert!(
            matches!(msg.check, Some(SpellRequest::Check { tab: 1, .. })),
            "a Forget for another tab must not cancel the check"
        );
    }

    #[test]
    fn merge_keeps_the_latest_check() {
        let mut msg = SpellMsg::default();
        let segments = Arc::new(vec![segment(0..4, Vec::new())]);
        merge_spell_request(&mut msg, check_request(1, "first", Arc::clone(&segments)));
        merge_spell_request(&mut msg, check_request(1, "second", segments));
        match msg.check {
            Some(SpellRequest::Check { text, .. }) => {
                assert_eq!(text.materialize(), "second", "the newest Check wins")
            }
            other => panic!("expected a check, got {other:?}"),
        }
    }
}
