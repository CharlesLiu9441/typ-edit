//! Git integration: one worker thread owns every `git2` call, and the UI reads a
//! cheap `Arc<GitSnapshot>`.
//!
//! The worker (not the GUI thread) performs repository discovery, so a slow or
//! broken repository never blocks a frame. The state is lazily created only while
//! `Settings::git_enabled` is set and a project folder is open.

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use typ_edit_engine::Waker;
use typ_edit_git::{BlameLine, CommitInfo, FileStatus, GitRepo, StatusKind};

/// A unit of work for the git worker thread.
pub(super) enum GitRequest {
    /// Re-read HEAD, branch and working-tree status.
    RefreshStatus,
    /// List commits, optionally restricted to `path`.
    Log { path: Option<PathBuf>, limit: usize },
    /// Unified patch of `oid`, optionally restricted to `path`.
    Diff { oid: String, path: Option<PathBuf> },
    /// Line blame for `path`; `line_count` is the unsaved buffer's line count
    /// used to truncate committed-file attribution positionally.
    Blame {
        path: PathBuf,
        line_count: Option<usize>,
    },
}

/// Result of a status refresh.
pub(super) struct GitStatusResult {
    pub(super) repo_root: Option<PathBuf>,
    pub(super) branch: String,
    pub(super) head_short: String,
    pub(super) statuses: Vec<FileStatus>,
    /// First failure of this refresh, if any: the UI surfaces it so an unreadable
    /// or locked repository cannot be mistaken for a clean tree. `None` on success
    /// and when the folder is simply not a repository.
    pub(super) error: Option<String>,
}

/// A worker response, one per request.
pub(super) enum GitResponse {
    Status(GitStatusResult),
    Log {
        path: Option<PathBuf>,
        items: Vec<CommitInfo>,
    },
    Diff {
        oid: String,
        text: String,
    },
    Blame {
        path: PathBuf,
        lines: Vec<BlameLine>,
    },
}

/// Owns the git worker thread. Dropping it drops the request sender, which ends
/// the thread after its current request.
pub(super) struct GitWorker {
    requests: Sender<GitRequest>,
    responses: Receiver<GitResponse>,
}

impl GitWorker {
    /// Discovers the repository on the worker thread and serves requests until
    /// the handle is dropped.
    pub(super) fn spawn(project_root: PathBuf, waker: Waker) -> Self {
        // Request queue: bounded so a slow worker can never grow it without limit.
        // The UI enqueues with `try_send` (see [`Self::request`]), so a full queue
        // drops the request with a warning rather than blocking a frame. Requests
        // are few and the refresh is coalesced, so 16 is far above normal use.
        let (request_tx, request_rx) = bounded(16);
        // Response queue: bounded, so the worker's blocking `send` applies
        // backpressure. The UI drains every response each frame, so it never fills.
        let (response_tx, response_rx) = bounded(16);
        std::thread::Builder::new()
            .name("typ-edit-git".to_owned())
            .spawn(move || {
                let repo = GitRepo::discover(&project_root);
                while let Ok(request) = request_rx.recv() {
                    if response_tx.send(respond(repo.as_ref(), request)).is_err() {
                        break;
                    }
                    waker.wake();
                }
            })
            .expect("spawn git worker");
        Self {
            requests: request_tx,
            responses: response_rx,
        }
    }

