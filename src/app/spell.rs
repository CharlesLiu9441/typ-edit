//! App-side spell checking: the editor's issue list, request bookkeeping, the
//! persisted user dictionary and the session-only ignore set.
//!
//! The engine owns the actual lint ([`typ_edit_engine`]); this module mirrors
//! the parse pipeline instead: a debounced request carries the shared buffer
//! snapshot, and only results for the active tab at a fresh-enough generation
//! may replace the applied issue list. User-dictionary and ignored words are
//! filtered out here, so the editor only ever paints issues that still stand.

use super::document::DocumentController;
use super::state::EditorTab;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use typ_edit_engine::Engine;
use typ_edit_model::{SpellIssue, SpellRequest};

/// Name of the user-dictionary file inside the config directory.
const DICTIONARY_FILE: &str = "dictionary.txt";

/// Session spell state: the filtered issues shown by the editor, the request
/// bookkeeping that mirrors the parse pipeline, the user dictionary loaded from
/// disk, and the words ignored for this session only.
pub(super) struct SpellState {
    /// Issues that survived the dictionary/ignore filter, handed to the editor.
    pub(super) issues: Vec<SpellIssue>,
    /// Generation of the last applied result (same counter as parses).
    pub(super) generation: u64,
    /// Whether the worker holds a snapshot for the active tab.
    pub(super) in_flight: bool,
    /// An edit (or tab switch) arrived while the state was not clean.
    pub(super) dirty: bool,
    /// Whether the feature was enabled on the previous pump, so a toggle-on can
    /// request a pass without waiting for an edit.
    pub(super) was_enabled: bool,
    /// Lowercased user-dictionary words; persisted to disk.
    pub(super) dictionary: BTreeSet<String>,
    /// Lowercased words ignored for this session; never persisted.
    pub(super) ignored: HashSet<String>,
}

impl SpellState {
    /// Loads the session spell state, reading the user dictionary from disk.
    pub(super) fn load() -> Self {
        Self {
            issues: Vec::new(),
            generation: 0,
            in_flight: false,
            dirty: false,
            was_enabled: false,
            dictionary: load_dictionary_from_disk(),
            ignored: HashSet::new(),
        }
    }
}

/// Whether the debounced spell request is due: the feature is enabled, the
/// buffer is Typst, an edit is pending, no pass is in flight, and the last edit
/// has been idle for at least [`SPELL_DEBOUNCE`]. A never-edited buffer has no
/// idle stamp and is due immediately.
pub(super) fn spell_due(
    enabled: bool,
    plain_text: bool,
    dirty: bool,
    in_flight: bool,
    idle: Option<Duration>,
) -> bool {
    enabled
        && !plain_text
        && dirty
        && !in_flight
        && idle.is_none_or(|elapsed| elapsed >= SPELL_DEBOUNCE)
}

/// Idle time after an edit before the debounced spell pass is requested.
pub(super) const SPELL_DEBOUNCE: Duration = Duration::from_millis(300);

/// Whether the spell feature just switched from off to on. True for exactly one
/// pump: `was_enabled` is set to the current state at the end of every pump, so
/// the toggle-on handling (mark dirty + request a fresh parse) cannot loop.
pub(super) fn spell_toggled_on(enabled: bool, was_enabled: bool) -> bool {
    enabled && !was_enabled
}

/// Parses a dictionary file: one word per line, trimmed, empty lines skipped,
/// everything lowercased for case-insensitive matching.
pub(super) fn load_dictionary(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Serializes a dictionary as sorted, `\n`-joined words (the on-disk format).
pub(super) fn dictionary_text(dictionary: &BTreeSet<String>) -> String {
    dictionary
        .iter()
        .cloned()
        .collect::<Vec<String>>()
        .join("\n")
}

/// Drops issues whose word is in the user dictionary or the session ignore set.
/// Matching is case-insensitive (both sides lowercased).
pub(super) fn filter_issues(
    issues: Vec<SpellIssue>,
    dictionary: &BTreeSet<String>,
    ignored: &HashSet<String>,
) -> Vec<SpellIssue> {
    issues
        .into_iter()
        .filter(|issue| {
            let word = issue.word.to_lowercase();
            !dictionary.contains(&word) && !ignored.contains(&word)
        })
        .collect()
}

/// The user-dictionary file path (`config_dir()/dictionary.txt`); `None` when
/// the platform config directory cannot be resolved.
pub(super) fn dictionary_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join(DICTIONARY_FILE))
}

