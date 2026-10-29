//! Git UI integration: status-bar blame, the history/diff entry points, the
//! three git commands, and the inline-blame value handed to the editor.
//!
//! Every surface reads the shared [`GitState`](super::git::GitState) snapshot and
//! the newest worker answers; nothing here calls `git2` directly. All of it is
//! inert while `Settings::git_enabled` is off or the folder is not a repository.

use super::git::GitState;
use super::state::SessionState;
use crate::shell::tree::repo_relative;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use typ_edit_git::{BlameLine, CommitInfo};
use typ_edit_i18n::{t, t_with};

/// One open diff window.
#[derive(Clone)]
pub(super) struct DiffWindow {
    pub(super) title: String,
    pub(super) oid: String,
}

/// Session-only git UI state (history selection and the open diff window).
#[derive(Default)]
pub(super) struct GitUiState {
    /// Selected row of the history list, an index into `GitState::log`.
    pub(super) history_selected: usize,
    /// Snapshot revision the current blame request was derived from.
    pub(super) blame_revision: u64,
    /// `(repo-relative path, 1-based line)` of the last blame request.
    pub(super) blame_requested: Option<(PathBuf, u32)>,
    pub(super) diff: Option<DiffWindow>,
}

/// Seconds since the Unix epoch, or `0` when the clock is before it.
pub(super) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// The i18n key and count of the bucket `delta` seconds falls into.
pub(super) fn relative_bucket(delta: i64) -> (&'static str, i64) {
    if delta < 60 {
        ("time.just_now", 0)
    } else if delta < 3_600 {
        ("time.minutes", delta / 60)
    } else if delta < 86_400 {
        ("time.hours", delta / 3_600)
    } else if delta < 2_592_000 {
        ("time.days", delta / 86_400)
    } else if delta < 31_536_000 {
        ("time.months", delta / 2_592_000)
    } else {
        ("time.years", delta / 31_536_000)
    }
}

/// Localized relative-time label for `then` measured against `now` (Unix
/// seconds). Future timestamps clamp to "just now".
pub(super) fn relative_time(now: i64, then: i64) -> String {
    let (key, count) = relative_bucket((now - then).max(0));
    if key == "time.just_now" {
        return t(key).into_owned();
    }
    t_with(key, &[("n", &count.to_string())])
}

/// The blame entry for 1-based `line`, when present.
pub(super) fn blame_for_line(lines: &[BlameLine], line: u32) -> Option<&BlameLine> {
    lines.iter().find(|entry| entry.line == line)
}

/// The full status-bar blame text: `作者 · 相对时间 · 短 hash · 摘要`.
pub(super) fn format_blame(entry: &BlameLine, now: i64) -> String {
    format!(
        "{} · {} · {} · {}",
        entry.author,
        relative_time(now, entry.time),
        entry.short,
        entry.summary
    )
}

/// Whether a blame segment may be shown at all.
pub(super) fn blame_visible(enabled: bool, has_repo: bool, has_file: bool) -> bool {
    enabled && has_repo && has_file
}

/// The history commit hash to copy: the selected row, else the HEAD short id.
pub(super) fn pick_hash<'a>(
    log: &'a [CommitInfo],
    selected: usize,
    head_short: &'a str,
) -> Option<&'a str> {
    log.get(selected)
        .map(|commit| commit.short.as_str())
        .or_else(|| (!head_short.is_empty()).then_some(head_short))
}

impl GitState {
    /// Repository-relative path of the active file, when it is inside the repo.
    pub(super) fn active_repo_path(&self, session: &SessionState) -> Option<PathBuf> {
        let repo_root = self.repo_root.as_deref()?;
        let file = session.tabs[session.active_tab].editor.path.as_deref()?;
        repo_relative(repo_root, file).map(PathBuf::from)
    }

