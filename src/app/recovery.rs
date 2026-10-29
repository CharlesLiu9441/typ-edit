//! Project-local crash recovery: a debounced autosave snapshot of unsaved tabs,
//! a restore prompt on project open / app start, and cleanup on save/exit.

use super::EditorTab;
use super::state::{SessionState, ToastMsg};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use typ_edit_i18n::{t, t_with};
use typ_edit_model::TextSnapshot;

/// Snapshot format version understood by this build.
pub(super) const SNAPSHOT_VERSION: u32 = 1;
/// Snapshot file name inside the project's `.typ-edit` directory.
const SNAPSHOT_FILE: &str = "recovery.json";
/// Snapshot file name for untitled buffers when no project is open.
const UNTITLED_SNAPSHOT_FILE: &str = "recovery-untitled.json";
/// Debounce after the last edit before an autosave snapshot is written.
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);
/// Periodic safety interval that rewrites the snapshot while tabs stay dirty.
const AUTOSAVE_PERIOD: Duration = Duration::from_secs(30);
/// Largest tab content included in a snapshot (5 MiB).
pub(super) const MAX_SNAPSHOT_CONTENT: usize = 5 * 1024 * 1024;

/// A persisted autosave snapshot. `version` gates format changes; corrupt JSON
/// and unknown versions load as absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RecoverySnapshot {
    pub(super) version: u32,
    pub(super) saved_at_ms: u64,
    pub(super) tabs: Vec<RecoveryTab>,
}

/// One dirty tab inside a [`RecoverySnapshot`]. `path` is relative to the
/// project root when inside it, otherwise absolute; `None` is an untitled tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RecoveryTab {
    pub(super) path: Option<String>,
    pub(super) content: String,
    pub(super) active: bool,
}

/// One snapshot entry that still needs recovery, prepared for the prompt.
pub(super) struct RecoveryCandidate {
    pub(super) resolved_path: Option<PathBuf>,
    pub(super) content: String,
    pub(super) active: bool,
}

impl RecoveryCandidate {
    /// Label shown in the restore list: the resolved path, or the untitled name.
    pub(super) fn label(&self) -> String {
        match &self.resolved_path {
            Some(path) => path.display().to_string(),
            None => t("app.unnamed").into_owned(),
        }
    }
}

/// An unanswered restore prompt. While it is pending, the autosave writer stays
/// off so a fresh snapshot cannot overwrite unrecovered data.
pub(super) struct RecoveryPrompt {
    pub(super) snapshot_path: PathBuf,
    pub(super) saved_at_ms: u64,
    pub(super) candidates: Vec<RecoveryCandidate>,
    /// Main file whose auto-open was deferred until the prompt resolves.
    pub(super) deferred_main: Option<PathBuf>,
}

/// Which button the restore prompt resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryChoice {
    Restore,
    Later,
    Discard,
}

/// Runtime crash-recovery state, driven from [`Recovery::tick`].
#[derive(Default)]
pub(super) struct Recovery {
    /// Time of the last dirty-set event; drives the debounce.
    pub(super) last_change: Option<Instant>,
    /// Time the current snapshot was last written; drives the periodic safety.
    pub(super) last_snapshot: Option<Instant>,
    /// Snapshot file this session writes and cleans up, if any.
    pub(super) snapshot_path: Option<PathBuf>,
    /// The user chose Later: the existing snapshot is left untouched and the
    /// writer stays off for the rest of the session.
    pub(super) unresolved_existing: bool,
    /// Pending restore prompt, if any.
    pub(super) prompt: Option<RecoveryPrompt>,
    /// Skip-set that produced the last "too large" toast, for once-per-change.
    pub(super) last_skip_toast: Option<Vec<String>>,
    /// Whether the last snapshot write failed (once-per-failure-cycle toast).
    pub(super) write_failed: bool,
    /// Background snapshot writer, created on the first autosave.
    writer: Option<SnapshotWriter>,
    /// Whether a snapshot job is currently in flight.
    writer_busy: bool,
}

impl Recovery {
    /// Records an edit (or a programmatic dirty-set) for the debounce.
    pub(super) fn note_change(&mut self) {
        self.last_change = Some(Instant::now());
    }

