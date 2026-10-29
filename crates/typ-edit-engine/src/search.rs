//! Project-wide search worker: walks `.typ` files under a root and reports
//! line-based, byte-ranged matches.
//!
//! Mirrors the spell worker: the thread coalesces to the latest
//! [`ProjectSearchRequest`], so a burst of keystrokes costs at most one search
//! pass. Matching is line-based only — a query never spans a newline.

use crate::latest::LatestReceiver;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crossbeam_channel::Sender;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::thread::JoinHandle;
use typ_edit_model::{
    BytePos, CharPos, CharRange, FileMatches, ProjectSearchRequest, ProjectSearchResult,
    SearchMatch, SearchOptions,
};

/// Upper bound on a regex pattern, in bytes. Guards against pathological
/// compilation cost from a pasted wall of text; matches the editor's find bar.
const MAX_REGEX_LEN: usize = 4 * 1024;

/// Preview length cap, in chars.
const PREVIEW_CHARS: usize = 240;

/// Resource caps for one search pass.
struct Limits {
    max_file_bytes: usize,
    max_results: usize,
}

const DEFAULT_LIMITS: Limits = Limits {
    max_file_bytes: 2 * 1024 * 1024,
    max_results: 2000,
};

/// Spawns the project-search worker thread. Reads a latest-wins mailbox and is
/// supervised; the search itself is stateless.
pub fn spawn(
    req: LatestReceiver<ProjectSearchRequest>,
    res_tx: Sender<ProjectSearchResult>,
    waker: Waker,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-search".to_owned())
        .spawn(move || {
            supervise(Pipeline::Search, &health, || {
                while let Ok(request) = req.recv() {
                    let result = run(&request);
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    waker.wake();
                }
            });
        })
        .expect("spawn search worker")
}

/// Searches `request.root` recursively for `request.query`.
pub fn run(request: &ProjectSearchRequest) -> ProjectSearchResult {
    run_with_limits(request, DEFAULT_LIMITS)
}

/// [`run`] with explicit resource caps, for tests and future tuning.
fn run_with_limits(request: &ProjectSearchRequest, limits: Limits) -> ProjectSearchResult {
    let mut result = ProjectSearchResult {
        generation: request.generation,
        ..ProjectSearchResult::default()
    };
    if request.query.is_empty() {
        return result;
    }
    let matcher = match Matcher::new(&request.query, request.options) {
        Ok(matcher) => matcher,
        Err(err) => {
            result.error = Some(err);
            return result;
        }
    };

    let mut state = SearchState {
        root: &request.root,
        matcher: &matcher,
        include: request
            .include
            .iter()
            .filter_map(|p| Glob::new(p))
            .collect(),
        exclude: request
            .exclude
            .iter()
            .filter_map(|p| Glob::new(p))
            .collect(),
        limits,
        files: Vec::new(),
        total: 0,
        truncated: false,
    };
    if let Err(err) = state.walk(&request.root) {
        // Only a failure to read the root's own entries reaches here; an
        // unreadable sub-directory is skipped inside `walk`.
        result.error = Some(err.to_string());
        return result;
    }

    result.total_matches = state.total;
    result.truncated = state.truncated;
    result.files = std::mem::take(&mut state.files);
    result.files.sort_by(|a, b| a.relative.cmp(&b.relative));
    result
}

/// Mutable state threaded through the recursive walk.
struct SearchState<'a> {
    root: &'a Path,
    matcher: &'a Matcher,
    include: Vec<Glob>,
    exclude: Vec<Glob>,
    limits: Limits,
    files: Vec<FileMatches>,
    total: usize,
    truncated: bool,
}