    /// Requests the active file's history unless it is loaded or already pending.
    pub(super) fn request_history_log(&mut self, session: &SessionState) {
        let Some(relative) = self.active_repo_path(session) else {
            return;
        };
        if self.log_path.as_deref() == Some(relative.as_path())
            || self.log_pending.as_deref() == Some(relative.as_path())
        {
            return;
        }
        self.git_request_log(Some(relative));
    }
}

impl GitUiState {
    /// 1-based logical line of caret byte `cursor_byte` in the active buffer.
    fn caret_line(&self, session: &SessionState, cursor_byte: usize) -> usize {
        session.tabs[session.active_tab]
            .editor
            .line_column(cursor_byte)
            .0
    }

    /// Drops every cached blame answer (feature off, no repo, or no file).
    fn clear_blame(&mut self, git: &mut GitState) {
        self.blame_requested = None;
        if git.blame_path.is_some() || !git.blame.is_empty() {
            git.blame.clear();
            git.blame_path = None;
        }
    }

    /// Requests blame for the active file's caret line when the target changed,
    /// the snapshot advanced, or no answer is cached. Called once per frame.
    pub(super) fn maybe_request_blame(
        &mut self,
        git: &mut GitState,
        session: &SessionState,
        cursor_byte: usize,
        enabled: bool,
    ) {
        if !enabled || git.repo_root.is_none() {
            self.clear_blame(git);
            return;
        }
        let Some(relative) = git.active_repo_path(session) else {
            self.clear_blame(git);
            return;
        };
        let revision = git.snapshot.revision;
        if self.blame_revision != revision {
            self.blame_revision = revision;
            self.blame_requested = None;
        }
        let key = (
            relative.clone(),
            self.caret_line(session, cursor_byte) as u32,
        );
        if self.blame_requested.as_ref() == Some(&key) {
            return;
        }
        let line_count = session.tabs[session.active_tab]
            .editor
            .line_count_without_trailing_newline();
        self.blame_requested = Some(key);
        git.git_request_blame(relative, Some(line_count));
    }

    /// The status-bar blame segment for 1-based `line`, when available.
    pub(super) fn status_blame(
        &self,
        git: &GitState,
        session: &SessionState,
        enabled: bool,
        line: usize,
    ) -> Option<String> {
        if !blame_visible(
            enabled,
            git.repo_root.is_some(),
            session.tabs[session.active_tab].editor.path.is_some(),
        ) {
            return None;
        }
        let relative = git.active_repo_path(session)?;
        if git.blame_path.as_deref() != Some(relative.as_path()) {
            return None;
        }
        let entry = blame_for_line(&git.blame, line as u32)?;
        Some(format_blame(entry, unix_now()))
    }