/// Reads and parses the user dictionary; an absent/unreadable file is empty.
pub(super) fn load_dictionary_from_disk() -> BTreeSet<String> {
    dictionary_path()
        .and_then(|path| crate::platform::fs_ops::read_to_string(&path).ok())
        .map(|text| load_dictionary(&text))
        .unwrap_or_default()
}

/// Rewrites the whole user dictionary. A missing config directory is a no-op;
/// a write failure is returned so the caller can toast it.
pub(super) fn save_dictionary(dictionary: &BTreeSet<String>) -> std::io::Result<()> {
    let Some(path) = dictionary_path() else {
        return Ok(());
    };
    crate::platform::fs_ops::write_atomic(&path, dictionary_text(dictionary).as_bytes())
}

impl SpellState {
    /// Marks the shown issues stale after an edit, so the debounce re-runs.
    pub(super) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Clears the in-flight flag without an applied result (worker death), so
    /// the UI is never stuck waiting on a request that will never answer.
    pub(super) fn clear_in_flight(&mut self) {
        self.in_flight = false;
    }

    /// Drops the previous buffer's spell ranges on a tab switch: clears the
    /// issues and orphans any in-flight pass, so the new tab's request is sent
    /// now instead of being deferred. `was_enabled` is left untouched.
    pub(super) fn reset_for_new_document(&mut self) {
        self.issues.clear();
        self.in_flight = false;
        self.dirty = false;
    }

    /// Records whether the feature was enabled on this pump, so the next pump
    /// can detect a fresh off-to-on transition.
    pub(super) fn note_enabled(&mut self, enabled: bool) {
        self.was_enabled = enabled;
    }

    /// Applies an accepted pass: records its generation, filters the issues
    /// through the dictionary/ignore set and clears the in-flight flag.
    pub(super) fn apply_result(&mut self, generation: u64, issues: Vec<SpellIssue>) {
        self.generation = generation;
        self.issues = filter_issues(issues, &self.dictionary, &self.ignored);
        self.in_flight = false;
    }

    /// Requests a spell pass for the active buffer unless the feature is off or
    /// the file is plain text. While a pass is in flight the request is only
    /// queued (`dirty`), so an internal edit never forces a second snapshot.
    pub(super) fn trigger_spell(
        &mut self,
        engine: &Engine,
        doc: &DocumentController,
        tab: &EditorTab,
        enabled: bool,
        plain_text: bool,
    ) {
        if !enabled || plain_text {
            return;
        }
        // The request carries the parse worker's prose segments, so a parse that
        // has not caught up yet has stale/empty segments. Leave `dirty` set and
        // make no request; the debounce re-fires once the parse lands.
        if doc.parse_generation != doc.edit_generation {
            return;
        }
        if self.in_flight {
            self.dirty = true;
            return;
        }
        self.in_flight = true;
        self.dirty = false;
        let text = tab.editor.text_snapshot();
        let segments = Arc::new(doc.parse.prose.clone());
        engine.request_spell(SpellRequest::Check {
            generation: doc.edit_generation,
            tab: tab.id,
            text,
            segments,
        });
    }

    /// Adds `word` to the user dictionary, persists it and re-filters the shown
    /// issues so its underline disappears at once. The word is kept in memory
    /// even when the persist fails; the caller surfaces that error.
    pub(super) fn add_to_dictionary(&mut self, word: &str) -> std::io::Result<()> {
        self.dictionary.insert(word.to_lowercase());
        let saved = save_dictionary(&self.dictionary);
        self.refilter_spell_issues();
        saved
    }