impl SearchState<'_> {
    /// Recursively walks `dir`, collecting matches. Unreadable subtrees are
    /// skipped rather than aborting the walk.
    fn walk(&mut self, dir: &Path) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            if self.truncated {
                return Ok(());
            }
            let entry = entry?;
            let name = entry.file_name();
            // Hidden entries (`.typ-edit`, `.typ-edit.json`, dotted dirs) are
            // never searched.
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let file_type = entry.file_type()?;
            // `file_type` does not follow symlinks; skip them outright so a
            // link cycle cannot spin the walk.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let _ = self.walk(&entry.path());
            } else if file_type.is_file() {
                self.visit_file(&entry.path());
            }
        }
        Ok(())
    }

    /// Applies the extension/glob/size filters, then collects matches for one
    /// candidate file.
    fn visit_file(&mut self, path: &Path) {
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("typ"))
        {
            return;
        }
        let relative = relative_slash(self.root, path);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let include_ok = self.include.is_empty()
            || self
                .include
                .iter()
                .any(|glob| glob.matches(&relative, &name));
        let exclude_hit = self
            .exclude
            .iter()
            .any(|glob| glob.matches(&relative, &name));
        if !include_ok || exclude_hit {
            return;
        }
        let oversized = std::fs::metadata(path)
            .map(|meta| meta.len())
            .unwrap_or(u64::MAX)
            > self.limits.max_file_bytes as u64;
        if oversized {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };

        let matches = self.collect_matches(&text);
        if !matches.is_empty() {
            self.files.push(FileMatches {
                path: path.to_path_buf(),
                relative,
                matches,
            });
        }
    }

    /// Scans `text` line by line, stopping once the result cap is exceeded.
    fn collect_matches(&mut self, text: &str) -> Vec<SearchMatch> {
        let mut matches = Vec::new();
        let mut line_number = 0usize;
        let mut line_start = 0usize;
        for segment in text.split_inclusive('\n') {
            if self.truncated {
                break;
            }
            let line_end = line_start + segment.len();
            let line = segment.strip_suffix('\n').unwrap_or(segment);
            let line = line.strip_suffix('\r').unwrap_or(line);
            for range in self.matcher.find_in(line) {
                if self.total >= self.limits.max_results {
                    self.truncated = true;
                    break;
                }
                let Some((preview, preview_match)) = make_preview(line, &range) else {
                    continue;
                };
                matches.push(SearchMatch {
                    line: line_number,
                    byte: BytePos(line_start + range.start),
                    byte_len: range.end - range.start,
                    preview,
                    preview_match,
                });
                self.total += 1;
            }
            line_start = line_end;
            line_number += 1;
        }
        matches
    }
}

/// A compiled query: the literal KMP automaton or a compiled regex, plus the
/// whole-word toggle.
struct Matcher {
    kind: MatcherKind,
    whole_word: bool,
}

enum MatcherKind {
    Literal(KmpAutomaton),
    Regex(regex::Regex),
}

impl Matcher {
    fn new(query: &str, options: SearchOptions) -> Result<Self, String> {
        let kind = if options.regex {
            MatcherKind::Regex(build_regex(query, options)?)
        } else {
            MatcherKind::Literal(KmpAutomaton::new(query, options.case_sensitive))
        };
        Ok(Self {
            kind,
            whole_word: options.whole_word,
        })
    }

    /// All matches within a single line, as byte ranges into `line`.
    fn find_in(&self, line: &str) -> Vec<Range<usize>> {
        match &self.kind {
            MatcherKind::Literal(automaton) => {
                let found = automaton.find_all(line);
                if self.whole_word {
                    found
                        .into_iter()
                        .filter(|range| whole_word_ok(line, range))
                        .collect()
                } else {
                    found
                }
            }
            // The regex source already encodes case and whole-word boundaries.
            MatcherKind::Regex(regex) => {
                regex.find_iter(line).map(|m| m.start()..m.end()).collect()
            }
        }
    }
}

/// Compiles a regex query, applying the same length guard and whole-word wrap
/// as the editor's find bar. `!case_sensitive` prefixes `(?i)` in either mode.
fn build_regex(query: &str, options: SearchOptions) -> Result<regex::Regex, String> {
    if query.len() > MAX_REGEX_LEN {
        return Err(format!(
            "pattern too long ({} bytes, max {MAX_REGEX_LEN})",
            query.len()
        ));
    }
    let body = if options.whole_word {
        format!(r"\b(?:{query})\b")
    } else {
        query.to_owned()
    };
    let source = if options.case_sensitive {
        body
    } else {
        format!("(?i){body}")
    };
    regex::Regex::new(&source).map_err(|err| err.to_string())
}