    /// The editor inline-blame value for the caret line, when enabled.
    pub(super) fn blame_inline_for_caret(
        &self,
        git: &GitState,
        session: &SessionState,
        cursor_byte: usize,
        enabled: bool,
        inline_enabled: bool,
    ) -> Option<(usize, String)> {
        if !enabled || !inline_enabled {
            return None;
        }
        let relative = git.active_repo_path(session)?;
        if git.blame_path.as_deref() != Some(relative.as_path()) {
            return None;
        }
        let line = self.caret_line(session, cursor_byte) as u32;
        let entry = blame_for_line(&git.blame, line)?;
        let text = format!(
            "  · {} · {}",
            entry.author,
            relative_time(unix_now(), entry.time)
        );
        Some((line as usize - 1, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: u32, time: i64) -> BlameLine {
        BlameLine {
            line,
            oid: "a".repeat(40),
            short: "aaaaaaa".to_owned(),
            author: "Alice".to_owned(),
            time,
            summary: "add chapter".to_owned(),
        }
    }

    fn commit(short: &str) -> CommitInfo {
        CommitInfo {
            oid: "b".repeat(40),
            short: short.to_owned(),
            author: "Bob".to_owned(),
            time: 0,
            summary: "c".to_owned(),
        }
    }

    #[test]
    fn caret_line_uses_the_buffer_line_index() {
        use crate::app::state::EditorTab;
        use typ_edit_editor::EditorState;

        let mut editor = EditorState::new();
        editor.set_text("α\nbbb\nc".to_owned());
        let session = SessionState {
            tabs: vec![EditorTab {
                id: 0,
                editor,
                dirty: false,
            }],
            active_tab: 0,
            close_prompt: None,
            allow_close: false,
            next_tab_id: 1,
        };
        let ui = GitUiState::default();
        assert_eq!(ui.caret_line(&session, 0), 1, "start is line 1");
        assert_eq!(ui.caret_line(&session, 2), 1, "newline stays on line 1");
        assert_eq!(ui.caret_line(&session, 3), 2, "after the newline is line 2");
        assert_eq!(ui.caret_line(&session, 7), 3, "the last char is line 3");
        assert_eq!(
            ui.caret_line(&session, usize::MAX),
            3,
            "out-of-range bytes clamp to the last line"
        );
    }

    #[test]
    fn relative_bucket_covers_now_minutes_hours_days_months_years() {
        assert_eq!(relative_bucket(0), ("time.just_now", 0));
        assert_eq!(relative_bucket(59), ("time.just_now", 0));
        assert_eq!(relative_bucket(120), ("time.minutes", 2));
        assert_eq!(relative_bucket(7_200), ("time.hours", 2));
        assert_eq!(relative_bucket(172_800), ("time.days", 2));
        assert_eq!(relative_bucket(5_184_000), ("time.months", 2));
        assert_eq!(relative_bucket(63_072_000), ("time.years", 2));
    }

    #[test]
    fn relative_time_renders_the_default_language_units() {
        let now = 10_000_000_000;
        assert_eq!(relative_time(now, now), "刚刚");
        assert_eq!(relative_time(now, now - 120), "2 分钟前");
        assert_eq!(relative_time(now, now - 7_200), "2 小时前");
        assert_eq!(relative_time(now, now - 172_800), "2 天前");
        assert_eq!(relative_time(now, now - 5_184_000), "2 个月前");
        assert_eq!(relative_time(now, now - 63_072_000), "2 年前");
    }

    #[test]
    fn relative_time_clamps_future_timestamps_to_just_now() {
        assert_eq!(relative_time(100, 1_000), "刚刚");
        assert_eq!(relative_time(1_000, 1_000), "刚刚");
    }

    #[test]
    fn blame_for_line_matches_the_one_based_line() {
        let lines = [entry(1, 0), entry(3, 0)];
        assert_eq!(blame_for_line(&lines, 1).map(|e| e.line), Some(1));
        assert_eq!(blame_for_line(&lines, 3).map(|e| e.line), Some(3));
        assert!(blame_for_line(&lines, 2).is_none());
        assert!(blame_for_line(&[], 1).is_none());
    }

    #[test]
    fn format_blame_orders_author_time_hash_summary() {
        let text = format_blame(&entry(1, 0), 0);
        assert!(
            text.starts_with("Alice · 刚刚 · aaaaaaa · add chapter"),
            "got {text:?}"
        );
    }

    #[test]
    fn pick_hash_prefers_the_selected_commit_then_head() {
        let log = [commit("1111111"), commit("2222222")];
        assert_eq!(pick_hash(&log, 1, "head123"), Some("2222222"));
        assert_eq!(
            pick_hash(&log, 9, "head123"),
            Some("head123"),
            "an out-of-range selection falls back to HEAD"
        );
        assert_eq!(pick_hash(&[], 0, "head123"), Some("head123"));
        assert_eq!(
            pick_hash(&[], 0, ""),
            None,
            "no commit and no HEAD yields none"
        );
    }

    #[test]
    fn every_relative_bucket_key_resolves() {
        // One representative delta per branch: just-now, minutes, hours, days,
        // months, years.
        for delta in [0, 120, 7_200, 172_800, 5_184_000, 63_072_000] {
            let (key, _count) = relative_bucket(delta);
            assert_ne!(&*t(key), key, "unresolved i18n key: {key}");
        }
    }
}