    /// Whether writing or deleting the snapshot must be suppressed.
    fn writer_off(&self) -> bool {
        self.unresolved_existing || self.prompt.is_some()
    }
}

/// One dirty tab handed to the snapshot worker. The text is an O(1) rope
/// snapshot; the worker materializes it on its own thread.
struct SnapshotTabJob {
    path: Option<PathBuf>,
    content: TextSnapshot,
    active: bool,
}

/// One snapshot request: build, serialize, and atomically write on the worker.
struct SnapshotJob {
    root: Option<PathBuf>,
    target: PathBuf,
    saved_at_ms: u64,
    saved_at: Instant,
    tabs: Vec<SnapshotTabJob>,
}

/// Worker outcome applied on the UI thread on a later tick.
struct SnapshotResult {
    target: PathBuf,
    saved_at: Instant,
    skipped: Vec<String>,
    empty: bool,
    error: Option<String>,
}

/// Latest-only snapshot pipeline: the UI hands over O(1) text handles and the
/// worker owns materialization, JSON serialization, and the atomic rename.
struct SnapshotWriter {
    tx: Sender<SnapshotJob>,
    rx: Receiver<SnapshotResult>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SnapshotWriter {
    /// Spawns the writer thread. A failed spawn leaves `tx` disconnected, which
    /// [`Recovery::queue_snapshot`] reports as a normal write failure.
    fn spawn() -> Self {
        let (tx, jobs) = bounded(1);
        let (results, rx) = bounded(1);
        let spawned = std::thread::Builder::new()
            .name("typ-edit-recovery".to_owned())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if results.send(run_snapshot_job(job)).is_err() {
                        break;
                    }
                }
            });
        let join = match spawned {
            Ok(join) => Some(join),
            Err(err) => {
                log::error!("cannot start recovery snapshot worker: {err}");
                None
            }
        };
        Self { tx, rx, join }
    }
}

/// Builds and writes one snapshot entirely on the worker thread.
fn run_snapshot_job(job: SnapshotJob) -> SnapshotResult {
    let entries: Vec<(Option<PathBuf>, String, bool, bool)> = job
        .tabs
        .into_iter()
        .map(|tab| (tab.path, tab.content.materialize(), true, tab.active))
        .collect();
    let tabs: Vec<SnapshotTab<'_>> = entries
        .iter()
        .map(|(path, content, dirty, active)| SnapshotTab {
            path: path.as_deref(),
            content: content.as_str(),
            dirty: *dirty,
            active: *active,
        })
        .collect();
    let build = build_snapshot(&tabs, job.root.as_deref(), job.saved_at_ms);
    if build.snapshot.tabs.is_empty() {
        return SnapshotResult {
            target: job.target,
            saved_at: job.saved_at,
            skipped: build.skipped,
            empty: true,
            error: None,
        };
    }
    let error = write_snapshot_file(&job.target, &build.snapshot)
        .err()
        .map(|err| err.to_string());
    SnapshotResult {
        target: job.target,
        saved_at: job.saved_at,
        skipped: build.skipped,
        empty: false,
        error,
    }
}

/// One tab considered for a snapshot. The content is borrowed so the builder
/// stays free of the editor types and is unit-testable.
pub(super) struct SnapshotTab<'a> {
    pub(super) path: Option<&'a Path>,
    pub(super) content: &'a str,
    pub(super) dirty: bool,
    pub(super) active: bool,
}

/// Result of [`build_snapshot`]: the snapshot and the names skipped for size.
pub(super) struct SnapshotBuild {
    pub(super) snapshot: RecoverySnapshot,
    pub(super) skipped: Vec<String>,
}

/// Builds a snapshot of only the dirty tabs, skipping any content larger than
/// [`MAX_SNAPSHOT_CONTENT`] and recording the skipped file names.
pub(super) fn build_snapshot(
    tabs: &[SnapshotTab<'_>],
    root: Option<&Path>,
    saved_at_ms: u64,
) -> SnapshotBuild {
    let mut snapshot_tabs = Vec::new();
    let mut skipped = Vec::new();
    for tab in tabs {
        if !tab.dirty {
            continue;
        }
        if tab.content.len() > MAX_SNAPSHOT_CONTENT {
            skipped.push(skip_name(tab.path));
            continue;
        }
        snapshot_tabs.push(RecoveryTab {
            path: tab.path.map(|path| encode_path(path, root)),
            content: tab.content.to_owned(),
            active: tab.active,
        });
    }
    SnapshotBuild {
        snapshot: RecoverySnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms,
            tabs: snapshot_tabs,
        },
        skipped,
    }
}

