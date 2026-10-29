//! Merging of parse (syntax) and compile diagnostics for the editor and the
//! error list. Both surfaces show the same merged list.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use typ_edit_model::{ByteRange, Diag, Severity};

/// Fills the empty path of each diagnostic with `path`.
///
/// Parse diagnostics carry no path of their own — they always belong to the
/// buffer that produced them, so the caller stamps the open file's path.
pub(super) fn with_path(diags: &mut [Diag], path: &Path) {
    for diag in diags {
        if diag.path.as_os_str().is_empty() {
            diag.path = path.to_path_buf();
        }
    }
}

/// Merges parse diagnostics (whose path the caller has already filled in) with
/// the compile diagnostics, de-duplicated by `(path, range, message)`.
///
/// Compile entries win when both report the same location and message. A parse
/// warning that overlaps a compile error is then dropped: a spot that is both a
/// lint warning and a compile error renders red only. Compile warnings and parse
/// errors are untouched. The result is sorted by `range.start`; the sort is
/// stable, so entries that share a start keep compile-before-parse order.
pub(super) fn merged_diagnostics(parse: Vec<Diag>, compile: &[Diag]) -> Vec<Diag> {
    let key = |diag: &Diag| {
        (
            diag.path.clone(),
            diag.range.start.0,
            diag.range.end.0,
            diag.message.clone(),
        )
    };
    // Tag each entry with its source so a parse warning can be outranked by a
    // compile error at an overlapping range.
    let mut merged: Vec<(bool, Diag)> = compile.iter().cloned().map(|diag| (false, diag)).collect();
    let mut seen: HashSet<(PathBuf, usize, usize, String)> =
        merged.iter().map(|(_, diag)| key(diag)).collect();
    for diag in parse {
        if seen.insert(key(&diag)) {
            merged.push((true, diag));
        }
    }
    let compile_errors: Vec<ByteRange> = compile
        .iter()
        .filter(|diag| diag.severity == Severity::Error)
        .map(|diag| diag.range.clone())
        .collect();
    merged.retain(|(is_parse, diag)| {
        !(*is_parse
            && diag.severity == Severity::Warning
            && compile_errors
                .iter()
                .any(|range| ranges_overlap(&diag.range, range)))
    });
    let mut merged: Vec<Diag> = merged.into_iter().map(|(_, diag)| diag).collect();
    merged.sort_by_key(|diag| diag.range.start);
    merged
}

/// Whether two byte ranges overlap; touching endpoints do not, equal ranges do.
fn ranges_overlap(a: &ByteRange, b: &ByteRange) -> bool {
    a.start < b.end && b.start < a.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_model::{BytePos, Severity};

    fn diag(path: &str, start: usize, end: usize, message: &str) -> Diag {
        Diag {
            path: PathBuf::from(path),
            range: BytePos(start)..BytePos(end),
            message: message.to_owned(),
            severity: Severity::Error,
        }
    }

    #[test]
    fn with_path_fills_only_empty_paths() {
        let mut diags = vec![diag("", 0, 1, "syntax"), diag("/w/a.typ", 2, 3, "compile")];
        with_path(&mut diags, Path::new("/w/a.typ"));
        assert_eq!(diags[0].path, PathBuf::from("/w/a.typ"));
        assert_eq!(diags[1].path, PathBuf::from("/w/a.typ"));
    }

    #[test]
    fn merged_dedups_same_location_and_message_keeping_compile() {
        // The parse twin is identical on the dedup key; the compile copy is a
        // warning, so keeping it proves which side survived.
        let parse = vec![diag("/w/a.typ", 0, 5, "boom")];
        let mut compile_diag = diag("/w/a.typ", 0, 5, "boom");
        compile_diag.severity = Severity::Warning;
        let compile = vec![compile_diag];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].severity, Severity::Warning, "compile entry wins");
    }

    #[test]
    fn merged_keeps_different_messages_at_the_same_range() {
        let parse = vec![diag("/w/a.typ", 0, 5, "parse")];
        let compile = vec![diag("/w/a.typ", 0, 5, "compile")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merged_sorts_by_start_with_compile_before_parse_on_ties() {
        // Compile and parse share start 0; the stable sort keeps the compile
        // entry first even though parse is the first argument.
        let parse = vec![diag("/w/a.typ", 0, 1, "parse")];
        let compile = vec![
            diag("/w/a.typ", 0, 1, "compile"),
            diag("/w/a.typ", 5, 6, "later"),
        ];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(
            merged
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>(),
            ["compile", "parse", "later"]
        );
    }

    #[test]
    fn merged_handles_empty_inputs() {
        assert!(merged_diagnostics(Vec::new(), &[]).is_empty());
        let compile = vec![diag("/w/a.typ", 0, 1, "compile")];
        assert_eq!(merged_diagnostics(Vec::new(), &compile).len(), 1);
        let parse = vec![diag("/w/a.typ", 0, 1, "parse")];
        assert_eq!(merged_diagnostics(parse, &[]).len(), 1);
    }

    fn warning(path: &str, start: usize, end: usize, message: &str) -> Diag {
        let mut diag = diag(path, start, end, message);
        diag.severity = Severity::Warning;
        diag
    }

    #[test]
    fn merged_compile_error_outranks_overlapping_parse_warning() {
        let parse = vec![warning("/w/a.typ", 0, 5, "lint")];
        let compile = vec![diag("/w/a.typ", 0, 5, "compile error")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 1, "only the compile error survives");
        assert_eq!(merged[0].message, "compile error");
        assert_eq!(merged[0].severity, Severity::Error);
    }

    #[test]
    fn merged_drops_parse_warning_on_partial_overlap_with_compile_error() {
        let parse = vec![warning("/w/a.typ", 3, 9, "lint")];
        let compile = vec![diag("/w/a.typ", 0, 5, "compile error")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].message, "compile error");
    }

    #[test]
    fn merged_keeps_disjoint_parse_warning_and_compile_error() {
        let parse = vec![warning("/w/a.typ", 10, 15, "lint")];
        let compile = vec![diag("/w/a.typ", 0, 5, "compile error")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merged_keeps_parse_warning_when_only_a_compile_warning_overlaps() {
        let parse = vec![warning("/w/a.typ", 0, 5, "lint")];
        let compile = vec![warning("/w/a.typ", 0, 5, "compile warning")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 2, "no error to outrank the parse warning");
    }

    #[test]
    fn merged_keeps_parse_error_overlapping_compile_error() {
        let parse = vec![diag("/w/a.typ", 0, 5, "parse error")];
        let compile = vec![diag("/w/a.typ", 0, 5, "compile error")];
        let merged = merged_diagnostics(parse, &compile);
        assert_eq!(merged.len(), 2, "parse errors are never dropped");
    }
}
