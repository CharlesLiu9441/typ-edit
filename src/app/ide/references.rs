//! Find-references conversion: engine [`ReferenceLocation`]s into the search
//! view's [`ProjectSearchResult`], plus the two pure helpers that back it.
//!
//! The engine response carries only byte ranges; line numbers and previews are
//! computed here from each file's text so the existing search result list can
//! render references unchanged. Every helper is pure, so the conversion is
//! exercised without a filesystem or a running engine.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use typ_edit_model::{
    BytePos, CharPos, FileMatches, ProjectSearchResult, ReferenceLocation, SearchMatch,
    TextSnapshot,
};

/// Preview length cap, in chars; mirrors the engine search worker's cap so a
/// reference row reads identically to a search row.
const PREVIEW_CHARS: usize = 240;

/// Whether `ch` may appear inside an extracted identifier.
///
/// ASCII letters/digits plus `_`/`-` cover Typst identifiers; `:` is included
/// so a label (`<fig:one>`) is captured whole. Non-ASCII characters (CJK,
/// accented letters) act as boundaries, keeping the header label to the ASCII
/// run the cursor sits in.
fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':')
}

/// The char immediately before byte `at`, or `None` at the string start.
fn char_before(text: &str, at: usize) -> Option<char> {
    text.get(..at)?.chars().next_back()
}

/// The char at byte `at`, or `None` at the string end.
fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..)?.chars().next()
}