/// Char-wise KMP matcher, ported from the editor's find bar so the engine does
/// not depend on the UI crate. Scanning resumes after each match, so `"aa"` in
/// `"aaaa"` yields `0..2` and `2..4`.
struct KmpAutomaton {
    pattern: Vec<char>,
    failure: Vec<usize>,
    case_sensitive: bool,
}

impl KmpAutomaton {
    fn new(pattern: &str, case_sensitive: bool) -> Self {
        let pattern: Vec<char> = pattern.chars().collect();
        let mut failure = vec![0usize; pattern.len()];
        let mut k = 0usize;
        for i in 1..pattern.len() {
            while k > 0 && !chars_match(pattern[i], pattern[k], case_sensitive) {
                k = failure[k - 1];
            }
            if chars_match(pattern[i], pattern[k], case_sensitive) {
                k += 1;
            }
            failure[i] = k;
        }
        Self {
            pattern,
            failure,
            case_sensitive,
        }
    }

    /// All non-overlapping matches as ascending byte ranges.
    fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        let mut matches = Vec::new();
        let m = self.pattern.len();
        if m == 0 {
            return matches;
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut q = 0usize;
        for (i, &(byte, ch)) in chars.iter().enumerate() {
            while q > 0 && !chars_match(self.pattern[q], ch, self.case_sensitive) {
                q = self.failure[q - 1];
            }
            if chars_match(self.pattern[q], ch, self.case_sensitive) {
                q += 1;
            }
            if q == m {
                matches.push(chars[i + 1 - m].0..byte + ch.len_utf8());
                q = 0;
            }
        }
        matches
    }
}

fn chars_match(a: char, b: char, case_sensitive: bool) -> bool {
    a == b || (!case_sensitive && a.to_lowercase().eq(b.to_lowercase()))
}

/// A word character for whole-word matching: Unicode alphanumerics (which
/// includes CJK ideographs) plus `_`, mirroring the regex crate's Unicode `\b`.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The char immediately before byte `at`, or `None` at the string start.
fn char_before(text: &str, at: usize) -> Option<char> {
    text.get(..at)?.chars().next_back()
}

/// The char at byte `at`, or `None` at the string end.
fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..)?.chars().next()
}

/// Whether `range` is flanked by non-word characters (or the string edges).
fn whole_word_ok(text: &str, range: &Range<usize>) -> bool {
    let before_ok = char_before(text, range.start).is_none_or(|ch| !is_word_char(ch));
    let after_ok = char_at(text, range.end).is_none_or(|ch| !is_word_char(ch));
    before_ok && after_ok
}

/// Builds the display preview for one match: the line text, truncated to at
/// most [`PREVIEW_CHARS`] chars around the match, plus the match's CHAR range
/// inside that preview. Returns `None` if `range` is not a valid slice of
/// `line` (defensive; the matchers always produce valid ranges).
fn make_preview(line: &str, range: &Range<usize>) -> Option<(String, CharRange)> {
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
    Some((preview, CharPos(preview_start)..CharPos(preview_end)))
}

/// A simple glob: `*` = any run of chars, `?` = exactly one char, everything
/// else literal. Matching is case-insensitive and anchored to the whole target.
/// A pattern containing `/` matches the root-relative path; otherwise it
/// matches the file name only.
struct Glob {
    regex: regex::Regex,
    path_scoped: bool,
}

impl Glob {
    /// Compiles `pattern`; empty (or all-whitespace) patterns are ignored.
    fn new(pattern: &str) -> Option<Self> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        let path_scoped = pattern.contains('/');
        let mut source = String::from("(?i)^");
        for ch in pattern.chars() {
            match ch {
                '*' => source.push_str(".*"),
                '?' => source.push('.'),
                _ => source.push_str(&regex::escape(&ch.to_string())),
            }
        }
        source.push('$');
        let regex = regex::Regex::new(&source).ok()?;
        Some(Self { regex, path_scoped })
    }

    fn matches(&self, relative: &str, name: &str) -> bool {
        let target = if self.path_scoped { relative } else { name };
        self.regex.is_match(target)
    }
}

