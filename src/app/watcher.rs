//! Recursive project-file watcher and the passive-refresh policy.
//!
//! `notify` supplies the platform backend: Windows uses
//! `ReadDirectoryChangesW` with `bWatchSubtree = true`, macOS uses FSEvents,
//! and the other targets use their native inotify/kqueue backend. The app only
//! consumes paths; debouncing, tree invalidation and tab-level conflict
//! detection stay here so the UI layer sees one coalesced batch.

use super::TypEditApp;
use super::state::ExternalChange;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// Coalesce bursts (an editor often writes/renames in several steps).
const WATCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// The app's one recursive watcher. `watch` is idempotent for the same root.
pub(super) struct ProjectWatcher {
    root: Option<PathBuf>,
    watcher: Option<RecommendedWatcher>,
    tx: Sender<notify::Result<Event>>,
    rx: Receiver<notify::Result<Event>>,
    ctx: egui::Context,
}

impl ProjectWatcher {
    pub(super) fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            root: None,
            watcher: None,
            tx,
            rx,
            ctx,
        }
    }

    /// Starts watching `root` recursively, replacing any previous root.
    pub(super) fn watch(&mut self, root: PathBuf) -> notify::Result<()> {
        if self.root.as_deref() == Some(root.as_path()) && self.watcher.is_some() {
            return Ok(());
        }
        // Dropping the old handle unregisters its watch before the new one is
        // installed, so a workspace switch cannot leave both roots feeding us.
        self.watcher = None;
        self.root = None;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                if tx.send(event).is_ok() {
                    ctx.request_repaint();
                }
            },
            Config::default(),
        )?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        self.watcher = Some(watcher);
        self.root = Some(root);
        Ok(())
    }

    /// Drains the callback channel and returns the relevant paths of this
    /// frame. `.typ-edit` is app metadata (lock, recovery snapshots) and is
    /// deliberately invisible to the project tree and compile pipeline.
    pub(super) fn poll(&mut self) -> BTreeSet<PathBuf> {
        let Some(root) = self.root.clone() else {
            return BTreeSet::new();
        };
        let mut paths = BTreeSet::new();
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Ok(event) => {
                    for path in event.paths {
                        if path.starts_with(&root) && is_relevant_path(&path) {
                            paths.insert(path);
                        }
                    }
                }
                Err(error) => log::warn!("project watcher: {error}"),
            }
        }
        paths
    }
}

/// Whether a changed path is project content rather than `.typ-edit` bookkeeping.
fn is_relevant_path(path: &Path) -> bool {
    path.file_name() != Some(std::ffi::OsStr::new(crate::workspace::MAIN_MARKER))
        && !path
            .components()
            .any(|component| component.as_os_str() == crate::workspace::SNAPSHOT_DIR)
}

impl TypEditApp {
    /// Polls the watcher and applies one debounced batch. Called once per frame
    /// from `pump`, before the UI renders.
    pub(super) fn poll_project_watcher(&mut self, ctx: &egui::Context) {
        let paths = self.watcher.poll();
        if !paths.is_empty() {
            self.watch_pending.extend(paths);
            self.watch_due = Some(Instant::now() + WATCH_DEBOUNCE);
        }
        let Some(due) = self.watch_due else {
            return;
        };
        let now = Instant::now();
        if now < due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
            return;
        }
        self.watch_due = None;
        let paths = std::mem::take(&mut self.watch_pending);
        if !paths.is_empty() {
            self.apply_watch_paths(paths);
        }
    }

    /// Applies one coalesced watcher batch.
    ///
    /// Tree entries are invalidated first so the next draw rereads them. The
    /// active buffer is never replaced here: the conflict modal owns that
    /// decision. Compiling before the user answers therefore sees the current
    /// in-memory buffer, exactly as the requirement specifies.
    pub(super) fn apply_watch_paths(&mut self, paths: BTreeSet<PathBuf>) {
        if self.tree.root.is_none() || paths.is_empty() {
            return;
        }
        for path in &paths {
            self.tree.invalidate(path);
        }
        self.detect_external_changes(&paths);
        self.trigger_compile();
    }

    /// Compares every changed path against the open tab that owns it.
    ///
    /// Equal bytes mean the change was this editor's own save; the associated
    /// conflict (if any) is cleared silently. Different bytes create or refresh
    /// a tab-keyed conflict, which the overlay renders only for the active tab.
    fn detect_external_changes(&mut self, paths: &BTreeSet<PathBuf>) {
        for path in paths {
            let tabs: Vec<(usize, u64, String)> = self
                .session
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| tab.editor.path.as_deref() == Some(path.as_path()))
                .map(|(index, tab)| (index, tab.id, tab.editor.shared_text().to_string()))
                .collect();
            if tabs.is_empty() {
                continue;
            }
            let Ok(disk) = crate::platform::fs_ops::read_to_string(path) else {
                // A deletion is not actionable through the save/keep dialog:
                // there is no disk copy to adopt. Tree refresh already ran.
                continue;
            };
            for (index, tab_id, memory) in tabs {
                if disk == memory {
                    self.external_changes.remove(&tab_id);
                } else {
                    self.external_changes.insert(
                        tab_id,
                        ExternalChange {
                            tab_id,
                            path: self.session.tabs[index]
                                .editor
                                .path
                                .clone()
                                .unwrap_or_else(|| path.clone()),
                            disk_text: disk.clone(),
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_metadata_paths_are_not_watched_content() {
        let project = Path::new("project");
        assert!(!is_relevant_path(
            &project
                .join(crate::workspace::SNAPSHOT_DIR)
                .join("local/lock")
        ));
        assert!(!is_relevant_path(
            &project
                .join(crate::workspace::SNAPSHOT_DIR)
                .join("snapshot.json")
        ));
        assert!(!is_relevant_path(
            &project.join(crate::workspace::MAIN_MARKER)
        ));
        assert!(is_relevant_path(&project.join("chapters/one.typ")));
        assert!(is_relevant_path(&project.join("assets/figure.png")));
    }

    #[test]
    fn poll_keeps_only_relevant_paths_inside_the_root() {
        use notify::event::{CreateKind, EventKind};

        let root = PathBuf::from("project");
        let nested = root.join("chapters").join("one.typ");
        let metadata = root.join(crate::workspace::SNAPSHOT_DIR).join("local/lock");
        let marker = root.join(crate::workspace::MAIN_MARKER);
        let outside = PathBuf::from("elsewhere.typ");
        let (tx, rx) = mpsc::channel();
        let mut watcher = ProjectWatcher {
            root: Some(root),
            watcher: None,
            tx: tx.clone(),
            rx,
            ctx: egui::Context::default(),
        };
        for path in [nested.clone(), metadata, marker, outside] {
            tx.send(Ok(
                Event::new(EventKind::Create(CreateKind::File)).add_path(path)
            ))
            .expect("send synthetic event");
        }

        assert_eq!(watcher.poll(), BTreeSet::from([nested]));
    }
}