    /// Enqueues a request without ever blocking the UI thread. Returns `false`
    /// when the request was dropped — the queue is full or the worker is gone —
    /// so a caller that set a pending marker can clear it instead of stranding
    /// the UI on an answer that will never arrive.
    pub(super) fn request(&self, request: GitRequest) -> bool {
        match self.requests.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                log::warn!("git request queue full; dropping request");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub(super) fn drain(&self) -> Vec<GitResponse> {
        let mut out = Vec::new();
        while let Ok(response) = self.responses.try_recv() {
            out.push(response);
        }
        out
    }
}

/// Unwraps a git query result, logging the failure and falling back to the empty
/// value. A broken repository, a locked index or a permission error must not be
/// indistinguishable from a genuinely clean tree, so the reason is recorded (and
/// optionally handed to the UI through `error`).
fn logged<T: Default, E: std::fmt::Display>(
    result: Result<T, E>,
    what: &str,
    error: Option<&mut Option<String>>,
) -> T {
    result.unwrap_or_else(|failure| {
        log::warn!("git {what} failed: {failure}");
        if let Some(slot) = error
            && slot.is_none()
        {
            *slot = Some(failure.to_string());
        }
        T::default()
    })
}

/// Runs one request. Failures degrade to empty results, so a missing repository
/// or an unborn HEAD never panics the worker; each failure is logged by
/// [`logged`].
fn respond(repo: Option<&GitRepo>, request: GitRequest) -> GitResponse {
    match request {
        GitRequest::RefreshStatus => {
            let Some(repo) = repo else {
                return GitResponse::Status(GitStatusResult {
                    repo_root: None,
                    branch: String::new(),
                    head_short: String::new(),
                    statuses: Vec::new(),
                    error: None,
                });
            };
            let mut error = None;
            let head = logged(repo.head(), "head", Some(&mut error));
            let statuses = logged(repo.status(), "status", Some(&mut error));
            GitResponse::Status(GitStatusResult {
                repo_root: repo.workdir(),
                branch: head.branch,
                head_short: head.short,
                statuses,
                error,
            })
        }
        GitRequest::Log { path, limit } => {
            let items = match repo {
                Some(repo) => logged(repo.log(path.as_deref(), limit), "log", None),
                None => Vec::new(),
            };
            GitResponse::Log { path, items }
        }
        GitRequest::Diff { oid, path } => {
            let text = match repo {
                Some(repo) => logged(repo.diff_commit(&oid, path.as_deref()), "diff", None),
                None => String::new(),
            };
            GitResponse::Diff { oid, text }
        }
        GitRequest::Blame { path, line_count } => {
            let mut lines = match repo {
                Some(repo) => logged(repo.blame(&path, None), "blame", None),
                None => Vec::new(),
            };
            if let Some(count) = line_count {
                lines.truncate(count);
            }
            GitResponse::Blame { path, lines }
        }
    }
}

/// UI-facing git state for the open repository.
///
/// `revision` changes on every applied refresh, so the UI can detect a new
/// snapshot without comparing the whole map.
#[derive(Clone, Debug, Default)]
pub(super) struct GitSnapshot {
    pub(super) branch: String,
    pub(super) head_short: String,
    pub(super) statuses: BTreeMap<PathBuf, StatusKind>,
    /// First failure of the last refresh, for the status bar; `None` while the
    /// repository is readable (or absent).
    pub(super) error: Option<String>,
    pub(super) revision: u64,
}

/// Everything the app keeps about git: the lazy worker, the UI snapshot, and the
/// newest log/blame/diff answers.
pub(super) struct GitState {
    waker: Waker,
    worker: Option<GitWorker>,
    /// Root the current worker was spawned for, to detect a folder change.
    worker_root: Option<PathBuf>,
    /// A status refresh is queued or in flight; further triggers are coalesced.
    refresh_pending: bool,
    /// Repository root of the last refresh; `None` when the folder is not a repo.
    pub(super) repo_root: Option<PathBuf>,
    /// Latest status snapshot; the UI reads this `Arc` directly.
    pub(super) snapshot: Arc<GitSnapshot>,
    revision: u64,
    /// Latest `Log` answer and the path it belongs to.
    pub(super) log: Vec<CommitInfo>,
    pub(super) log_path: Option<PathBuf>,
    /// File a `Log` request is in flight for; `None` when none is pending.
    pub(super) log_pending: Option<PathBuf>,
    /// Latest `Blame` answer and the path it belongs to.
    pub(super) blame: Vec<BlameLine>,
    pub(super) blame_path: Option<PathBuf>,
    /// Latest `Diff` answer: the commit oid and its patch text.
    pub(super) diff: Option<(String, String)>,
}

impl GitState {
    pub(super) fn new(waker: Waker) -> Self {
        Self {
            waker,
            worker: None,
            worker_root: None,
            refresh_pending: false,
            repo_root: None,
            snapshot: Arc::new(GitSnapshot::default()),
            revision: 0,
            log: Vec::new(),
            log_path: None,
            log_pending: None,
            blame: Vec::new(),
            blame_path: None,
            diff: None,
        }
    }