/// The identifier surrounding byte `byte` in `text`, scanning back and forward
/// over [`is_ident_char`]. The offset is snapped down to a char boundary, so a
/// cursor mid-character still resolves; an empty run returns `None`.
pub(super) fn ident_at(text: &str, byte: usize) -> Option<String> {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let mut start = byte;
    while let Some(ch) = char_before(text, start) {
        if is_ident_char(ch) {
            start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let mut end = byte;
    while let Some(ch) = char_at(text, end) {
        if is_ident_char(ch) {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    (start < end).then(|| text[start..end].to_owned())
}

pub(super) fn ident_at_editor(
    editor: &typ_edit_editor::EditorState,
    byte: usize,
) -> Option<String> {
    let (line, offset) = editor.line_text_at_byte(byte);
    ident_at(&line, offset)
}

/// `path` relative to `root` with forward slashes when it lies inside `root`,
/// else the bare file name. `root` of `None` always falls back to the name.
pub(super) fn relative_display(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

/// One off-UI reference-preview request. Open buffers travel as O(1) text
/// snapshots; the worker materializes only the files the references mention.
pub(super) struct ReferencePreviewRequest {
    pub(super) seq: u64,
    pub(super) locations: Vec<ReferenceLocation>,
    pub(super) root: Option<PathBuf>,
    pub(super) open: Vec<(PathBuf, TextSnapshot)>,
    pub(super) generation: u64,
    pub(super) ctx: egui::Context,
}

pub(super) struct ReferencePreviewReply {
    pub(super) seq: u64,
    pub(super) generation: u64,
    pub(super) result: Result<ProjectSearchResult, usize>,
}

pub(super) struct ReferencePreviewWorker {
    pub(super) tx: Sender<ReferencePreviewRequest>,
    pub(super) rx: Receiver<ReferencePreviewReply>,
}

impl ReferencePreviewWorker {
    pub(super) fn spawn() -> Self {
        let (tx, requests) = mpsc::channel::<ReferencePreviewRequest>();
        let (replies, rx) = mpsc::channel::<ReferencePreviewReply>();
        let spawned = std::thread::Builder::new()
            .name("typ-edit-reference-preview".to_owned())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let open: HashMap<PathBuf, TextSnapshot> = request.open.into_iter().collect();
                    let text_of = |path: &Path| -> Option<String> {
                        open.get(path)
                            .map(TextSnapshot::materialize)
                            .or_else(|| crate::platform::fs_ops::read_to_string(path).ok())
                    };
                    let result = references_to_result(
                        &request.locations,
                        request.root.as_deref(),
                        text_of,
                        request.generation,
                    );
                    if replies
                        .send(ReferencePreviewReply {
                            seq: request.seq,
                            generation: request.generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    request.ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            log::error!("cannot start reference preview worker: {err}");
        }
        Self { tx, rx }
    }
}

/// Builds the display preview for one match: the line text, truncated to at
/// most [`PREVIEW_CHARS`] chars around the match, plus the match's CHAR range
/// inside that preview. `None` when `range` is not a valid slice of `line`.
fn make_preview(line: &str, range: Range<usize>) -> Option<(String, Range<usize>)> {
    let matched = line.get(range.clone())?;
    let total_chars = line.chars().count();
    let match_start = line.get(..range.start)?.chars().count();
    let match_len = matched.chars().count();

    let (start_char, take) = if total_chars <= PREVIEW_CHARS {
        (0, total_chars)
    } else {
        let end_needed = match_start + match_len;
        let start = match_start
            .saturating_sub(PREVIEW_CHARS / 4)
            .max(end_needed.saturating_sub(PREVIEW_CHARS))
            .min(total_chars - PREVIEW_CHARS)
            .min(match_start);
        (start, PREVIEW_CHARS)
    };

    let preview: String = line.chars().skip(start_char).take(take).collect();
    let preview_start = match_start - start_char;
    let preview_end = (match_start + match_len - start_char).min(preview.chars().count());
    Some((preview, preview_start..preview_end))
}

/// One [`SearchMatch`] for the byte range `byte..byte + byte_len` in `text`, or
/// `None` when the range is out of bounds, off a char boundary, or spans a
/// newline (a reference range never should).
fn reference_match(text: &str, byte: usize, byte_len: usize) -> Option<SearchMatch> {
    let end = byte.checked_add(byte_len)?;
    let matched = text.get(byte..end)?;
    if matched.contains('\n') {
        return None;
    }
    let line_start = text.get(..byte)?.rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[byte..]
        .find('\n')
        .map_or(text.len(), |index| byte + index);
    let line = text.get(line_start..line_end)?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let local = (byte - line_start)..(end - line_start);
    let (preview, preview_match) = make_preview(line, local)?;
    let line_number = text.get(..byte)?.matches('\n').count();
    Some(SearchMatch {
        line: line_number,
        byte: BytePos(byte),
        byte_len,
        preview,
        preview_match: CharPos(preview_match.start)..CharPos(preview_match.end),
    })
}

/// Converts sorted engine references into the search view's result shape.
///
/// `text_of` supplies each file's text (open tab or disk); a path answered with
/// `None` is skipped. Locations with an out-of-bounds or off-boundary byte are
/// skipped too. Files are grouped in input order (the engine sorts by path, so
/// grouping preserves that order). Returns `Err(skipped)` when no match could be
/// built, so the caller can report "no references" without an empty panel.
pub(super) fn references_to_result(
    locations: &[ReferenceLocation],
    root: Option<&Path>,
    text_of: impl Fn(&Path) -> Option<String>,
    generation: u64,
) -> Result<ProjectSearchResult, usize> {
    let mut files: Vec<FileMatches> = Vec::new();
    let mut cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut total = 0usize;
    let mut skipped = 0usize;
    for location in locations {
        let text = cache
            .entry(location.path.clone())
            .or_insert_with(|| text_of(&location.path));
        let Some(text) = text.as_deref() else {
            skipped += 1;
            continue;
        };
        let byte_len = location.range.end.0.saturating_sub(location.range.start.0);
        let Some(matched) = reference_match(text, location.range.start.0, byte_len) else {
            skipped += 1;
            continue;
        };
        total += 1;
        match files.last_mut() {
            Some(file) if file.path == location.path => file.matches.push(matched),
            _ => files.push(FileMatches {
                path: location.path.clone(),
                relative: relative_display(root, &location.path),
                matches: vec![matched],
            }),
        }
    }
    if total == 0 {
        return Err(skipped);
    }
    Ok(ProjectSearchResult {
        generation,
        files,
        total_matches: total,
        truncated: false,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(path: &str, start: usize, end: usize) -> ReferenceLocation {
        ReferenceLocation {
            path: PathBuf::from(path),
            range: BytePos(start)..BytePos(end),
        }
    }

    #[test]
    fn ident_at_editor_reads_only_the_line() {
        let mut editor = typ_edit_editor::EditorState::new();
        editor.set_text("alpha\n  foo.bar\nomega".to_owned());
        assert_eq!(ident_at_editor(&editor, 10).as_deref(), Some("foo"));
        assert_eq!(ident_at_editor(&editor, 5).as_deref(), Some("alpha"));
    }

    #[test]
    fn ident_at_extracts_the_word_with_multibyte_neighbors() {
        // bytes: 中 0..3, f 3, o 4, o 5, 中 6..9, space 9, b 10...
        let text = "中foo中 bar";
        assert_eq!(ident_at(text, 4).as_deref(), Some("foo"));
        assert_eq!(ident_at(text, 3).as_deref(), Some("foo"));
        assert_eq!(ident_at(text, 6).as_deref(), Some("foo"));
        // A multibyte char is a boundary, not identifier text.
        assert_eq!(ident_at(text, 0), None);
        assert_eq!(ident_at(text, 99).as_deref(), Some("bar"));
    }

    #[test]
    fn ident_at_accepts_label_and_kebab_characters() {
        assert_eq!(ident_at("see <fig:one>", 7).as_deref(), Some("fig:one"));
        assert_eq!(ident_at("my-name", 3).as_deref(), Some("my-name"));
        assert_eq!(ident_at("   ", 1), None);
    }

    #[test]
    fn relative_display_strips_root_and_falls_back_to_the_name() {
        assert_eq!(
            relative_display(Some(Path::new("/proj")), Path::new("/proj/sub/a.typ")),
            "sub/a.typ"
        );
        assert_eq!(
            relative_display(Some(Path::new("/proj")), Path::new("/elsewhere/c.typ")),
            "c.typ"
        );
        assert_eq!(
            relative_display(None, Path::new("/elsewhere/c.typ")),
            "c.typ"
        );
    }

    #[test]
    fn reference_preview_worker_materializes_snapshots_off_thread() {
        let worker = ReferencePreviewWorker::spawn();
        let path = PathBuf::from("a.typ");
        worker
            .tx
            .send(ReferencePreviewRequest {
                seq: 9,
                locations: vec![location("a.typ", 6, 10)],
                root: None,
                open: vec![(
                    path,
                    TextSnapshot::from_owned("alpha\nbeta target gamma\n", 4),
                )],
                generation: 12,
                ctx: egui::Context::default(),
            })
            .expect("send preview request");
        let reply = worker
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("preview reply");
        assert_eq!(reply.seq, 9);
        assert_eq!(reply.generation, 12);
        let result = reply.result.expect("readable snapshot");
        assert_eq!(result.total_matches, 1);
        assert_eq!(result.files[0].relative, "a.typ");
        assert_eq!(result.files[0].matches[0].preview, "beta target gamma");
    }

    #[test]
    fn references_to_result_builds_line_preview_and_char_range() {
        let text = "alpha\nbeta target gamma\nomega".to_owned();
        // "target" starts after "alpha\n"(6) + "beta "(5) = byte 11.
        let locations = vec![location("/proj/a.typ", 11, 17)];
        let result = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |_| Some(text.clone()),
            3,
        )
        .expect("one match");
        assert_eq!(result.generation, 3);
        assert_eq!(result.total_matches, 1);
        assert!(!result.truncated);
        assert!(result.error.is_none());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].relative, "a.typ");
        let matched = &result.files[0].matches[0];
        assert_eq!(matched.line, 1);
        assert_eq!(matched.byte, BytePos(11));
        assert_eq!(matched.byte_len, 6);
        assert_eq!(matched.preview, "beta target gamma");
        assert_eq!(matched.preview_match, CharPos(5)..CharPos(11));
    }

    #[test]
    fn references_to_result_maps_cjk_preview_char_offsets() {
        // "abc "(4) + 你(3) + 好(3) = 世 starts at byte 10.
        let text = "abc 你好世界 xyz".to_owned();
        let locations = vec![location("/proj/a.typ", 10, 16)];
        let result = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |_| Some(text.clone()),
            1,
        )
        .expect("one match");
        let matched = &result.files[0].matches[0];
        assert_eq!(matched.line, 0);
        assert_eq!(matched.byte, BytePos(10));
        assert_eq!(matched.byte_len, 6);
        assert_eq!(matched.preview, "abc 你好世界 xyz");
        // chars: a0 b1 c2 ' '3 你4 好5 世6 界7
        assert_eq!(matched.preview_match, CharPos(6)..CharPos(8));
    }

    #[test]
    fn references_to_result_groups_files_in_sorted_order() {
        let a = "x ref\nref y".to_owned();
        let b = "ref".to_owned();
        let locations = vec![
            location("/proj/a.typ", 2, 5),
            location("/proj/a.typ", 6, 9),
            location("/proj/b.typ", 0, 3),
        ];
        let mut texts = HashMap::new();
        texts.insert(PathBuf::from("/proj/a.typ"), a);
        texts.insert(PathBuf::from("/proj/b.typ"), b);
        let result = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |path| texts.get(path).cloned(),
            9,
        )
        .expect("three matches");
        assert_eq!(result.total_matches, 3);
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[0].relative, "a.typ");
        assert_eq!(result.files[0].matches.len(), 2);
        assert_eq!(result.files[0].matches[0].line, 0);
        assert_eq!(result.files[0].matches[1].line, 1);
        assert_eq!(result.files[1].relative, "b.typ");
        assert_eq!(result.files[1].matches.len(), 1);
    }

    #[test]
    fn references_to_result_uses_file_name_outside_root() {
        let text = "ok ref here".to_owned();
        let locations = vec![location("/elsewhere/c.typ", 3, 6)];
        let result = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |_| Some(text.clone()),
            1,
        )
        .expect("one match");
        assert_eq!(result.files[0].relative, "c.typ");
        assert_eq!(result.files[0].path, PathBuf::from("/elsewhere/c.typ"));
    }

    #[test]
    fn references_to_result_skips_unreadable_and_out_of_range() {
        let locations = vec![
            location("/proj/missing.typ", 0, 3),
            location("/proj/a.typ", 900, 903),
            location("/proj/a.typ", 3, 6),
        ];
        let text = "ok ref here".to_owned();
        let result = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |path| (path == Path::new("/proj/a.typ")).then(|| text.clone()),
            2,
        )
        .expect("the in-range match survives");
        assert_eq!(result.total_matches, 1);
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].matches[0].byte, BytePos(3));
    }

    #[test]
    fn references_to_result_returns_the_skipped_count_when_empty() {
        let locations = vec![
            location("/proj/missing.typ", 0, 3),
            location("/proj/other.typ", 99, 102),
        ];
        let error = references_to_result(
            &locations,
            Some(Path::new("/proj")),
            |path| (path == Path::new("/proj/other.typ")).then(|| "short".to_owned()),
            1,
        )
        .expect_err("nothing kept");
        assert_eq!(error, 2);
    }

    #[test]
    fn references_to_result_on_no_locations_is_an_empty_error() {
        assert_eq!(
            references_to_result(&[], Some(Path::new("/proj")), |_| None, 1).unwrap_err(),
            0
        );
    }
}
