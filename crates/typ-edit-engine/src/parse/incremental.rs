//! Incremental parse state: one persistent [`Source`] plus cached highlight
//! spans, updated by splicing the reparsed window instead of re-walking the
//! whole tree.

use std::ops::Range;

use typ_edit_analysis::lexical;
use typ_edit_model::{
    BytePos, ByteRange, CharCount, HighlightKind, IdeData, LayoutHints, ParseResult, TextStats,
};
use typst::syntax::{LinkedNode, Source};

use super::complete::DefinedNames;
use super::highlight::{highlights as full_highlights, highlights_in, typed as typed_highlights};
use super::{
    call_params, diagnostics, fold_ranges, lint_diagnostics, parse_file_id, prose_segments, visit,
};

/// Persistent parser owned by the worker thread.
///
/// Holds exactly one [`Source`]: cloning it out would make the next
/// [`Source::edit`] deep-copy the whole tree (`Arc::make_mut`), so the source is
/// never cloned and only ever updated in place through
/// [`Source::replace`]. One fused [`visit`] supplies stats, mode ranges, math
/// atoms, list lines and definitions; diagnostics, call parameters, folds,
/// symbols and prose remain separate passes, and only the highlight spans are
/// updated incrementally.
pub(crate) struct IncrementalParser {
    source: Option<Source>,
    highlights: Vec<(ByteRange, HighlightKind)>,
    /// The definition set the cached `highlights` were tagged with. A change to
    /// it can retag identifiers outside the reparsed window, so the cache is
    /// only spliced while the set is stable.
    defined: DefinedNames,
    #[cfg(test)]
    windowed_splices: usize,
    #[cfg(test)]
    full_fallbacks: usize,
}

impl IncrementalParser {
    pub(crate) fn new() -> Self {
        Self {
            source: None,
            highlights: Vec::new(),
            defined: DefinedNames::default(),
            #[cfg(test)]
            windowed_splices: 0,
            #[cfg(test)]
            full_fallbacks: 0,
        }
    }

    /// Produces the same [`ParseResult`] as [`super::run`] for `text`/`cursor`,
    /// reusing the persistent tree and splicing only the reparsed highlight
    /// window. Takes the materialized text by value so the first parse moves it
    /// straight into the [`Source`] without a second full copy. The `generation`
    /// stays `0`; the worker stamps it like [`super::run`]'s callers do.
    pub(crate) fn parse(&mut self, text: String, cursor: usize, want_spell: bool) -> ParseResult {
        // Bring the persistent tree in line with `text`, capturing the reparsed
        // window so cached highlight spans can be spliced.
        let action = self.sync_source(text);
        let source = self.source.as_ref().expect("initialized by sync_source");
        let root = LinkedNode::new(source.root());

        // One fused walk supplies BOTH the definition set the highlight update
        // needs and every layout/ide field of the result, so definitions are
        // walked once per request.
        let fused = visit(&root, source.text());
        let new_defined = DefinedNames::from_items(&fused.definitions);

        let (windowed, fell_back) = apply_highlights(
            &root,
            &action,
            &new_defined,
            &mut self.highlights,
            &mut self.defined,
        );
        #[cfg(test)]
        {
            if windowed {
                self.windowed_splices += 1;
            }
            if fell_back {
                self.full_fallbacks += 1;
            }
        }
        #[cfg(not(test))]
        let _ = (windowed, fell_back);

        let mut diagnostics = diagnostics(source);
        diagnostics.extend(lint_diagnostics(source));
        let call_params = call_params(&root, cursor);
        // Fold candidates are cheap to recompute as a full walk of the
        // persistent tree; only the highlight spans are spliced incrementally.
        let folds = fold_ranges(source);
        // The symbol outline is a full walk too, for the same reason.
        let symbols = lexical::symbols(source);
        // Prose segments are a full walk too; they feed only the spell worker.
        let prose = if want_spell {
            prose_segments(source)
        } else {
            Vec::new()
        };

        ParseResult {
            generation: 0,
            tab: 0,
            highlights: self.highlights.clone(),
            diagnostics,
            stats: TextStats {
                words: fused.words,
                chars: CharCount(fused.chars),
            },
            layout: LayoutHints {
                blocked_ranges: fused.blocked_ranges,
                code_ranges: fused.code_ranges,
                math_ranges: fused.math_ranges,
                math_atoms: fused.math_atoms,
                list_lines: fused.list_lines,
            },
            ide: IdeData {
                definitions: fused.definitions,
                call_params,
            },
            folds,
            symbols,
            prose,
        }
    }