    fn drain(&self) -> Vec<GitResponse> {
        self.worker
            .as_ref()
            .map(GitWorker::drain)
            .unwrap_or_default()
    }

    /// Drops the worker and clears every cached answer; cheap while disabled.
    pub(super) fn shutdown(&mut self) {
        if self.worker.take().is_none() && self.repo_root.is_none() {
            return;
        }
        self.worker_root = None;
        self.refresh_pending = false;
        self.repo_root = None;
        self.snapshot = Arc::new(GitSnapshot::default());
        self.log.clear();
        self.log_path = None;
        self.log_pending = None;
        self.blame.clear();
        self.blame_path = None;
        self.diff = None;
    }

    pub(super) fn apply(&mut self, response: GitResponse) {
        match response {
            GitResponse::Status(status) => {
                self.refresh_pending = false;
                self.repo_root = status.repo_root;
                self.revision += 1;
                self.snapshot = Arc::new(GitSnapshot {
                    branch: status.branch,
                    head_short: status.head_short,
                    statuses: status
                        .statuses
                        .into_iter()
                        .map(|file| (file.path, file.kind))
                        .collect(),
                    error: status.error,
                    revision: self.revision,
                });
            }
            GitResponse::Log { path, items } => {
                if self.log_pending == path {
                    self.log_pending = None;
                }
                self.log_path = path;
                self.log = items;
            }
            GitResponse::Diff { oid, text } => self.diff = Some((oid, text)),
            GitResponse::Blame { path, lines } => {
                self.blame_path = Some(path);
                self.blame = lines;
            }
        }
    }
}

impl GitState {
    /// Ensures a worker exists for `root` and requests a status refresh.
    ///
    /// Triggers: project open, successful save, and P4-3b's manual-refresh command.
    /// While disabled it shuts the worker down instead, so the feature costs
    /// nothing; a runtime enable takes effect on the next trigger.
    pub(crate) fn refresh(&mut self, enabled: bool, root: Option<PathBuf>) {
        if !enabled {
            self.shutdown();
            return;
        }
        let Some(root) = root else {
            self.shutdown();
            return;
        };
        if self.worker.is_none() || self.worker_root.as_deref() != Some(root.as_path()) {
            self.worker = Some(GitWorker::spawn(root.clone(), self.waker.clone()));
            self.worker_root = Some(root);
            self.refresh_pending = false;
        }
        if self.refresh_pending {
            return;
        }
        if let Some(worker) = &self.worker {
            // A dropped request (full queue) keeps `refresh_pending` clear, so the
            // next trigger retries instead of the UI never refreshing again.
            self.refresh_pending = worker.request(GitRequest::RefreshStatus);
        }
    }

    /// Requests the history of `path`, recording it as the pending log target.
    /// A no-op while no worker exists (git disabled or not a repository).
    pub(super) fn git_request_log(&mut self, path: Option<PathBuf>) {
        if self.worker.is_none() {
            return;
        }
        self.log_pending = path.clone();
        if let Some(worker) = &self.worker {
            // Clear the pending marker if the request was dropped, so a later
            // answer for a different path is not mistaken for this one.
            if !worker.request(GitRequest::Log { path, limit: 100 }) {
                self.log_pending = None;
            }
        }
    }

    /// Requests the unified patch of `oid` (optionally limited to `path`).
    pub(super) fn git_request_diff(&mut self, oid: String, path: Option<PathBuf>) {
        if let Some(worker) = &self.worker {
            let _ = worker.request(GitRequest::Diff { oid, path });
        }
    }

    /// Requests line blame for `path` with the unsaved buffer's line count.
    pub(super) fn git_request_blame(&mut self, path: PathBuf, line_count: Option<usize>) {
        if let Some(worker) = &self.worker {
            let _ = worker.request(GitRequest::Blame { path, line_count });
        }
    }

    /// Drains and applies worker responses; called once per frame from `pump`.
    pub(super) fn drain_and_apply(&mut self) {
        let responses = self.drain();
        for response in responses {
            self.apply(response);
        }
    }
}