/// `path` relative to `root` with forward slashes, for display and globs.
fn relative_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-search-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&path, contents).expect("write file");
    }

    fn request(root: &Path, query: &str) -> ProjectSearchRequest {
        ProjectSearchRequest {
            generation: 7,
            root: root.to_path_buf(),
            query: query.to_owned(),
            options: SearchOptions::default(),
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }

    fn relatives(result: &ProjectSearchResult) -> Vec<String> {
        result
            .files
            .iter()
            .map(|file| file.relative.clone())
            .collect()
    }

    #[test]
    fn walk_searches_only_typ_files_and_skips_hidden_and_non_typ() {
        let root = temp_dir("walk");
        write_file(&root, "a.typ", "alpha cat");
        write_file(&root, "sub/b.typ", "beta cat");
        write_file(&root, ".hidden/c.typ", "gamma cat");
        write_file(&root, ".typ-edit/recovery.json", r#"{"cat": true}"#);
        write_file(&root, "readme.md", "cat");

        let result = run(&request(&root, "cat"));

        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(relatives(&result), vec!["a.typ", "sub/b.typ"]);
        assert_eq!(result.total_matches, 2);
        assert!(!result.truncated);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn literal_case_sensitivity_toggles_matches() {
        let root = temp_dir("case");
        write_file(&root, "a.typ", "Cat cat CAT");

        let mut req = request(&root, "cat");
        req.options.case_sensitive = true;
        assert_eq!(run(&req).total_matches, 1);

        req.options.case_sensitive = false;
        assert_eq!(run(&req).total_matches, 3);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn literal_whole_word_rejects_substrings() {
        let root = temp_dir("wholeword");
        write_file(&root, "a.typ", "cat category cat");

        let mut req = request(&root, "cat");
        assert_eq!(run(&req).total_matches, 3);

        req.options.whole_word = true;
        let strict = run(&req);
        assert_eq!(strict.total_matches, 2, "{:?}", strict.files);
        let matches = &strict.files[0].matches;
        assert_eq!(matches[0].byte, BytePos(0));
        assert_eq!(matches[1].byte, BytePos(13));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn regex_matches_digit_runs_and_invalid_regex_errors() {
        let root = temp_dir("regex");
        write_file(&root, "a.typ", "a1 b22 c333");

        let mut req = request(&root, r"\d+");
        req.options.regex = true;
        let result = run(&req);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(result.total_matches, 3);
        let matches = &result.files[0].matches;
        assert_eq!((matches[0].byte.0, matches[0].byte_len), (1, 1));
        assert_eq!((matches[1].byte.0, matches[1].byte_len), (4, 2));
        assert_eq!((matches[2].byte.0, matches[2].byte_len), (8, 3));

        req.query = "(".to_owned();
        let bad = run(&req);
        assert!(bad.error.is_some());
        assert!(bad.files.is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn regex_whole_word_and_case_insensitive() {
        let root = temp_dir("regexword");
        write_file(&root, "a.typ", "cat category CAT");

        let mut req = request(&root, "cat");
        req.options.regex = true;
        req.options.whole_word = true;
        // Case-insensitive by default: `cat` and `CAT` match, `category` does not.
        assert_eq!(run(&req).total_matches, 2);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn include_and_exclude_globs_filter_by_name_and_path() {
        let root = temp_dir("globs");
        write_file(&root, "a.typ", "cat");
        write_file(&root, "sub/b.typ", "cat");
        write_file(&root, "sub/c.typ", "cat");

        let mut req = request(&root, "cat");

        req.include = vec!["*.typ".to_owned()];
        assert_eq!(
            relatives(&run(&req)),
            vec!["a.typ", "sub/b.typ", "sub/c.typ"]
        );

        req.include.clear();
        req.exclude = vec!["sub/*".to_owned()];
        assert_eq!(relatives(&run(&req)), vec!["a.typ"]);

        req.exclude.clear();
        req.include = vec!["sub/*".to_owned()];
        assert_eq!(relatives(&run(&req)), vec!["sub/b.typ", "sub/c.typ"]);

        req.include = vec!["b*".to_owned()];
        assert_eq!(relatives(&run(&req)), vec!["sub/b.typ"]);

        req.include = vec!["a.typ".to_owned()];
        assert_eq!(relatives(&run(&req)), vec!["a.typ"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn globs_are_case_insensitive() {
        let root = temp_dir("globcase");
        write_file(&root, "Report.TYP", "cat");

        let mut req = request(&root, "cat");
        req.include = vec!["report.typ".to_owned()];
        assert_eq!(relatives(&run(&req)), vec!["Report.TYP"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn size_cap_skips_oversized_files() {
        let root = temp_dir("size");
        write_file(&root, "small.typ", "cat");
        write_file(&root, "big.typ", "cat padding padding padding");

        let limits = Limits {
            max_file_bytes: 16,
            max_results: 100,
        };
        let result = run_with_limits(&request(&root, "cat"), limits);

        assert_eq!(relatives(&result), vec!["small.typ"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn result_cap_marks_truncated() {
        let root = temp_dir("cap");
        write_file(&root, "a.typ", "cat cat cat");

        let limits = Limits {
            max_file_bytes: 1024,
            max_results: 2,
        };
        let result = run_with_limits(&request(&root, "cat"), limits);

        assert!(result.truncated);
        assert_eq!(result.total_matches, 2);
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].matches.len(), 2);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn match_metadata_carries_line_byte_and_preview_range() {
        let root = temp_dir("meta");
        write_file(&root, "a.typ", "hello\nworld cat here\nbye");

        let result = run(&request(&root, "cat"));
        assert!(result.error.is_none(), "{:?}", result.error);
        let m = &result.files[0].matches[0];
        assert_eq!(m.line, 1);
        assert_eq!(m.byte, BytePos(12));
        assert_eq!(m.byte_len, 3);
        assert_eq!(m.preview, "world cat here");
        assert_eq!(m.preview_match, CharPos(6)..CharPos(9));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn multibyte_line_reports_byte_offsets_and_char_preview() {
        let root = temp_dir("cjk");
        write_file(&root, "a.typ", "abc 你好世界");

        let result = run(&request(&root, "世界"));
        let m = &result.files[0].matches[0];
        assert_eq!(m.line, 0);
        assert_eq!(m.byte, BytePos(10));
        assert_eq!(m.byte_len, 6);
        assert_eq!(m.preview, "abc 你好世界");
        assert_eq!(m.preview_match, CharPos(6)..CharPos(8));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn long_line_preview_is_truncated_around_the_match() {
        let root = temp_dir("preview");
        let line = format!("{}{}{}", "x".repeat(300), "cat", "y".repeat(50));
        write_file(&root, "a.typ", &line);

        let result = run(&request(&root, "cat"));
        let m = &result.files[0].matches[0];
        assert_eq!(m.byte, BytePos(300));
        assert_eq!(m.byte_len, 3);
        assert_eq!(m.preview.chars().count(), 240);
        assert_eq!(m.preview_match, CharPos(187)..CharPos(190));
        let around: String = m
            .preview
            .chars()
            .skip(m.preview_match.start.0)
            .take(m.preview_match.end.0 - m.preview_match.start.0)
            .collect();
        assert_eq!(around, "cat");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_query_is_an_empty_result_without_error() {
        let root = temp_dir("empty");
        write_file(&root, "a.typ", "cat");

        let result = run(&request(&root, ""));
        assert!(result.error.is_none());
        assert!(result.files.is_empty());
        assert_eq!(result.total_matches, 0);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn invalid_utf8_file_is_skipped() {
        let root = temp_dir("utf8");
        write_file(&root, "good.typ", "cat");
        fs::write(root.join("bad.typ"), [0xff, 0xfe, 0x00]).expect("write bad bytes");

        let result = run(&request(&root, "cat"));
        assert!(result.error.is_none());
        assert_eq!(relatives(&result), vec!["good.typ"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn overlong_regex_is_an_error() {
        let root = temp_dir("longregex");
        write_file(&root, "a.typ", "cat");

        let mut req = request(&root, &"a".repeat(MAX_REGEX_LEN + 1));
        req.options.regex = true;
        assert!(run(&req).error.is_some());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn result_carries_the_request_generation() {
        let root = temp_dir("gen");
        write_file(&root, "a.typ", "cat");

        assert_eq!(run(&request(&root, "cat")).generation, 7);
        fs::remove_dir_all(&root).ok();
    }
}