    /// Ignores `word` for this session and re-filters the shown issues.
    pub(super) fn ignore_spell_word(&mut self, word: &str) {
        self.ignored.insert(word.to_lowercase());
        self.refilter_spell_issues();
    }

    /// Re-applies the dictionary/ignore filter to the currently shown issues.
    fn refilter_spell_issues(&mut self) {
        self.issues = filter_issues(
            std::mem::take(&mut self.issues),
            &self.dictionary,
            &self.ignored,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(word: &str, start: usize) -> SpellIssue {
        SpellIssue {
            range: typ_edit_model::BytePos(start)..typ_edit_model::BytePos(start + word.len()),
            word: word.to_owned(),
            suggestions: Vec::new(),
        }
    }

    #[test]
    fn dictionary_round_trips_through_text() {
        let text = "apple\nBanana\n\n  cherry  \n";
        let set = load_dictionary(text);
        assert_eq!(
            set.iter().cloned().collect::<Vec<String>>(),
            vec!["apple".to_owned(), "banana".to_owned(), "cherry".to_owned()]
        );
        assert_eq!(dictionary_text(&set), "apple\nbanana\ncherry");
        assert_eq!(load_dictionary(&dictionary_text(&set)), set);
    }

    #[test]
    fn load_dictionary_trims_skips_empty_and_lowercases() {
        let set = load_dictionary("  Word \n\t\nTWO\t\n");
        assert!(set.contains("word"));
        assert!(set.contains("two"));
        assert_eq!(set.len(), 2, "blank lines are skipped");
    }

    #[test]
    fn filter_drops_dictionary_and_ignored_words_case_insensitively() {
        let issues = vec![issue("Teh", 0), issue("WROLD", 4), issue("acme", 10)];
        let mut dictionary = BTreeSet::new();
        dictionary.insert("teh".to_owned());
        let mut ignored = HashSet::new();
        ignored.insert("wrold".to_owned());
        let kept = filter_issues(issues, &dictionary, &ignored);
        assert_eq!(kept.len(), 1, "only the unrelated issue survives");
        assert_eq!(kept[0].word, "acme");
    }

    #[test]
    fn filter_keeps_every_unrelated_issue() {
        let issues = vec![issue("alpha", 0), issue("beta", 6)];
        let kept = filter_issues(issues.clone(), &BTreeSet::new(), &HashSet::new());
        assert_eq!(kept, issues);
    }

    #[test]
    fn spell_due_waits_out_the_debounce() {
        assert!(!spell_due(
            true,
            false,
            true,
            false,
            Some(Duration::from_millis(299))
        ));
        assert!(spell_due(
            true,
            false,
            true,
            false,
            Some(Duration::from_millis(300))
        ));
        // No edit stamp yet (freshly opened tab): due immediately.
        assert!(spell_due(true, false, true, false, None));
    }

    #[test]
    fn spell_due_is_false_when_disabled_plain_clean_or_in_flight() {
        assert!(!spell_due(false, false, true, false, None));
        assert!(!spell_due(true, true, true, false, None));
        assert!(!spell_due(true, false, false, false, None));
        assert!(!spell_due(true, false, true, true, None));
    }

    #[test]
    fn spell_toggled_on_fires_only_on_the_off_to_on_transition() {
        assert!(spell_toggled_on(true, false));
        assert!(
            !spell_toggled_on(true, true),
            "an already-enabled feature must not re-trigger"
        );
        assert!(!spell_toggled_on(false, false));
        assert!(
            !spell_toggled_on(false, true),
            "turning the feature off must not re-trigger"
        );
    }

    #[test]
    fn dictionary_path_targets_the_config_directory_file() {
        // The platform config dir may be unavailable in exotic environments;
        // when it resolves, the file must be the one we read and write.
        if let Some(path) = dictionary_path() {
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some(DICTIONARY_FILE)
            );
        }
    }
}