/// Display name of a skipped tab, falling back to the untitled name.
fn skip_name(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| t("app.unnamed").into_owned())
}

/// Encodes a path: relative to `root` with forward slashes when inside it,
/// otherwise an absolute forward-slashed path.
pub(super) fn encode_path(path: &Path, root: Option<&Path>) -> String {
    if let Some(root) = root
        && let Ok(relative) = path.strip_prefix(root)
    {
        return slash(relative);
    }
    slash(path)
}

/// Resolves a stored snapshot path back to a `PathBuf`.
pub(super) fn decode_path(stored: &str, root: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(stored);
    if path.is_absolute() {
        return path;
    }
    match root {
        Some(root) => root.join(path),
        None => path,
    }
}

/// Normalizes a path to a forward-slashed string (lossy for non-UTF-8 paths).
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Parses a snapshot, tolerating a UTF-8 BOM; `None` for corrupt JSON or an
/// unknown `version`.
pub(super) fn parse_snapshot(text: &str) -> Option<RecoverySnapshot> {
    let snapshot: RecoverySnapshot =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    (snapshot.version == SNAPSHOT_VERSION).then_some(snapshot)
}

/// Loads a snapshot from disk; `None` when absent, unreadable, or invalid.
pub(super) fn load_snapshot(path: &Path) -> Option<RecoverySnapshot> {
    let text = crate::platform::fs_ops::read_to_string(path).ok()?;
    parse_snapshot(&text)
}

/// Atomically writes `snapshot` to `path` (`path.tmp`, then rename), creating
/// the parent directory as needed.
pub(super) fn write_snapshot_file(path: &Path, snapshot: &RecoverySnapshot) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(snapshot).map_err(std::io::Error::other)?;
    crate::platform::fs_ops::write_atomic(path, json.as_bytes())
}

/// Milliseconds since the Unix epoch.
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Last-modified time of `path` in milliseconds since the epoch.
pub(super) fn file_mtime_ms(path: &Path) -> Option<u64> {
    crate::platform::fs_ops::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

/// Whether a snapshot entry still needs recovery: an untitled tab, a file that
/// no longer exists, or a file the snapshot is newer than.
pub(super) fn entry_qualifies(
    stored_path: Option<&str>,
    saved_at_ms: u64,
    mtime_ms: Option<u64>,
) -> bool {
    if stored_path.is_none() {
        return true;
    }
    match mtime_ms {
        None => true,
        Some(mtime) => saved_at_ms > mtime,
    }
}

/// Filters a snapshot down to the entries that still need recovery.
pub(super) fn qualifying_candidates(
    snapshot: &RecoverySnapshot,
    root: Option<&Path>,
) -> Vec<RecoveryCandidate> {
    let mut candidates = Vec::new();
    for tab in &snapshot.tabs {
        let resolved = tab.path.as_deref().map(|stored| decode_path(stored, root));
        let mtime = resolved.as_deref().and_then(file_mtime_ms);
        if entry_qualifies(tab.path.as_deref(), snapshot.saved_at_ms, mtime) {
            candidates.push(RecoveryCandidate {
                resolved_path: resolved,
                content: tab.content.clone(),
                active: tab.active,
            });
        }
    }
    candidates
}

/// Project snapshot path: `<root>/.typ-edit/recovery.json`.
pub(super) fn project_snapshot_path(root: &Path) -> PathBuf {
    root.join(crate::workspace::SNAPSHOT_DIR)
        .join(SNAPSHOT_FILE)
}

/// Untitled snapshot path in the app config directory, if it resolves.
pub(super) fn untitled_snapshot_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join(UNTITLED_SNAPSHOT_FILE))
}

