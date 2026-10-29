//! Prose statistics (characters, words) and parser diagnostics.

use std::ops::Range;
use std::path::PathBuf;

use typ_edit_model::{BytePos, ByteRange, Diag, Severity, is_cjk};
use typst::syntax::{DiagSpan, DiagSpanKind, Source};

/// Counts prose characters and words over the `Text`/`MathText` leaves of a
/// tree.
///
/// - `chars`: every character of `Text`/`MathText` leaves. Spaces (a separate `Space`
///   leaf), raw blocks, comments and escapes are not prose and are excluded.
/// - `words`: maximal whitespace-delimited runs, except that CJK/Kana/Hangul
///   characters count individually (a run like `hello世界` is 3 words: `hello`, `世`,
///   `界`). This mirrors how editors count CJK text.
pub(crate) fn count_words(text: &str) -> usize {
    let mut words = 0;
    let mut in_run = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if in_run {
                words += 1;
                in_run = false;
            }
        } else if is_cjk(c) {
            if in_run {
                words += 1;
                in_run = false;
            }
            words += 1;
        } else {
            in_run = true;
        }
    }
    if in_run {
        words += 1;
    }
    words
}

/// Converts the parser's error/warning list into UI diagnostics.
pub(crate) fn diagnostics(source: &Source) -> Vec<Diag> {
    let (errors, warnings) = source.root().errors_and_warnings();
    errors
        .iter()
        .chain(&warnings)
        .filter_map(|diagnostic| {
            Some(Diag {
                // Filled in by the caller: parse results always belong to the open file.
                path: PathBuf::new(),
                range: diag_range(source, diagnostic.span)?,
                message: diagnostic.message.to_string(),
                severity: if diagnostic.is_error {
                    Severity::Error
                } else {
                    Severity::Warning
                },
            })
        })
        .collect()
}

/// Runs the pure syntax lints from the analysis crate.
///
/// These are appended after the syntax diagnostics by both parse paths, so the
/// merged list keeps parser errors/warnings first and the lints after (P3-3b).
pub(crate) fn lint_diagnostics(source: &Source) -> Vec<Diag> {
    typ_edit_analysis::lint::lint_source(source)
}

/// The byte range `span` names within `source`, ignoring the span's own file id
/// so callers keep their own id policy. `None` for a detached span or a
/// `Number` span `source` cannot resolve.
pub(crate) fn span_local_range(source: &Source, span: DiagSpan) -> Option<Range<usize>> {
    match span.get() {
        DiagSpanKind::Detached => None,
        DiagSpanKind::Number { num, sub_range, .. } => source.range(num, sub_range),
        DiagSpanKind::Range { range, .. } => Some(range),
    }
}

/// Maps a diagnostic span to a byte range of `source` (`None` if it points elsewhere).
fn diag_range(source: &Source, span: DiagSpan) -> Option<ByteRange> {
    if span.id() != Some(source.id()) {
        return None;
    }
    span_local_range(source, span).map(|range| BytePos(range.start)..BytePos(range.end))
}