    /// Brings the persistent source in line with `text`, returning what changed
    /// so the highlight cache can be brought in line too.
    fn sync_source(&mut self, text: String) -> SourceAction {
        if self.source.is_none() {
            self.source = Some(Source::new(parse_file_id(), text));
            return SourceAction::Initial;
        }
        let source = self.source.as_mut().expect("present");
        // Comparing the text up front distinguishes "unchanged" from the
        // delete-to-empty case, where `Source::replace` also returns `0..0` but
        // the tree genuinely changed.
        if source.text() == text.as_str() {
            return SourceAction::Unchanged;
        }
        let old_len = source.text().len();
        let window = source.replace(&text);
        let new_len = source.text().len();
        SourceAction::Edited {
            window,
            old_len,
            new_len,
        }
    }
}

/// What [`IncrementalParser::sync_source`] did to the persistent tree.
enum SourceAction {
    /// The tree was created from scratch (first parse).
    Initial,
    /// The text was unchanged; the cached highlights are still valid.
    Unchanged,
    /// The text was replaced; `window` is the reparsed range in the new
    /// `new_len`-byte text.
    Edited {
        window: Range<usize>,
        old_len: usize,
        new_len: usize,
    },
}

/// Brings `highlights` in line with the tree `sync_source` produced.
///
/// When the document's definition set changed, a full recompute replaces the
/// splice: a renamed `#let` retags identifier leaves both inside and outside the
/// reparsed window, and the window splice only sees the inside. Returns
/// `(windowed, fell_back)` for the test-only splice counters.
fn apply_highlights(
    root: &LinkedNode<'_>,
    action: &SourceAction,
    new_defined: &DefinedNames,
    highlights: &mut Vec<(ByteRange, HighlightKind)>,
    defined: &mut DefinedNames,
) -> (bool, bool) {
    match action {
        SourceAction::Initial => {
            *highlights = typed_highlights(full_highlights(root, new_defined));
        }
        SourceAction::Unchanged => {}
        SourceAction::Edited {
            window,
            old_len,
            new_len,
        } => {
            if *new_defined == *defined {
                *highlights = splice_highlights(
                    root,
                    highlights,
                    ReparsedWindow {
                        window: window.clone(),
                        old_len: *old_len,
                    },
                    new_defined,
                );
                *defined = new_defined.clone();
                return (!(window.start == 0 && window.end == *new_len), false);
            }
            *highlights = typed_highlights(full_highlights(root, new_defined));
            *defined = new_defined.clone();
            return (false, true);
        }
    }
    *defined = new_defined.clone();
    (false, false)
}

/// The outcome of one [`Source::replace`]: the reparsed range in the NEW text's
/// byte coordinates, plus the pre-edit text length needed to map it back.
struct ReparsedWindow {
    window: Range<usize>,
    old_len: usize,
}

/// Rebuilds the highlight vector after `edit`'s window was reparsed.
///
/// Window math: `new_len = root.range().end` is the post-edit byte length, so
/// `delta = new_len - old_len` is how far every byte after the edit shifts. The
/// reparsed window maps back to the old text as `old_start..old_end` with
/// `old_start = window.start` (the prefix is unchanged, so its offsets match)
/// and `old_end = window.end - delta`. Cached spans that end at or before
/// `old_start` are kept verbatim; cached spans that start at or after `old_end`
/// are shifted by `delta`; everything between is replaced by a windowed walk of
/// the new tree. Spans are disjoint leaf ranges that never straddle a window
/// boundary, and the windowed walk emits document order, so the concatenation is
/// exactly a full walk of the new tree.
///
/// `defined` must equal the set the cached spans were tagged with; the caller
/// guarantees this, since a changed set can retag leaves outside the window.
fn splice_highlights(
    root: &LinkedNode<'_>,
    cached: &[(ByteRange, HighlightKind)],
    edit: ReparsedWindow,
    defined: &DefinedNames,
) -> Vec<(ByteRange, HighlightKind)> {
    let new_len = root.range().end;
    let reparsed = edit.window;

    // A full reparse (including the empty replacement of everything) has no
    // usable prefix/suffix; re-walk the whole new tree.
    if reparsed.start == 0 && reparsed.end == new_len {
        return typed_highlights(full_highlights(root, defined));
    }

    let delta = new_len as isize - edit.old_len as isize;
    let old_start = reparsed.start;
    let old_end = (reparsed.end as isize - delta).max(0) as usize;

    let mut out = Vec::with_capacity(cached.len());
    for (range, kind) in cached {
        if range.end.0 <= old_start {
            out.push((range.clone(), *kind));
        }
    }
    out.extend(typed_highlights(highlights_in(root, reparsed, defined)));
    for (range, kind) in cached {
        if range.start.0 >= old_end {
            let start = (range.start.0 as isize + delta) as usize;
            let end = (range.end.0 as isize + delta) as usize;
            out.push((BytePos(start)..BytePos(end), *kind));
        }
    }
    out
}

#[cfg(test)]
mod tests;