/// Formats a Unix-millisecond timestamp as `YYYY-MM-DD HH:MM` (UTC).
pub(super) fn format_utc_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Days since the Unix epoch to a civil `(year, month, day)` (Howard Hinnant's
/// algorithm), so timestamps need no date dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// One restore step the caller applies: open a file (or a fresh tab) and fill it
/// with the recovered content.
pub(in crate::app) enum RecoveryRestoreStep {
    /// A resolved path: the caller opens it, then sets its tab's content.
    File {
        path: PathBuf,
        content: String,
        active: bool,
    },
    /// An untitled buffer: the caller creates a fresh recovery tab.
    Untitled { content: String, active: bool },
}

/// App-only effects produced by a recovery transition, applied by the caller in
/// order.
#[derive(Default)]
pub(in crate::app) struct RecoveryOutcome {
    /// Toasts to raise, in order.
    pub(in crate::app) toasts: Vec<ToastMsg>,
    /// Restore steps (empty unless the user chose Restore).
    pub(in crate::app) restore: Vec<RecoveryRestoreStep>,
    /// A deferred main file to open after the transition.
    pub(in crate::app) open_main: Option<PathBuf>,
}

impl Recovery {
    /// Debounced/periodic autosave and cleanup, called once per frame from
    /// [`eframe::App::logic`] so it also runs while the window is hidden.
    /// Returns the toasts the caller must raise, in order.
    pub(in crate::app) fn tick(
        &mut self,
        ctx: &egui::Context,
        session: &SessionState,
        root: Option<&Path>,
        enabled: bool,
    ) -> RecoveryOutcome {
        let mut out = RecoveryOutcome::default();
        if !enabled {
            // A job queued just before the toggle may still land; drain it and
            // delete the file it wrote before returning.
            self.drain_snapshot_results(&mut out.toasts);
            if self.writer_busy {
                ctx.request_repaint_after(Duration::from_millis(100));
                return out;
            }
            if !self.writer_off()
                && let Some(path) = self.snapshot_path.take()
                && let Err(err) = crate::platform::fs_ops::remove_file(&path)
            {
                log::warn!("cannot remove recovery snapshot {}: {err}", path.display());
            }
            return out;
        }
        if self.writer_off() {
            return out;
        }
        self.drain_snapshot_results(&mut out.toasts);
        if self.writer_busy {
            // The worker has no egui context, so poll for its result instead of
            // waiting for the next debounce deadline.
            ctx.request_repaint_after(Duration::from_millis(100));
            return out;
        }
        let now = Instant::now();
        if !session.tabs.iter().any(|tab| tab.dirty) {
            if let Some(path) = self.snapshot_path.take()
                && let Err(err) = crate::platform::fs_ops::remove_file(&path)
            {
                log::warn!("cannot remove recovery snapshot {}: {err}", path.display());
            }
            return out;
        }
        // Tabs can be dirtied without a keystroke (restore); seed the debounce
        // rather than writing immediately.
        if self.last_change.is_none() {
            self.last_change = Some(now);
            return out;
        }
        let due_debounce = self.last_change.is_none_or(|changed| {
            now.saturating_duration_since(changed) >= AUTOSAVE_DEBOUNCE
                && self.last_snapshot.is_none_or(|saved| saved < changed)
        });
        let due_periodic = self
            .last_snapshot
            .is_some_and(|saved| now.saturating_duration_since(saved) >= AUTOSAVE_PERIOD);
        if due_debounce || due_periodic {
            self.queue_snapshot(now, session, root, &mut out.toasts);
        }
        // A repaint is otherwise not guaranteed once the UI goes idle, so wake
        // at the next debounce/periodic deadline, or sooner while a job runs.
        let wait = if self.writer_busy {
            Duration::from_millis(100)
        } else {
            self.next_snapshot_wait(now)
        };
        ctx.request_repaint_after(wait);
        out
    }

    /// Time until the next debounce or periodic snapshot check is due.
    fn next_snapshot_wait(&self, now: Instant) -> Duration {
        let mut wait = AUTOSAVE_PERIOD;
        if let Some(changed) = self.last_change
            && self.last_snapshot.is_none_or(|saved| saved < changed)
        {
            wait = AUTOSAVE_DEBOUNCE.saturating_sub(now.saturating_duration_since(changed));
        }
        if let Some(saved) = self.last_snapshot {
            wait = wait.min(AUTOSAVE_PERIOD.saturating_sub(now.saturating_duration_since(saved)));
        }
        wait
    }

    /// Captures dirty tabs as O(1) text handles and queues one worker job.
    fn queue_snapshot(
        &mut self,
        now: Instant,
        session: &SessionState,
        root: Option<&Path>,
        toasts: &mut Vec<ToastMsg>,
    ) {
        let active = session.active_tab;
        let tabs: Vec<SnapshotTabJob> = session
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.dirty)
            .map(|(index, tab)| SnapshotTabJob {
                path: tab.editor.path.clone(),
                content: tab.editor.text_snapshot(),
                active: index == active,
            })
            .collect();
        let target = match root {
            Some(root) => project_snapshot_path(root),
            None => match untitled_snapshot_path() {
                Some(path) => path,
                None => {
                    self.last_snapshot = Some(now);
                    return;
                }
            },
        };
        let job = SnapshotJob {
            root: root.map(Path::to_path_buf),
            target,
            saved_at_ms: now_ms(),
            saved_at: now,
            tabs,
        };
        if self.writer.is_none() {
            self.writer = Some(SnapshotWriter::spawn());
        }
        let sent = match self.writer.as_ref() {
            Some(writer) => writer.tx.try_send(job),
            None => return,
        };
        match sent {
            Ok(()) => self.writer_busy = true,
            // A result is waiting to be drained; keep polling.
            Err(TrySendError::Full(_)) => self.writer_busy = true,
            Err(TrySendError::Disconnected(_)) => {
                self.writer = None;
                self.last_snapshot = Some(now);
                if !self.write_failed {
                    self.write_failed = true;
                    toasts.push(ToastMsg::error(t("recovery.write_failed")));
                }
            }
        }
    }

    /// Applies every completed worker job: skip/write toasts and snapshot path.
    fn drain_snapshot_results(&mut self, toasts: &mut Vec<ToastMsg>) {
        let mut results = Vec::new();
        if let Some(writer) = &self.writer {
            while let Ok(result) = writer.rx.try_recv() {
                results.push(result);
            }
        }
        for result in results {
            self.writer_busy = false;
            self.apply_snapshot_result(result, toasts);
        }
    }

    /// Applies one worker outcome to the UI-side state.
    fn apply_snapshot_result(&mut self, result: SnapshotResult, toasts: &mut Vec<ToastMsg>) {
        if result.skipped.is_empty() {
            self.last_skip_toast = None;
        } else if self.last_skip_toast.as_deref() != Some(result.skipped.as_slice()) {
            let name = result.skipped.first().cloned().unwrap_or_default();
            toasts.push(ToastMsg::warning(t_with(
                "recovery.too_large",
                &[("name", name.as_str())],
            )));
            self.last_skip_toast = Some(result.skipped.clone());
        }
        if result.empty {
            // Every dirty tab is oversized: back off instead of spinning.
            self.last_snapshot = Some(result.saved_at);
            return;
        }
        if let Some(err) = result.error {
            log::error!(
                "cannot write recovery snapshot {}: {err}",
                result.target.display()
            );
            self.last_snapshot = Some(result.saved_at);
            if !self.write_failed {
                self.write_failed = true;
                toasts.push(ToastMsg::error(t("recovery.write_failed")));
            }
            return;
        }
        if self
            .snapshot_path
            .as_deref()
            .is_some_and(|old| old != result.target)
            && let Some(old) = self.snapshot_path.take()
            && let Err(err) = crate::platform::fs_ops::remove_file(&old)
        {
            log::warn!(
                "cannot remove stale recovery snapshot {}: {err}",
                old.display()
            );
        }
        self.snapshot_path = Some(result.target);
        self.last_snapshot = Some(result.saved_at);
        self.write_failed = false;
    }

    /// Stops the writer and applies any result it already produced. Graceful
    /// exit calls this before deleting the snapshot so an in-flight job cannot
    /// recreate the file after cleanup.
    pub(in crate::app) fn shutdown_writer(&mut self) {
        let Some(writer) = self.writer.take() else {
            return;
        };
        let SnapshotWriter { tx, rx, join } = writer;
        drop(tx);
        if let Some(join) = join
            && join.join().is_err()
        {
            log::error!("recovery snapshot worker panicked");
        }
        let mut toasts = Vec::new();
        while let Ok(result) = rx.try_recv() {
            self.writer_busy = false;
            self.apply_snapshot_result(result, &mut toasts);
        }
        self.writer_busy = false;
    }

    /// Checks for a project snapshot; returns `true` when a prompt was armed,
    /// in which case the caller must not auto-open `deferred_main`.
    pub(in crate::app) fn arm_project_recovery(
        &mut self,
        root: &Path,
        deferred_main: Option<PathBuf>,
    ) -> bool {
        let path = project_snapshot_path(root);
        self.arm_recovery(path, Some(root.to_path_buf()), deferred_main)
    }

    /// At startup with no project open, checks the untitled snapshot.
    pub(in crate::app) fn maybe_prompt_startup_recovery(&mut self, has_root: bool) {
        if has_root {
            return;
        }
        if let Some(path) = untitled_snapshot_path() {
            self.arm_recovery(path, None, None);
        }
    }

    /// Loads `path` and arms the prompt when at least one entry qualifies; a
    /// snapshot whose entries are all stale is deleted.
    fn arm_recovery(
        &mut self,
        path: PathBuf,
        root: Option<PathBuf>,
        deferred_main: Option<PathBuf>,
    ) -> bool {
        let Some(snapshot) = load_snapshot(&path) else {
            return false;
        };
        let candidates = qualifying_candidates(&snapshot, root.as_deref());
        if candidates.is_empty() {
            if let Err(err) = crate::platform::fs_ops::remove_file(&path) {
                log::warn!(
                    "cannot remove stale recovery snapshot {}: {err}",
                    path.display()
                );
            }
            return false;
        }
        self.snapshot_path = Some(path.clone());
        self.prompt = Some(RecoveryPrompt {
            snapshot_path: path,
            saved_at_ms: snapshot.saved_at_ms,
            candidates,
            deferred_main,
        });
        true
    }

    /// Applies the user's choice to the pending restore prompt and returns the
    /// app-only effects (a restore plan, a deferred main file).
    pub(in crate::app) fn resolve_choice(&mut self, choice: RecoveryChoice) -> RecoveryOutcome {
        let mut out = RecoveryOutcome::default();
        let Some(prompt) = self.prompt.take() else {
            return out;
        };
        match choice {
            RecoveryChoice::Restore => self.plan_restore(&prompt, &mut out),
            RecoveryChoice::Later => {
                self.unresolved_existing = true;
                out.open_main = prompt.deferred_main.filter(|path| path.is_file());
            }
            RecoveryChoice::Discard => {
                if let Err(err) = crate::platform::fs_ops::remove_file(&prompt.snapshot_path) {
                    log::warn!(
                        "cannot remove recovery snapshot {}: {err}",
                        prompt.snapshot_path.display()
                    );
                }
                self.snapshot_path = None;
                out.open_main = prompt.deferred_main.filter(|path| path.is_file());
            }
        }
        self.prompt = None;
        out
    }

    /// Builds the restore plan from the prompt's candidates and stamps the
    /// debounce so the restored tabs are snapshotted again.
    fn plan_restore(&mut self, prompt: &RecoveryPrompt, out: &mut RecoveryOutcome) {
        out.restore = prompt
            .candidates
            .iter()
            .map(|candidate| match &candidate.resolved_path {
                Some(path) => RecoveryRestoreStep::File {
                    path: path.clone(),
                    content: candidate.content.clone(),
                    active: candidate.active,
                },
                None => RecoveryRestoreStep::Untitled {
                    content: candidate.content.clone(),
                    active: candidate.active,
                },
            })
            .collect();
        out.open_main = prompt.deferred_main.clone().filter(|path| path.is_file());
        self.note_change();
        self.last_snapshot = None;
    }

    /// Creates (or reuses the pristine scratch slot for) a dirty recovery tab
    /// and returns its index. The caller runs `on_tab_switched` after.
    pub(in crate::app) fn new_recovery_tab(
        &mut self,
        content: String,
        session: &mut SessionState,
    ) -> usize {
        let index = if session.tabs.len() == 1
            && session.tabs[0].editor.path.is_none()
            && session.tabs[0].editor.is_empty()
            && !session.tabs[0].dirty
        {
            session.active_tab
        } else {
            let id = session.alloc_tab_id();
            session.tabs.push(EditorTab::new(id));
            session.tabs.len() - 1
        };
        session.tabs[index].editor.set_text(content);
        session.tabs[index].dirty = true;
        session.set_active(index);
        index
    }

    /// Consumes the session's snapshot path (graceful-exit cleanup).
    pub(in crate::app) fn take_snapshot_path(&mut self) -> Option<PathBuf> {
        self.snapshot_path.take()
    }

    /// Whether the user chose Later: the writer stays off for the session.
    pub(in crate::app) fn unresolved(&self) -> bool {
        self.unresolved_existing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tab(stored: Option<&str>, content: &str, active: bool) -> RecoveryTab {
        RecoveryTab {
            path: stored.map(str::to_owned),
            content: content.to_owned(),
            active,
        }
    }

    fn snapshot(version: u32, saved_at_ms: u64, tabs: Vec<RecoveryTab>) -> RecoverySnapshot {
        RecoverySnapshot {
            version,
            saved_at_ms,
            tabs,
        }
    }

    #[test]
    fn snapshot_serde_round_trips() {
        let original = snapshot(
            1,
            1_700_000_000_000,
            vec![
                tab(Some("chapter.typ"), "= Hi", true),
                tab(None, "scratch", false),
            ],
        );
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed = parse_snapshot(&json).expect("parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn corrupt_json_loads_as_absent() {
        assert_eq!(parse_snapshot("{ not json"), None);
    }

    #[test]
    fn unknown_version_loads_as_absent() {
        let json = serde_json::to_string(&snapshot(2, 0, vec![])).expect("serialize");
        assert_eq!(parse_snapshot(&json), None);
    }

    #[test]
    fn bom_is_tolerated() {
        let json = serde_json::to_string(&snapshot(1, 5, vec![])).expect("serialize");
        let parsed = parse_snapshot(&format!("\u{feff}{json}")).expect("parse with BOM");
        assert_eq!(parsed.saved_at_ms, 5);
    }

    #[test]
    fn encode_decodes_relative_paths_under_the_root() {
        let root = PathBuf::from("proj");
        let path = root.join("src").join("chapter.typ");
        let encoded = encode_path(&path, Some(&root));
        assert_eq!(encoded, "src/chapter.typ");
        assert_eq!(decode_path(&encoded, Some(&root)), path);
    }

    #[test]
    fn encode_keeps_paths_outside_the_root_absolute() {
        let root = std::env::temp_dir().join("typ-edit-recovery-root");
        let outside = std::env::temp_dir()
            .join("typ-edit-recovery-elsewhere")
            .join("a.typ");
        let encoded = encode_path(&outside, Some(&root));
        assert!(Path::new(&encoded).is_absolute(), "encoded: {encoded}");
        assert_eq!(decode_path(&encoded, Some(&root)), outside);
    }

    #[test]
    fn entry_qualification_covers_each_reason() {
        // Untitled always qualifies.
        assert!(entry_qualifies(None, 10, Some(100)));
        // Missing file qualifies.
        assert!(entry_qualifies(Some("a.typ"), 10, None));
        // Newer snapshot than disk qualifies.
        assert!(entry_qualifies(Some("a.typ"), 100, Some(50)));
        // Same or older snapshot does not.
        assert!(!entry_qualifies(Some("a.typ"), 50, Some(50)));
        assert!(!entry_qualifies(Some("a.typ"), 50, Some(100)));
    }

    #[test]
    fn build_snapshot_keeps_only_dirty_tabs_and_reports_oversized() {
        let big = "x".repeat(MAX_SNAPSHOT_CONTENT + 1);
        let root = PathBuf::from("proj");
        let path_a = root.join("a.typ");
        let path_big = root.join("big.typ");
        let tabs = [
            SnapshotTab {
                path: Some(&path_a),
                content: "= A",
                dirty: true,
                active: true,
            },
            SnapshotTab {
                path: Some(&path_a),
                content: "= clean",
                dirty: false,
                active: false,
            },
            SnapshotTab {
                path: None,
                content: "scratch",
                dirty: true,
                active: false,
            },
            SnapshotTab {
                path: Some(&path_big),
                content: &big,
                dirty: true,
                active: false,
            },
        ];
        let build = build_snapshot(&tabs, Some(&root), 1234);
        assert_eq!(build.snapshot.saved_at_ms, 1234);
        assert_eq!(build.snapshot.tabs.len(), 2);
        assert_eq!(build.snapshot.tabs[0].path.as_deref(), Some("a.typ"));
        assert!(build.snapshot.tabs[0].active);
        assert_eq!(build.snapshot.tabs[1].path, None);
        assert_eq!(build.skipped, vec!["big.typ".to_owned()]);
    }

    #[test]
    fn format_utc_ms_matches_known_timestamps() {
        assert_eq!(format_utc_ms(0), "1970-01-01 00:00");
        assert_eq!(format_utc_ms(1_700_000_000_000), "2023-11-14 22:13");
    }

    #[test]
    fn snapshot_worker_writes_a_materialized_job() {
        let dir =
            std::env::temp_dir().join(format!("typ-edit-recovery-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("recovery.json");
        let root = PathBuf::from("proj");
        let job = SnapshotJob {
            root: Some(root.clone()),
            target: target.clone(),
            saved_at_ms: 99,
            saved_at: Instant::now(),
            tabs: vec![SnapshotTabJob {
                path: Some(root.join("chapter.typ")),
                content: TextSnapshot::from_owned("= Chapter", 7),
                active: true,
            }],
        };
        let writer = SnapshotWriter::spawn();
        writer.tx.send(job).expect("queue snapshot");
        let result = writer
            .rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker result");
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(!result.empty);
        assert!(result.skipped.is_empty());
        let loaded = load_snapshot(&target).expect("load");
        assert_eq!(loaded.saved_at_ms, 99);
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].path.as_deref(), Some("chapter.typ"));
        assert_eq!(loaded.tabs[0].content, "= Chapter");
        assert!(loaded.tabs[0].active);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drain_applies_a_completed_snapshot() {
        let (job_tx, _job_rx) = bounded(1);
        let (result_tx, result_rx) = bounded(1);
        let mut recovery = Recovery {
            writer: Some(SnapshotWriter {
                tx: job_tx,
                rx: result_rx,
                join: None,
            }),
            writer_busy: true,
            write_failed: true,
            ..Recovery::default()
        };
        let target = PathBuf::from("recovery.json");
        let saved_at = Instant::now();
        result_tx
            .send(SnapshotResult {
                target: target.clone(),
                saved_at,
                skipped: Vec::new(),
                empty: false,
                error: None,
            })
            .expect("result");
        let mut toasts = Vec::new();
        recovery.drain_snapshot_results(&mut toasts);
        assert_eq!(recovery.snapshot_path, Some(target));
        assert_eq!(recovery.last_snapshot, Some(saved_at));
        assert!(!recovery.writer_busy);
        assert!(!recovery.write_failed);
        assert!(toasts.is_empty());
    }

    #[test]
    fn shutdown_joins_and_applies_a_pending_write() {
        let dir =
            std::env::temp_dir().join(format!("typ-edit-recovery-shutdown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("recovery.json");
        let writer = SnapshotWriter::spawn();
        writer
            .tx
            .send(SnapshotJob {
                root: None,
                target: target.clone(),
                saved_at_ms: 7,
                saved_at: Instant::now(),
                tabs: vec![SnapshotTabJob {
                    path: None,
                    content: TextSnapshot::from_owned("scratch", 1),
                    active: true,
                }],
            })
            .expect("queue snapshot");
        let mut recovery = Recovery {
            writer: Some(writer),
            writer_busy: true,
            ..Recovery::default()
        };
        recovery.shutdown_writer();
        assert!(!recovery.writer_busy);
        assert_eq!(recovery.snapshot_path, Some(target.clone()));
        assert!(target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_snapshot_file_writes_atomically_and_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("typ-edit-recovery-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("recovery.json");
        let original = snapshot(1, 42, vec![tab(None, "hello", true)]);
        write_snapshot_file(&path, &original).expect("write");
        assert!(!path.with_extension("json.tmp").exists(), "tmp cleaned up");
        assert_eq!(load_snapshot(&path).expect("load"), original);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
