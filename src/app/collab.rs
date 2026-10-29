//! LAN collaboration controller: session lifecycle, mDNS discovery, the join
//! mirror, latency and status.
//!
//! Phase 3 wires the *lifecycle* only. Opening or joining a session, mirroring a
//! project to a local cache, tracking users/latency and tearing everything down
//! works here; applying remote edits, cursors, images and saves is Phase 4 and is
//! deliberately stubbed with `// Phase 4:` markers.
//!
//! All network handles live behind one enum ([`Session`]) so the app holds a
//! single [`CollabController`] field. `pump` drains every channel non-blockingly
//! (mirroring the git worker's drain) and returns a [`CollabOutcome`] the frame
//! applies, keeping the controller free of `TypEditApp` borrows.

use super::state::ToastMsg;
use crate::settings::config_dir;
use crate::workspace;
use crossbeam_channel::{Receiver, TryRecvError, bounded};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use typ_edit_collab::{
    Advertiser, Browser, ChangeOp, ChangeSet, ClientError, ClientEvent, CollabClient, CollabServer,
    DiscoveredService, DiscoveryEvent, HistoryOp, RejectKind, SERVER_SID, ServerConfig,
    ServerEvent, Sid, apply as apply_ops,
};
use typ_edit_editor::{CapturedEdit, RemoteCaret};
use typ_edit_i18n::{t, t_with};
use typ_edit_model::TextSnapshot;

/// How often a live client pings the server for a latency sample.
pub(super) const PING_INTERVAL: Duration = Duration::from_secs(2);

/// A latency above this is shown as a timeout in the status bar (`§5.11`).
pub(super) const RTT_TIMEOUT_MS: u64 = 1000;

/// How long an unanswered ping may stay outstanding before the client drops the
/// session (`§5.11`: 5 s).
const PING_DISCONNECT: Duration = Duration::from_secs(5);

/// Safety window for the server's `Busy` notification. The server sends
/// `busy: false` when the snapshot/resync finishes; if that frame is lost (for
/// example the server dies mid-snapshot), the client resumes heartbeats after
/// this window instead of pausing forever.
const SERVER_BUSY_WINDOW: Duration = Duration::from_secs(300);

/// Minimum interval between throttled cursor sends (`§5.2`: ~100ms).
const CURSOR_INTERVAL: Duration = Duration::from_millis(100);

/// Coalesce remote-edit mirror writes for this long. Shorter than the compile
/// debounce, so a compile forced by a remote edit sees the flushed mirror.
const MIRROR_WRITE_DEBOUNCE: Duration = Duration::from_millis(100);

/// The latency tick decision for a live client (`§5.11`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PingTick {
    /// No ping is outstanding and the interval elapsed: send a fresh one.
    Send,
    /// Do nothing yet (an outstanding ping is young, or the interval has not
    /// elapsed).
    Wait,
    /// The outstanding ping exceeded the disconnect window.
    Timeout,
}

/// Decides the latency tick. `outstanding` is how long the in-flight ping has
/// been unanswered (`None` when none is in flight); `since_last` is the time
/// since the last `Ping`.
///
/// A fresh `Ping` is sent only when no ping is outstanding, so `ping_sent_at`
/// keeps ageing towards [`PING_DISCONNECT`] instead of being reset every
/// [`PING_INTERVAL`] (the bug this pins).
pub(super) fn ping_tick(outstanding: Option<Duration>, since_last: Duration) -> PingTick {
    match outstanding {
        Some(age) if age >= PING_DISCONNECT => PingTick::Timeout,
        Some(_) => PingTick::Wait,
        None if since_last >= PING_INTERVAL => PingTick::Send,
        None => PingTick::Wait,
    }
}

/// The heartbeat decision for a client session.
///
/// During the join snapshot the server core is streaming files and cannot answer
/// ping frames; a normal 5 s timeout would disconnect a client that is simply
/// still syncing. The same applies to a live client while the server has
/// broadcast `Busy` for another peer's snapshot or for its own resync. `None`
/// means "do nothing".
pub(super) fn heartbeat_tick(
    live: bool,
    busy: bool,
    outstanding: Option<Duration>,
    since_last: Duration,
) -> Option<PingTick> {
    if !live || busy {
        return None;
    }
    Some(ping_tick(outstanding, since_last))
}

/// Which collaboration role is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(in crate::app) enum SessionKind {
    #[default]
    Off,
    Server,
    Client,
}

/// Progress of the join handshake and snapshot download.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum JoinFlow {
    /// No join in progress.
    #[default]
    Idle,
    /// A background connect/handshake thread is running.
    Connecting,
    /// The snapshot is downloading; `done` of `total` text files mirrored.
    Syncing { done: usize, total: usize },
    /// The snapshot landed and the mirror was opened.
    Live,
    /// The last join attempt failed.
    Failed,
}

impl JoinFlow {
    /// Records the tree's text-file count, starting or updating the sync state.
    pub(super) fn on_files(&mut self, total: usize) {
        match self {
            Self::Syncing {
                done,
                total: current,
            } => {
                *current = total;
                *done = (*done).min(total);
            }
            _ => *self = Self::Syncing { done: 0, total },
        }
    }

    /// Counts one mirrored file.
    pub(super) fn on_file(&mut self) {
        if let Self::Syncing { done, total } = self {
            *done = (*done + 1).min(*total);
        }
    }
}

/// One connected server-side user.
#[derive(Clone, Debug)]
pub(super) struct ClientUser {
    pub(super) sid: Sid,
    pub(super) user: String,
}

/// One remote participant's cursor, in byte offsets, keyed by session id.
#[derive(Clone, Debug)]
pub(super) struct RemoteCursor {
    pub(super) user: String,
    /// Root-relative, forward-slash file path.
    pub(super) file: String,
    pub(super) carets: Vec<u64>,
    pub(super) selections: Vec<(u64, u64)>,
    pub(super) color_index: u8,
}

/// A stable palette index for a user name (0..8).
pub(super) fn color_index_for(user: &str) -> u8 {
    let mut hash: u32 = 2166136261;
    for byte in user.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16777619);
    }
    (hash % 8) as u8
}

/// A live server session.
pub(super) struct ServerSession {
    server: CollabServer,
    advertiser: Option<Advertiser>,
    users: Vec<ClientUser>,
    /// The image directory as a root-relative, forward-slash path.
    image_dir_rel: String,
}

/// Everything the app needs to start a server in one value.
pub(super) struct ServerStart {
    pub(super) root: PathBuf,
    pub(super) main_file: Option<PathBuf>,
    pub(super) username: String,
    pub(super) password: Option<String>,
    pub(super) port: u16,
    /// Dirty open buffers, already root-relative, seeded into the DocStore.
    pub(super) initial_docs: Vec<(String, TextSnapshot)>,
}

/// A live client session.
pub(super) struct ClientSession {
    client: CollabClient,
    user: String,
    project: String,
    server_user: String,
    mirror: PathBuf,
    main_file: Option<String>,
    /// Root-relative image directory sent by the server in `Config`.
    image_dir: Option<String>,
    mirrored: usize,
    total: usize,
    rtt_ms: Option<u64>,
    last_ping: Instant,
    /// When the outstanding ping was sent (`None` when none is in flight).
    ping_sent_at: Option<Instant>,
    /// While the server is streaming a join snapshot, heartbeats are paused.
    /// The deadline bounds a lost `busy: false` frame.
    busy_until: Option<Instant>,
    ready_sent: bool,
}

/// The active collaboration session.
#[derive(Default)]
pub(super) enum Session {
    #[default]
    Off,
    Server(ServerSession),
    Client(Box<ClientSession>),
}

impl Session {
    pub(super) fn kind(&self) -> SessionKind {
        match self {
            Self::Off => SessionKind::Off,
            Self::Server(_) => SessionKind::Server,
            Self::Client(_) => SessionKind::Client,
        }
    }
}

/// One host-side apply in the order the server core emitted it.
///
/// Remote changes and authoritative resync replies must not be reordered: an
/// older `ResyncFile` applied after a newer change would put the host back on
/// stale text.
pub(super) enum HostApply {
    Changes(ChangeSet),
    Resync { path: String, text: Option<String> },
}

/// App-only effects of a `pump`, applied by the frame in order.
#[derive(Default)]
pub(super) struct CollabOutcome {
    /// Toasts to raise, each carrying its semantic family.
    pub(super) toasts: Vec<ToastMsg>,
    /// A mirror directory to open as the workspace.
    pub(super) open_root: Option<PathBuf>,
    /// Whether the app should park on the landing page (session ended).
    pub(super) navigate_landing: bool,
    /// Accepted remote change sets to apply (open tabs or mirror files).
    pub(super) remote_changes: Vec<ChangeSet>,
    /// Host-side changes and resync replies, in server-event order.
    pub(super) host_applies: Vec<HostApply>,
    /// Image basenames whose `#image(...)` snippet should be inserted (our own
    /// uploads, matched by upload id).
    pub(super) insert_images: Vec<String>,
    /// Image basenames to fetch into the mirror (every `ImageAdded`).
    pub(super) fetch_images: Vec<String>,
    /// Fetched binary files (root-relative path, bytes) to write into the mirror.
    pub(super) remote_bins: Vec<(String, Vec<u8>)>,
    /// Chunks of an on-demand chunked download.
    pub(super) file_chunks: Vec<(String, u64, u64, Vec<u8>)>,
    /// Files the server marked too large for a normal download.
    pub(super) too_large: Vec<(String, u64)>,
    /// Files the server could not read.
    pub(super) unavailable: Vec<String>,
    /// Assets whose download just completed (root-relative path).
    pub(super) downloaded: Vec<String>,
    /// A resync snapshot: `(root-relative path, content)` to replace first.
    pub(super) resync_files: Vec<(String, String)>,
    /// Resync change sets to replay after the snapshot, in order.
    pub(super) resync_applied: Vec<ChangeSet>,
    /// A resync completed; the app surfaces its toast after applying it.
    pub(super) resynced: bool,
}

/// The collaboration controller.
pub(super) struct CollabController {
    session: Session,
    // Dialog visibility and fields, read/written by the dialog layer.
    pub(super) share_open: bool,
    pub(super) join_open: bool,
    pub(super) users_open: bool,
    pub(super) info_open: bool,
    pub(super) stop_confirm: bool,
    pub(super) disconnect_confirm: bool,
    pub(super) share_username: String,
    pub(super) share_password: String,
    pub(super) share_port: String,
    pub(super) join_username: String,
    pub(super) join_address: String,
    pub(super) join_password: String,
    pub(super) discovered: Vec<DiscoveredService>,
    pub(super) join: JoinFlow,
    password_prompt: Option<Vec<SocketAddr>>,
    // Discovery.
    browser: Option<Browser>,
    // Join transport.
    join_rx: Option<Receiver<Result<CollabClient, ClientError>>>,
    /// Remote cursors keyed by session id (both roles).
    pub(super) cursors: BTreeMap<Sid, RemoteCursor>,
    /// Remote-edit mirror writes coalesced by path until the debounce elapses.
    ///
    /// Values are shared text snapshots: an open-tab remote edit can queue the
    /// editor's `Arc<String>` without copying the whole buffer on every key.
    pending_mirror_writes: BTreeMap<PathBuf, Arc<String>>,
    /// Earliest time the queued mirror writes may be flushed.
    mirror_write_due: Option<Instant>,
    /// Server-global undo availability (both roles), for the Edit menu.
    pub(super) history_can_undo: bool,
    /// Server-global redo availability (both roles).
    pub(super) history_can_redo: bool,
    /// This participant's latest cursors, fed by the editor pane each frame.
    cursor_file: String,
    cursor_carets: Vec<u64>,
    cursor_selections: Vec<(u64, u64)>,
    cursor_sent_at: Instant,
    /// Image upload correlation ids awaiting their `ImageAdded` echo.
    pending_uploads: Vec<u64>,
    /// The participant list of a client session (from `Joined` / `Users`).
    client_users: Vec<ClientUser>,
    /// The open-tab set last sent to the client (avoids per-frame sends).
    last_open_files: Vec<String>,
}

impl Default for CollabController {
    fn default() -> Self {
        Self {
            session: Session::Off,
            share_open: false,
            join_open: false,
            users_open: false,
            info_open: false,
            stop_confirm: false,
            disconnect_confirm: false,
            share_username: String::new(),
            share_password: String::new(),
            share_port: String::new(),
            join_username: String::new(),
            join_address: String::new(),
            join_password: String::new(),
            password_prompt: None,
            browser: None,
            discovered: Vec::new(),
            join: JoinFlow::Idle,
            join_rx: None,
            cursors: BTreeMap::new(),
            pending_mirror_writes: BTreeMap::new(),
            mirror_write_due: None,
            history_can_undo: false,
            history_can_redo: false,
            cursor_file: String::new(),
            cursor_carets: Vec::new(),
            cursor_selections: Vec::new(),
            cursor_sent_at: Instant::now(),
            pending_uploads: Vec::new(),
            client_users: Vec::new(),
            last_open_files: Vec::new(),
        }
    }
}

impl CollabController {
    // --- accessors -----------------------------------------------------------

    pub(super) fn kind(&self) -> SessionKind {
        self.session.kind()
    }

    pub(super) fn is_active(&self) -> bool {
        !matches!(self.session, Session::Off)
    }

    pub(super) fn is_server(&self) -> bool {
        matches!(self.session, Session::Server(_))
    }

    /// Clears every controller field that must not leak into a new session.
    /// `last_open_files` is the dangerous one: `set_open_files` compares against
    /// it, so a stale value would suppress the first open-file snapshot of the
    /// next session and make a later resync omit the active files.
    fn reset_session_state(&mut self) {
        self.cursors.clear();
        self.client_users.clear();
        self.pending_uploads.clear();
        self.last_open_files.clear();
        self.cursor_file.clear();
        self.cursor_carets.clear();
        self.cursor_selections.clear();
        self.history_can_undo = false;
        self.history_can_redo = false;
    }

    /// The image directory the active session uses, as a root-relative path.
    /// A client trusts the server's `Config`; the fallback is only for an
    /// inactive controller or an old server that omitted the field.
    pub(super) fn image_dir_rel(&self, fallback: &str) -> String {
        match &self.session {
            Session::Server(server) => server.image_dir_rel.clone(),
            Session::Client(client) => client
                .image_dir
                .clone()
                .unwrap_or_else(|| fallback.trim().to_owned()),
            Session::Off => fallback.trim().to_owned(),
        }
    }

    /// Asks a server host's core for the authoritative text of `path`.
    pub(super) fn request_doc(&mut self, path: &str) {
        if let Session::Server(server) = &self.session {
            server.server.request_doc(path);
        }
    }

    /// The bound port of the active server (test seam).
    #[cfg(test)]
    pub(super) fn server_port(&self) -> Option<u16> {
        match &self.session {
            Session::Server(server) => Some(server.server.port()),
            _ => None,
        }
    }

    // --- coalesced mirror writes --------------------------------------------

    /// Queues a mirror write, coalescing per path. The first write of a burst
    /// starts the debounce, so a continuous remote stream cannot starve it.
    pub(super) fn queue_mirror_write(&mut self, path: PathBuf, text: String, now: Instant) {
        self.queue_mirror_write_shared(path, Arc::new(text), now);
    }

    /// Queues an already-shared text snapshot without copying it.
    pub(super) fn queue_mirror_write_shared(
        &mut self,
        path: PathBuf,
        text: Arc<String>,
        now: Instant,
    ) {
        self.pending_mirror_writes.insert(path, text);
        self.mirror_write_due
            .get_or_insert(now + MIRROR_WRITE_DEBOUNCE);
    }

    /// The not-yet-written content of `path`, used as the base for subsequent
    /// remote ops on a closed file.
    pub(super) fn pending_mirror_text(&self, path: &Path) -> Option<&str> {
        self.pending_mirror_writes
            .get(path)
            .map(|text| text.as_str())
    }

    /// Whether the debounce has elapsed for the queued mirror writes.
    pub(super) fn mirror_writes_due(&self, now: Instant) -> bool {
        self.mirror_write_due.is_some_and(|due| now >= due)
    }

    /// How long until the next mirror flush, for the frame's repaint timer.
    pub(super) fn mirror_write_repaint(&self, now: Instant) -> Option<Duration> {
        self.mirror_write_due
            .map(|due| due.saturating_duration_since(now))
    }

    /// Removes the queued writes when due (or immediately when `force`d).
    pub(super) fn take_mirror_writes(
        &mut self,
        now: Instant,
        force: bool,
    ) -> Vec<(PathBuf, Arc<String>)> {
        if !force && !self.mirror_writes_due(now) {
            return Vec::new();
        }
        self.mirror_write_due = None;
        std::mem::take(&mut self.pending_mirror_writes)
            .into_iter()
            .collect()
    }

    /// The connected users of a server session (empty otherwise).
    pub(super) fn users(&self) -> &[ClientUser] {
        match &self.session {
            Session::Server(server) => &server.users,
            _ => &[],
        }
    }

    // --- dialog open/close ---------------------------------------------------

    /// Whether a collaboration modal currently covers the workspace. Used to
    /// stop the editor's hover tooltip from resolving behind dialogs.
    pub(super) fn modal_open(&self) -> bool {
        self.share_open
            || self.join_open
            || self.users_open
            || self.info_open
            || self.stop_confirm
            || self.disconnect_confirm
            || self.password_prompt.is_some()
    }

    /// Opens the share dialog, prefilling the last-used user name and port.
    pub(super) fn open_share_dialog(&mut self, settings: &crate::settings::Settings) {
        self.share_username = settings.collab.username.clone();
        self.share_port = settings.collab.port.to_string();
        self.share_password.clear();
        self.share_open = true;
    }

    /// Opens the join panel, prefilling the user name.
    pub(super) fn open_join_dialog(&mut self, settings: &crate::settings::Settings) {
        if self.is_active() {
            return;
        }
        self.join_username = settings.collab.username.clone();
        self.join_address.clear();
        self.join_password.clear();
        self.join = JoinFlow::Idle;
        self.discovered.clear();
        self.browser = match Browser::start() {
            Ok(browser) => Some(browser),
            Err(err) => {
                log::warn!("mDNS browse failed; manual ip:port still works: {err}");
                None
            }
        };
        self.join_open = true;
    }

    /// Opens the 已连接用户 dialog (server only).
    pub(super) fn open_users_dialog(&mut self) {
        if matches!(self.session, Session::Server(_)) {
            self.users_open = true;
        }
    }

    /// Opens the 连接信息 dialog (client only).
    pub(super) fn open_info_dialog(&mut self) {
        if matches!(self.session, Session::Client(_)) {
            self.info_open = true;
        }
    }

    /// Requests a server shutdown, raising the confirmation dialog.
    pub(super) fn request_stop(&mut self) {
        if matches!(self.session, Session::Server(_)) {
            self.stop_confirm = true;
        }
    }

    /// Requests a client disconnect, raising the confirmation dialog.
    pub(super) fn request_disconnect(&mut self) {
        if matches!(self.session, Session::Client(_)) {
            self.disconnect_confirm = true;
        }
    }

    pub(super) fn cancel_stop(&mut self) {
        self.stop_confirm = false;
    }

    pub(super) fn cancel_disconnect(&mut self) {
        self.disconnect_confirm = false;
    }

    pub(super) fn close_share(&mut self) {
        self.share_open = false;
    }

    pub(super) fn close_users(&mut self) {
        self.users_open = false;
    }

    pub(super) fn close_info(&mut self) {
        self.info_open = false;
    }

    /// Closes the join panel and stops the discovery browse.
    ///
    /// Cancelling while a connect/snapshot is in flight also tears the session
    /// down; cancelling after `Live` only closes the panel. Dropping the
    /// receiver discards an in-flight connect (M2).
    pub(super) fn close_join(&mut self) {
        let cancel = matches!(self.join, JoinFlow::Connecting | JoinFlow::Syncing { .. });
        self.join_open = false;
        self.browser = None;
        self.discovered.clear();
        self.password_prompt = None;
        self.join_rx = None;
        if cancel && self.is_client() {
            self.disconnect();
        } else if !matches!(self.join, JoinFlow::Live) {
            self.join = JoinFlow::Idle;
        }
    }

    // --- lifecycle -----------------------------------------------------------

    /// Starts a server and advertises it over mDNS.
    ///
    /// Returns the success toast for the caller to raise, or an error message.
    /// The dialog stays open on failure so the user can correct the port or
    /// password and retry.
    pub(super) fn start_server(
        &mut self,
        settings: &mut crate::settings::Settings,
        start: ServerStart,
    ) -> Result<ToastMsg, String> {
        let ServerStart {
            root,
            main_file,
            username,
            password,
            port,
            initial_docs,
        } = start;
        let project = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_owned());
        let main_relative = main_file
            .as_deref()
            .and_then(|path| path.strip_prefix(&root).ok())
            .map(|rel| rel.to_string_lossy().replace('\\', "/"));
        // The image dir must live under the root: the wire `ImageAdded` name is a
        // basename, and `Get{File}` resolves only under the root, so the bytes are
        // reachable only when the image dir is inside it. A configured absolute
        // path outside the project falls back to `<root>/image`.
        let configured = settings.workspace.image_dir.trim();
        let mut image_dir = if Path::new(configured).is_absolute() {
            PathBuf::from(configured)
        } else {
            root.join(configured)
        };
        if !image_dir.starts_with(&root) {
            log::warn!(
                "collab image dir {} is outside the project; using <root>/image",
                image_dir.display()
            );
            image_dir = root.join("image");
        }
        let image_dir_rel = image_dir
            .strip_prefix(&root)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let config = ServerConfig {
            root,
            project: project.clone(),
            main_file: main_relative,
            server_user: username.clone(),
            port,
            image_dir,
            max_connections: settings.collab.max_connections,
            idle_timeout: (settings.collab.idle_timeout_secs > 0)
                .then(|| Duration::from_secs(settings.collab.idle_timeout_secs)),
            ready_timeout: Duration::from_secs(30),
            initial_docs,
        };
        let needs_password = password.is_some();
        let server = match CollabServer::start(config, password) {
            Ok(server) => server,
            Err(err) => {
                return Err(t_with("collab.share.failed", &[("err", &err.to_string())]));
            }
        };
        let actual_port = server.port();
        let advertiser = Advertiser::start(&project, &username, needs_password, actual_port).ok();
        if advertiser.is_none() {
            log::warn!("mDNS advertise failed; direct ip:port still works");
        }
        settings.collab.username = username.clone();
        settings.collab.port = actual_port;
        self.reset_session_state();
        self.session = Session::Server(ServerSession {
            server,
            advertiser,
            users: Vec::new(),
            image_dir_rel,
        });
        self.share_open = false;
        self.join_open = false;
        self.join = JoinFlow::Idle;
        self.browser = None;
        self.join_rx = None;
        self.password_prompt = None;
        self.discovered.clear();
        Ok(ToastMsg::success(t_with(
            "collab.share.started",
            &[
                ("user", &username),
                ("project", &project),
                ("port", &actual_port.to_string()),
            ],
        )))
    }

    /// Broadcasts `ServerClosed`, flushes and unregisters.
    pub(super) fn stop_server(&mut self) {
        self.stop_confirm = false;
        if let Session::Server(server) = std::mem::replace(&mut self.session, Session::Off) {
            server.server.stop();
            drop(server.advertiser);
        }
        self.users_open = false;
        self.info_open = false;
        self.join_open = false;
        self.join = JoinFlow::Idle;
        self.browser = None;
        self.join_rx = None;
        self.password_prompt = None;
        self.discovered.clear();
        self.reset_session_state();
    }

    /// Says `Bye` and ends the client session.
    pub(super) fn disconnect(&mut self) {
        self.disconnect_confirm = false;
        if let Session::Client(client) = std::mem::replace(&mut self.session, Session::Off) {
            client.client.disconnect();
        }
        self.info_open = false;
        self.users_open = false;
        self.join_open = false;
        self.join = JoinFlow::Idle;
        self.browser = None;
        self.join_rx = None;
        self.password_prompt = None;
        self.discovered.clear();
        self.reset_session_state();
    }

    /// Begins a join from a set of candidate addresses (discovery or manual).
    pub(super) fn begin_join(
        &mut self,
        addresses: Vec<SocketAddr>,
        username: String,
        password: Option<String>,
    ) -> Option<String> {
        if addresses.is_empty() {
            return Some(t("collab.join.error.unreachable").into_owned());
        }
        if username.trim().is_empty() {
            return Some(t("collab.join.username_required").into_owned());
        }
        if self.is_active() || matches!(self.join, JoinFlow::Connecting | JoinFlow::Syncing { .. })
        {
            return Some(t("collab.join.error.busy").into_owned());
        }
        let user = username.clone();
        let (tx, rx) = bounded(1);
        std::thread::spawn(move || {
            let result = CollabClient::connect(&addresses, &user, password.as_deref());
            let _ = tx.send(result);
        });
        self.reset_session_state();
        self.join_rx = Some(rx);
        self.join = JoinFlow::Connecting;
        self.password_prompt = None;
        None
    }

    /// Parses the manual `ip:port` field and begins a join.
    pub(super) fn join_manual(&mut self) -> Option<String> {
        let Ok(address) = self.join_address.trim().parse::<SocketAddr>() else {
            return Some(t("collab.join.address_invalid").into_owned());
        };
        let username = self.join_username.clone();
        let password = (!self.join_password.is_empty()).then(|| self.join_password.clone());
        self.begin_join(vec![address], username, password)
    }

    /// Selects a discovered row: prompts for a password when required, else joins.
    pub(super) fn join_discovered(&mut self, service: &DiscoveredService) -> Option<String> {
        let addresses = service.addresses.clone();
        if service.needs_password {
            self.password_prompt = Some(addresses);
            self.join_password.clear();
            return None;
        }
        let username = self.join_username.clone();
        self.begin_join(addresses, username, None)
    }

    /// Confirms the password prompt and joins the stored addresses.
    pub(super) fn confirm_password_prompt(&mut self) -> Option<String> {
        let addresses = self.password_prompt.take()?;
        let password = (!self.join_password.is_empty()).then(|| self.join_password.clone());
        let username = self.join_username.clone();
        self.begin_join(addresses, username, password)
    }

    pub(super) fn password_prompt(&self) -> bool {
        self.password_prompt.is_some()
    }

    pub(super) fn cancel_password_prompt(&mut self) {
        self.password_prompt = None;
        self.join_password.clear();
    }

    // --- sync plumbing -------------------------------------------------------

    pub(super) fn is_client(&self) -> bool {
        matches!(self.session, Session::Client(_))
    }

    /// Sends a locally captured change set through the active session.
    pub(super) fn send_changes(&mut self, set: ChangeSet) {
        match &self.session {
            Session::Client(client) => client.client.send_changes(set),
            Session::Server(server) => server.server.submit_local_change(set),
            Session::Off => {}
        }
    }

    /// Sends this participant's cursors for `file` (root-relative path).
    pub(super) fn send_cursors(
        &mut self,
        file: &str,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    ) {
        match &self.session {
            Session::Client(client) => client.client.send_cursors(file, carets, selections),
            Session::Server(server) => {
                server.server.submit_local_cursors(file, carets, selections);
            }
            Session::Off => {}
        }
    }

    /// Requests a server-global undo/redo.
    pub(super) fn request_history(&mut self, op: HistoryOp) {
        match &self.session {
            Session::Client(client) => client.client.request_history(op),
            Session::Server(server) => server.server.request_local_history(op),
            Session::Off => {}
        }
    }

    /// Client Ctrl+S: mirrors locally (done by the app) and asks the server to save.
    pub(super) fn save_file(&mut self, path: &str) {
        if let Session::Client(client) = &self.session {
            client.client.save_file(path);
        }
    }

    /// Requests a file's bytes from the server (image fetch in the mirror).
    pub(super) fn get_file(&mut self, path: &str) {
        if let Session::Client(client) = &self.session {
            client.client.get_file(path);
        }
    }

    /// Requests a chunked file download; `allow_large` is only used after the
    /// user confirms the size prompt.
    pub(super) fn get_chunked_file(&mut self, path: &str, allow_large: bool) {
        if let Session::Client(client) = &self.session {
            client.client.get_chunked_file(path, allow_large);
        }
    }

    /// Sends the client's open-file set (for resync requests) when it changes.
    pub(super) fn set_open_files(&mut self, files: Vec<String>) {
        if self.last_open_files == files {
            return;
        }
        self.last_open_files = files.clone();
        if let Session::Client(client) = &self.session {
            client.client.set_open_files(files);
        }
    }

    /// Forces a resync of `files` (client only): used when a remote change set
    /// fails the local R5 validation, so the divergence is repaired (M1).
    pub(super) fn request_sync(&mut self, files: Vec<String>) {
        if let Session::Client(client) = &self.session {
            client.client.request_sync(files);
        }
    }

    /// Whether local captures may be flushed against `root`: a server always; a
    /// client only once the mirror snapshot landed and the mirror is the live
    /// root, so old-root tabs never broadcast stale paths (m5).
    pub(super) fn ready_for_local_edits(&self, root: Option<&std::path::Path>) -> bool {
        match &self.session {
            Session::Off => false,
            Session::Server(_) => true,
            Session::Client(client) => {
                self.join == JoinFlow::Live && Some(client.mirror.as_path()) == root
            }
        }
    }

    /// Uploads an image (client only), returning the correlation id. The id is
    /// remembered so the matching `ImageAdded` inserts the snippet (m2).
    pub(super) fn upload_image(&mut self, name: &str, bytes: Vec<u8>) -> Option<u64> {
        match &self.session {
            Session::Client(client) => {
                let upload_id = client.client.upload_image(name, bytes);
                self.pending_uploads.push(upload_id);
                Some(upload_id)
            }
            _ => None,
        }
    }

    /// Publishes a server-host image (server only).
    pub(super) fn publish_image(&mut self, name: &str, bytes: Vec<u8>) {
        if let Session::Server(server) = &self.session {
            server.server.publish_local_image(name, bytes);
        }
    }

    /// Remote carets for the active file (`rel` is a root-relative path).
    pub(super) fn remote_carets_for(&self, rel: &str) -> Vec<RemoteCaret> {
        self.cursors
            .values()
            .filter(|cursor| cursor.file == rel)
            .map(|cursor| RemoteCaret {
                user: cursor.user.clone(),
                color_index: cursor.color_index,
                path: PathBuf::from(&cursor.file),
                carets: cursor.carets.iter().map(|byte| *byte as usize).collect(),
                selections: cursor
                    .selections
                    .iter()
                    .map(|(start, end)| (*start as usize, *end as usize))
                    .collect(),
            })
            .collect()
    }

    /// The server-global `(can_undo, can_redo)`.
    pub(super) fn history_state(&self) -> (bool, bool) {
        (self.history_can_undo, self.history_can_redo)
    }

    /// Records this participant's cursors for the throttled send.
    pub(super) fn feed_local_cursors(
        &mut self,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    ) {
        self.cursor_file = file;
        self.cursor_carets = carets;
        self.cursor_selections = selections;
    }

    /// Sends the cached cursors when the throttle interval has elapsed.
    pub(super) fn tick_cursors(&mut self) {
        if !self.is_active() || self.cursor_file.is_empty() {
            return;
        }
        if self.cursor_sent_at.elapsed() < CURSOR_INTERVAL {
            return;
        }
        self.cursor_sent_at = Instant::now();
        let (file, carets, selections) = (
            self.cursor_file.clone(),
            self.cursor_carets.clone(),
            self.cursor_selections.clone(),
        );
        self.send_cursors(&file, carets, selections);
    }

    fn record_cursor(
        cursors: &mut std::collections::BTreeMap<Sid, RemoteCursor>,
        sid: Sid,
        user: String,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    ) {
        let color_index = color_index_for(&user);
        cursors.insert(
            sid,
            RemoteCursor {
                user,
                file,
                carets,
                selections,
                color_index,
            },
        );
    }

    /// Test seam: runs one `ImageStored` through the host snippet filter.
    #[cfg(test)]
    pub(super) fn inject_image_stored_for_test(&mut self, name: &str, by: Sid) -> CollabOutcome {
        let mut outcome = CollabOutcome::default();
        push_host_image_snippet(name.to_owned(), by, &mut outcome);
        outcome
    }

    // --- pump ----------------------------------------------------------------

    /// Drains every collaboration channel for this frame and returns the app
    /// effects to apply.
    pub(super) fn pump(&mut self, ctx: &egui::Context) -> CollabOutcome {
        let mut outcome = CollabOutcome::default();

        self.poll_join(&mut outcome);
        self.poll_discovery();
        self.poll_server(&mut outcome);
        self.poll_client(&mut outcome);

        if let Some(delay) = repaint_delay(
            self.is_active(),
            self.join_rx.is_some() || matches!(self.join, JoinFlow::Connecting),
            self.browser.is_some(),
        ) {
            ctx.request_repaint_after(delay);
        }
        outcome
    }

    fn poll_join(&mut self, outcome: &mut CollabOutcome) {
        let Some(rx) = &self.join_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            // The connecting thread is still working.
            Err(TryRecvError::Empty) => return,
            // The thread died without sending (a panic unwinds it and drops the
            // sender). Treat it as a failed join: otherwise the panel sits on
            // `Connecting` forever and every later frame polls a dead channel.
            Err(TryRecvError::Disconnected) => {
                self.join_rx = None;
                self.join = JoinFlow::Failed;
                outcome
                    .toasts
                    .push(ToastMsg::error(t("collab.join.error.interrupted")));
                return;
            }
        };
        self.join_rx = None;
        match result {
            Ok(client) => {
                let mirror = mirror_dir(&self.join_username);
                if let Err(err) = prepare_mirror(&mirror) {
                    log::warn!("cannot prepare mirror {}: {err}", mirror.display());
                    outcome.toasts.push(ToastMsg::error(t_with(
                        "collab.join.error.failed",
                        &[("err", &err.to_string())],
                    )));
                    self.join = JoinFlow::Failed;
                    return;
                }
                self.session = Session::Client(Box::new(ClientSession {
                    client,
                    user: self.join_username.clone(),
                    project: String::new(),
                    server_user: String::new(),
                    mirror,
                    main_file: None,
                    image_dir: None,
                    mirrored: 0,
                    total: 0,
                    rtt_ms: None,
                    last_ping: Instant::now(),
                    ping_sent_at: None,
                    busy_until: None,
                    ready_sent: false,
                }));
                self.reset_session_state();
                self.join = JoinFlow::Syncing { done: 0, total: 0 };
            }
            Err(error) => {
                outcome.toasts.push(join_error_text(&error));
                self.join = JoinFlow::Failed;
            }
        }
    }

    fn poll_discovery(&mut self) {
        let Some(browser) = &self.browser else {
            return;
        };
        while let Some(event) = browser.try_recv() {
            match event {
                DiscoveryEvent::Discovered(service) => {
                    if let Some(existing) = self
                        .discovered
                        .iter_mut()
                        .find(|known| known.instance == service.instance)
                    {
                        *existing = service;
                    } else {
                        self.discovered.push(service);
                    }
                }
                DiscoveryEvent::Removed(name) => {
                    self.discovered.retain(|service| service.instance != name);
                }
            }
        }
    }

    fn poll_server(&mut self, outcome: &mut CollabOutcome) {
        let mut events = Vec::new();
        if let Session::Server(server) = &self.session {
            while let Some(event) = server.server.try_recv_event() {
                events.push(event);
            }
        }
        let mut stopped = false;
        if let Session::Server(server) = &mut self.session {
            for event in events {
                match event {
                    ServerEvent::ClientJoined { sid, user } => {
                        server.users.retain(|known| known.sid != sid);
                        server.users.push(ClientUser { sid, user });
                    }
                    ServerEvent::ClientLeft { sid, .. } => {
                        server.users.retain(|known| known.sid != sid);
                        self.cursors.remove(&sid);
                    }
                    ServerEvent::ChangeApplied {
                        set,
                        already_applied,
                        ..
                    } => {
                        // Skip only a host edit the editor already applied. A host
                        // undo/redo is routed to the server, so its echo must be
                        // applied to the host buffer here.
                        if !already_applied {
                            outcome.host_applies.push(HostApply::Changes(set));
                        }
                    }
                    ServerEvent::HistoryState { can_undo, can_redo } => {
                        self.history_can_undo = can_undo;
                        self.history_can_redo = can_redo;
                    }
                    ServerEvent::Cursors {
                        sid,
                        user,
                        file,
                        carets,
                        selections,
                    } => {
                        Self::record_cursor(&mut self.cursors, sid, user, file, carets, selections)
                    }
                    ServerEvent::Saved { path, ok } => {
                        outcome.toasts.push(saved_toast(&path, ok));
                    }
                    ServerEvent::ImageStored { name, by, .. } => {
                        push_host_image_snippet(name, by, outcome);
                    }
                    ServerEvent::ResyncFile { path, text } => {
                        outcome.host_applies.push(HostApply::Resync { path, text });
                    }
                    ServerEvent::Stopped => stopped = true,
                }
            }
        }
        if stopped {
            self.session = Session::Off;
            self.join = JoinFlow::Idle;
            self.join_open = false;
            self.browser = None;
            self.join_rx = None;
            self.discovered.clear();
            self.reset_session_state();
            outcome.navigate_landing = true;
        }
    }

    fn poll_client(&mut self, outcome: &mut CollabOutcome) {
        let Session::Client(_) = &self.session else {
            return;
        };
        // Collect events first so the borrow of `self.session` ends before we
        // mutate other controller state.
        let mut events = Vec::new();
        if let Session::Client(client) = &self.session {
            while let Some(event) = client.client.try_recv_event() {
                events.push(event);
            }
        }
        for event in events {
            self.apply_client_event(event, outcome);
        }
        // Latency (`§5.11`): ping every interval; an outstanding ping past the
        // disconnect window ends the session with a toast. Suppressed while the
        // join snapshot is still streaming or the server is busy with another
        // peer's snapshot (the server cannot answer pings then).
        let live = self.join == JoinFlow::Live;
        let mut timed_out = false;
        if let Session::Client(client) = &mut self.session {
            let now = Instant::now();
            let busy = client.busy_until.is_some_and(|until| now < until);
            let outstanding = client.ping_sent_at.map(|sent| sent.elapsed());
            if let Some(tick) = heartbeat_tick(live, busy, outstanding, client.last_ping.elapsed())
            {
                match tick {
                    PingTick::Timeout => timed_out = true,
                    PingTick::Send => {
                        client.client.ping();
                        client.last_ping = Instant::now();
                        client.ping_sent_at = Some(Instant::now());
                    }
                    PingTick::Wait => {}
                }
            }
        }
        if timed_out {
            outcome
                .toasts
                .push(ToastMsg::warning(t("collab.toast.timeout")));
            self.disconnect();
            outcome.navigate_landing = true;
        }
    }

    fn apply_client_event(&mut self, event: ClientEvent, outcome: &mut CollabOutcome) {
        match event {
            ClientEvent::Joined {
                project,
                server_user,
                users,
                cursors,
                ..
            } => {
                if let Session::Client(client) = &mut self.session {
                    client.project = project;
                    client.server_user = server_user;
                }
                // Seed the participant list and the cursor table from `Joined`.
                self.client_users = users
                    .into_iter()
                    .map(|user| ClientUser {
                        sid: user.sid,
                        user: user.user,
                    })
                    .collect();
                let names: std::collections::BTreeMap<Sid, String> = self
                    .client_users
                    .iter()
                    .map(|user| (user.sid, user.user.clone()))
                    .collect();
                for cursor in cursors {
                    let user = names
                        .get(&cursor.sid)
                        .cloned()
                        .unwrap_or_else(|| format!("{}", cursor.sid));
                    Self::record_cursor(
                        &mut self.cursors,
                        cursor.sid,
                        user,
                        cursor.file,
                        cursor.carets,
                        cursor.selections,
                    );
                }
            }
            ClientEvent::Tree { entries } => {
                let total = entries.iter().filter(|entry| !entry.is_dir).count();
                if let Session::Client(client) = &mut self.session {
                    client.total = total;
                }
                self.join.on_files(total);
            }
            ClientEvent::Config {
                main_file,
                image_dir,
                ..
            } => {
                if let Session::Client(client) = &mut self.session {
                    client.main_file = main_file;
                    client.image_dir = image_dir;
                }
            }
            ClientEvent::FileText { path, text } => {
                let mirror = match &self.session {
                    Session::Client(client) => Some(client.mirror.clone()),
                    _ => None,
                };
                if let Some(mirror) = mirror {
                    match join_rel(&mirror, &path) {
                        Some(target) => {
                            if let Err(err) =
                                crate::platform::fs_ops::write_mirror(&target, text.as_bytes())
                            {
                                log::warn!("mirror write {} failed: {err}", target.display());
                            }
                        }
                        None => log::warn!("refusing unsafe mirror path {path:?}"),
                    }
                }
                if self.join == JoinFlow::Live {
                    outcome.downloaded.push(path);
                } else if let Session::Client(client) = &mut self.session {
                    client.mirrored += 1;
                    self.join.on_file();
                }
            }
            ClientEvent::SnapshotComplete => {
                let (mirror, main) = match &self.session {
                    Session::Client(client) => (
                        client.mirror.clone(),
                        client
                            .main_file
                            .as_deref()
                            .map(|rel| client.mirror.join(rel)),
                    ),
                    _ => return,
                };
                if let Err(err) = workspace::set_main_file(&mirror, main.as_deref()) {
                    log::warn!("cannot write mirror marker: {err}");
                }
                let (user, project) = match &mut self.session {
                    Session::Client(client) => {
                        if !client.ready_sent {
                            client.client.ready();
                            client.ready_sent = true;
                        }
                        client.total = client.total.max(client.mirrored);
                        // The join snapshot is over: reset the heartbeat clock so
                        // the first ping is sent two seconds after Live, not two
                        // seconds after the client session was installed.
                        client.last_ping = Instant::now();
                        client.ping_sent_at = None;
                        client.busy_until = None;
                        (client.user.clone(), client.project.clone())
                    }
                    _ => return,
                };
                self.join = JoinFlow::Live;
                // The join succeeded: close the panel and stop discovery. Leaving
                // it open used to trap users into clicking Cancel, which set
                // `join` back to Idle and silently stopped local edit sync.
                self.join_open = false;
                self.browser = None;
                self.discovered.clear();
                self.password_prompt = None;
                outcome.open_root = Some(mirror);
                outcome.toasts.push(ToastMsg::success(t_with(
                    "collab.join.connected",
                    &[("user", &user), ("project", &project)],
                )));
            }
            ClientEvent::Pong { rtt, .. } => {
                if let Session::Client(client) = &mut self.session {
                    client.rtt_ms = Some(rtt.as_millis() as u64);
                    client.ping_sent_at = None;
                }
            }
            ClientEvent::Busy { busy } => {
                if let Session::Client(client) = &mut self.session {
                    client.busy_until = busy.then(|| Instant::now() + SERVER_BUSY_WINDOW);
                    // Drop any outstanding ping: its answer may be stuck behind
                    // the server's snapshot/resync and must not count towards
                    // timeout.
                    client.ping_sent_at = None;
                    client.last_ping = Instant::now();
                }
            }
            ClientEvent::Users { list } => {
                self.client_users = list
                    .into_iter()
                    .map(|user| ClientUser {
                        sid: user.sid,
                        user: user.user,
                    })
                    .collect();
            }
            ClientEvent::ServerClosed => {
                outcome
                    .toasts
                    .push(ToastMsg::warning(t("collab.toast.server_closed")));
                self.disconnect();
                outcome.navigate_landing = true;
            }
            ClientEvent::Disconnected => {
                if self.is_active() {
                    outcome
                        .toasts
                        .push(ToastMsg::warning(t("collab.toast.disconnected")));
                }
                self.disconnect();
                outcome.navigate_landing = true;
            }
            ClientEvent::Rejected { error } => {
                outcome
                    .toasts
                    .push(join_error_text(&ClientError::Rejected(error)));
                self.disconnect();
            }
            // Phase 4: apply remote edits/cursors/images/saves from these events.
            ClientEvent::Changes { set, .. } => outcome.remote_changes.push(set),
            ClientEvent::Cursors {
                sid,
                user,
                file,
                carets,
                selections,
                ..
            } => Self::record_cursor(&mut self.cursors, sid, user, file, carets, selections),
            ClientEvent::HistoryState {
                can_undo, can_redo, ..
            } => {
                self.history_can_undo = can_undo;
                self.history_can_redo = can_redo;
            }
            ClientEvent::SyncState { files, applied, .. } => {
                outcome.resync_files = files
                    .into_iter()
                    .map(|file| (file.path, file.text))
                    .collect();
                outcome.resync_applied = applied;
                outcome.resynced = true;
            }
            ClientEvent::Saved { path, ok, .. } => outcome.toasts.push(saved_toast(&path, ok)),
            ClientEvent::ImageAdded {
                upload_id, name, ..
            } => {
                // Correlate by upload id: our own upload inserts a snippet, every
                // `ImageAdded` also fetches the bytes into the mirror.
                if let Some(index) = self
                    .pending_uploads
                    .iter()
                    .position(|pending| *pending == upload_id)
                {
                    self.pending_uploads.remove(index);
                    outcome.insert_images.push(name.clone());
                }
                outcome.fetch_images.push(name);
            }
            ClientEvent::FileBin { path, bytes } => {
                if let Session::Client(client) = &mut self.session
                    && self.join != JoinFlow::Live
                {
                    if let Some(target) = join_rel(&client.mirror, &path) {
                        if let Err(err) = crate::platform::fs_ops::write_mirror(&target, &bytes) {
                            log::warn!("mirror write {} failed: {err}", target.display());
                        }
                    } else {
                        log::warn!("refusing unsafe mirror path {path:?}");
                    }
                    client.mirrored += 1;
                    self.join.on_file();
                } else {
                    outcome.remote_bins.push((path, bytes));
                }
            }
            ClientEvent::FileChunk {
                path,
                offset,
                total,
                bytes,
            } => outcome.file_chunks.push((path, offset, total, bytes)),
            ClientEvent::FileTooLarge { path, size } => outcome.too_large.push((path, size)),
            ClientEvent::FileUnavailable { path } => outcome.unavailable.push(path),
            ClientEvent::LocalEditDropped => outcome
                .toasts
                .push(ToastMsg::warning(t("collab.toast.concurrent_lost"))),
            ClientEvent::Desync | ClientEvent::YouAreStale => outcome
                .toasts
                .push(ToastMsg::info(t("collab.toast.resync_triggered"))),
            ClientEvent::Resyncing { .. } => {}
        }
    }

    /// The status-bar text for the active session (`§5.11`).
    pub(super) fn status_text(&self) -> Option<String> {
        status_text(&self.session)
    }

    /// Whether the join panel should render its scanning placeholder.
    pub(super) fn join_scanning(&self) -> bool {
        matches!(self.join, JoinFlow::Connecting) && self.discovered.is_empty()
    }

    /// Test seam: runs one inbound `ClientEvent` through the receive path.
    #[cfg(test)]
    pub(super) fn inject_event_for_test(&mut self, event: ClientEvent) {
        let mut outcome = CollabOutcome::default();
        self.apply_client_event(event, &mut outcome);
    }
}

/// The repaint interval the collaboration layer needs this frame, or `None` when
/// it is idle and can wait for input.
///
/// A live session ticks for latency and cursor traffic. A join or a discovery
/// browse that is still in flight ticks faster: their results arrive on
/// background channels, and without a timer the panel would only repaint on the
/// next mouse or key event — the "正在扫描…" freeze (M1).
fn repaint_delay(active: bool, join_pending: bool, browsing: bool) -> Option<Duration> {
    if active {
        Some(Duration::from_secs(1))
    } else if join_pending || browsing {
        Some(Duration::from_millis(100))
    } else {
        None
    }
}

/// The status text for `session`, or `None` when off.
pub(super) fn status_text(session: &Session) -> Option<String> {
    match session {
        Session::Off => None,
        Session::Server(server) => Some(server_status_text(server.users.len() + 1)),
        Session::Client(client) => {
            let stale = client
                .ping_sent_at
                .is_some_and(|sent| sent.elapsed().as_millis() as u64 > RTT_TIMEOUT_MS);
            Some(client_status_text(
                &client.server_user,
                &client.project,
                client.rtt_ms,
                stale,
            ))
        }
    }
}

/// `N 人协作` for a server hosting `count` participants.
pub(super) fn server_status_text(count: usize) -> String {
    t_with("collab.status.server", &[("count", &count.to_string())])
}

/// `已连接 user@project · rtt ms`, with a timeout variant while the link is
/// stale (an outstanding ping past the threshold) or the last RTT was slow.
pub(super) fn client_status_text(
    server_user: &str,
    project: &str,
    rtt_ms: Option<u64>,
    stale: bool,
) -> String {
    if stale || rtt_ms.is_some_and(|ms| ms > RTT_TIMEOUT_MS) {
        return t_with(
            "collab.status.client_timeout",
            &[("user", server_user), ("project", project)],
        );
    }
    let rtt = rtt_ms.map_or_else(|| "–".to_owned(), |ms| ms.to_string());
    t_with(
        "collab.status.client",
        &[("user", server_user), ("project", project), ("rtt", &rtt)],
    )
}

/// Formats the join progress text, or `None` outside the syncing state.
pub(super) fn progress_text(flow: JoinFlow) -> Option<String> {
    match flow {
        JoinFlow::Syncing { done, total } => Some(t_with(
            "collab.join.syncing",
            &[("done", &done.to_string()), ("total", &total.to_string())],
        )),
        JoinFlow::Connecting => Some(t("collab.join.connecting").into_owned()),
        _ => None,
    }
}

/// The toast for a server-side save result.
fn saved_toast(path: &str, ok: bool) -> ToastMsg {
    if ok {
        ToastMsg::success(t_with("toast.saved", &[("name", path)]))
    } else {
        ToastMsg::error(t_with("collab.toast.save_failed", &[("path", path)]))
    }
}

/// Queues the snippet for an image the host itself published.
///
/// `ServerEvent::ImageStored` fires for every store — a remote client upload
/// included — so only the host's own publish (`by == SERVER_SID`) inserts a
/// snippet; a remote upload must insert nothing on the host (NEW-1).
fn push_host_image_snippet(name: String, by: Sid, outcome: &mut CollabOutcome) {
    if by == SERVER_SID {
        outcome.insert_images.push(name);
    }
}

/// The toast for a failed connect/handshake.
fn join_error_text(error: &ClientError) -> ToastMsg {
    let message = match error {
        ClientError::Rejected(RejectKind::Auth) | ClientError::ServerProofFailed => {
            t("collab.join.error.wrong_password").into_owned()
        }
        ClientError::Rejected(RejectKind::Version) => t("collab.join.error.version").into_owned(),
        ClientError::Rejected(RejectKind::Full) => t("collab.join.error.full").into_owned(),
        ClientError::PasswordRequired => t("collab.join.error.password_required").into_owned(),
        ClientError::Unreachable => t("collab.join.error.unreachable").into_owned(),
        ClientError::HandshakeTimeout => t("collab.join.error.handshake_timeout").into_owned(),
        _ => t_with("collab.join.error.failed", &[("err", &error.to_string())]),
    };
    ToastMsg::error(message)
}

/// A fresh, wiped mirror directory under the platform cache dir for `project`.
fn mirror_dir(project: &str) -> PathBuf {
    let base = config_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("typ-edit-collab"))
        .join("collab");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let slug: String = project
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    base.join(format!("{slug}-{}-{stamp}", std::process::id()))
}

/// Creates an empty mirror directory, wiping any stale content.
fn prepare_mirror(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        crate::platform::fs_ops::remove_dir_all(dir)?;
    }
    crate::platform::fs_ops::create_dir_all(dir)
}

/// Sorts and filters discovered services for the join list (stable order).
pub(super) fn filter_services(services: &[DiscoveredService]) -> Vec<DiscoveredService> {
    let mut list: Vec<DiscoveredService> = services
        .iter()
        .filter(|service| !service.project.trim().is_empty() && !service.addresses.is_empty())
        .cloned()
        .collect();
    list.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then_with(|| a.user.cmp(&b.user))
            .then_with(|| a.instance.cmp(&b.instance))
    });
    list
}

// ---- App-side wiring: capture flush, remote apply, cursor feed --------------

/// Converts a captured local edit to a wire op (insert when no bytes removed).
fn captured_to_op(edit: &CapturedEdit) -> ChangeOp {
    if edit.delete_bytes == 0 {
        ChangeOp::Insert {
            at: edit.at_byte as u64,
            text: edit.insert.clone(),
        }
    } else {
        ChangeOp::Delete {
            at: edit.at_byte as u64,
            text: edit.insert.clone(),
        }
    }
}

/// Converts a wire op to a captured edit for `apply_remote_edits`.
fn op_to_captured(op: &ChangeOp) -> CapturedEdit {
    match op {
        ChangeOp::Insert { at, text } => CapturedEdit {
            at_byte: *at as usize,
            delete_bytes: 0,
            insert: text.clone(),
        },
        ChangeOp::Delete { at, text } => CapturedEdit {
            at_byte: *at as usize,
            delete_bytes: text.len(),
            insert: text.clone(),
        },
    }
}

impl super::TypEditApp {
    /// The host's dirty open buffers as `(root-relative path, text)` pairs.
    ///
    /// Starting collaboration seeds these into the server's DocStore before the
    /// first accept, so a joiner never receives the stale on-disk version of a
    /// file the host is still editing (and the host's first change set is based
    /// on the same text the server validates against).
    pub(super) fn collab_initial_docs(&self) -> Vec<(String, TextSnapshot)> {
        let Some(root) = self.tree.root.as_deref() else {
            return Vec::new();
        };
        self.session
            .tabs
            .iter()
            .filter(|tab| tab.dirty)
            .filter_map(|tab| {
                let path = tab.editor.path.as_deref()?;
                let rel = path.strip_prefix(root).ok()?;
                Some((
                    rel.to_string_lossy().replace('\\', "/"),
                    tab.editor.text_snapshot(),
                ))
            })
            .collect()
    }

    /// Drains every tab's captured local edits and sends them as change sets.
    ///
    /// THE ONE-FRAME-DELAY MODEL: an edit captured during UI frame N is flushed
    /// at logic frame N+1, so the optimistic buffer and its wire change set never
    /// interleave with the same frame's paint. Capture is enabled on every open
    /// tab while a session is active (the tab-open path also enables it, so an
    /// edit made in the frame before registration is never lost); the flush
    /// itself is gated on the session actually editing the live root (m5).
    pub(super) fn flush_collab_captures(&mut self) {
        if !self.collab.is_active() {
            return;
        }
        for tab in &mut self.session.tabs {
            let _ = tab.editor.enable_edit_capture();
        }
        let root = self.tree.root.clone();
        if !self.collab.ready_for_local_edits(root.as_deref()) {
            return;
        }
        let Some(root) = root else {
            return;
        };
        let mut changes = Vec::new();
        let mut open_files = Vec::new();
        for tab in &mut self.session.tabs {
            let Some(path) = tab.editor.path.as_ref() else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(&root) else {
                continue;
            };
            let file = rel.to_string_lossy().replace('\\', "/");
            open_files.push(file.clone());
            let edits = tab.editor.enable_edit_capture().take();
            if edits.is_empty() {
                continue;
            }
            let ops = edits.iter().map(captured_to_op).collect();
            changes.push(ChangeSet::new(file, ops));
        }
        self.collab.set_open_files(open_files);
        for set in changes {
            self.collab.send_changes(set);
        }
    }

    /// Sends one tab's captured edits as a change set immediately.
    ///
    /// Used by format-on-save: the format edit is flushed BEFORE the server-side
    /// `Save`, so the Save's `g_expected` includes it and the server persists the
    /// formatted content instead of the pre-format bytes (NEW-3).
    pub(super) fn flush_tab_collab_edits(&mut self, index: usize) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        if !self.collab.ready_for_local_edits(Some(root.as_path())) {
            return;
        }
        let Some(tab) = self.session.tabs.get_mut(index) else {
            return;
        };
        let Some(path) = tab.editor.path.as_ref() else {
            return;
        };
        let Ok(rel) = path.strip_prefix(&root) else {
            return;
        };
        let file = rel.to_string_lossy().replace('\\', "/");
        let edits = tab.editor.enable_edit_capture().take();
        if edits.is_empty() {
            return;
        }
        let ops = edits.iter().map(captured_to_op).collect();
        self.collab.send_changes(ChangeSet::new(file, ops));
    }

    /// Writes coalesced remote-edit mirror files, when due or forced.
    ///
    /// A client's mirror is a disposable cache, so it uses the fast path without
    /// per-file fsync. The server host's root holds real documents, so those
    /// keep the atomic replace-and-sync path.
    pub(super) fn flush_mirror_writes(&mut self, force: bool) {
        let fast = self.collab.is_client();
        for (path, text) in self.collab.take_mirror_writes(Instant::now(), force) {
            let result = if fast {
                crate::platform::fs_ops::write_mirror(&path, text.as_bytes())
            } else {
                crate::platform::fs_ops::write_atomic(&path, text.as_bytes())
            };
            if let Err(err) = result {
                log::warn!("mirror write {} failed: {err}", path.display());
            }
        }
    }

    /// Applies a `pump` outcome: toasts, mirror open, resync, remote changes and
    /// images, in that order.
    pub(super) fn apply_collab_outcome(&mut self, outcome: CollabOutcome) {
        for toast in outcome.toasts {
            self.raise_toast(toast);
        }
        if let Some(root) = outcome.open_root {
            // A join mirror replaces the workspace: flush the old mirror first,
            // then drop old-root tabs so nothing broadcasts stale paths during
            // the sync window (m5). Opening the mirror records it in the MRU (v1).
            self.flush_mirror_writes(true);
            self.reset_tabs_for_mirror();
            self.open_root(root);
        }
        if outcome.navigate_landing {
            self.ui.landing = true;
            self.downloads = Default::default();
        }
        for (path, size) in outcome.too_large {
            self.downloads.too_large(&path, size);
        }
        for path in outcome.unavailable {
            self.downloads.unavailable(&path);
            self.toast_error(t_with("downloads.failed_toast", &[("path", &path)]));
        }
        let mirror = self.tree.root.clone();
        let mut chunk_complete = false;
        for (path, offset, total, bytes) in outcome.file_chunks {
            if let Some(mirror) = &mirror
                && self
                    .downloads
                    .progress(&path, offset, total, &bytes, mirror)
            {
                chunk_complete = true;
            }
        }
        if chunk_complete {
            self.trigger_compile();
        }
        // Resync snapshot first, then its trailing change sets, in order.
        for (path, text) in outcome.resync_files {
            self.apply_resync_file(&path, &text);
        }
        for set in outcome.resync_applied {
            self.apply_remote_change_set(&set);
        }
        for set in outcome.remote_changes {
            self.apply_remote_change_set(&set);
        }
        // Host-side applies keep the server's exact event order: a remote change
        // must not be applied after a newer authoritative resync reply (or vice
        // versa).
        for apply in outcome.host_applies {
            match apply {
                HostApply::Changes(set) => self.apply_remote_change_set(&set),
                HostApply::Resync { path, text } => {
                    self.apply_authoritative_text(&path, text.as_deref());
                    self.toast_warning(t("collab.toast.host_resynced"));
                }
            }
        }
        // Images: fetch the bytes for every `ImageAdded`, then insert our own
        // snippets (matched by upload id).
        for name in outcome.fetch_images {
            self.collab_fetch_image(&name);
        }
        for name in outcome.insert_images {
            self.collab_insert_image(&name);
        }
        let fast_bins = self.collab.is_client();
        for (path, bytes) in outcome.remote_bins {
            if let Some(root) = self.tree.root.clone() {
                let Some(target) = join_rel(&root, &path) else {
                    log::warn!("refusing unsafe mirror path {path:?}");
                    continue;
                };
                let result = if fast_bins {
                    crate::platform::fs_ops::write_mirror(&target, &bytes)
                } else {
                    crate::platform::fs_ops::write_atomic(&target, &bytes)
                };
                if let Err(err) = result {
                    log::warn!("mirror binary write failed: {err}");
                    self.downloads.unavailable(&path);
                } else {
                    self.downloads.complete(&path);
                }
                self.trigger_compile();
            }
        }
        for path in outcome.downloaded {
            self.downloads.complete(&path);
            self.trigger_compile();
        }
        if outcome.resynced {
            self.trigger_parse();
            self.compile.note_edit(Instant::now());
            self.toast_success(t("collab.toast.resynced"));
        }
        if outcome.navigate_landing {
            self.flush_mirror_writes(true);
        }
    }

    /// Replaces the workspace with a single fresh tab (the join-mirror switch).
    fn reset_tabs_for_mirror(&mut self) {
        self.session.tabs.clear();
        self.session.active_tab = 0;
        let id = self.session.alloc_tab_id();
        self.session.tabs.push(super::state::EditorTab::new(id));
    }

    /// Replaces one resync snapshot file: always on the mirror disk, and in an
    /// open tab's buffer too.
    fn apply_resync_file(&mut self, rel: &str, text: &str) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let Some(target) = join_rel(&root, rel) else {
            log::warn!("refusing unsafe resync path {rel:?}");
            return;
        };
        let paths: Vec<Option<PathBuf>> = self
            .session
            .tabs
            .iter()
            .map(|tab| tab.editor.path.clone())
            .collect();
        if let Some(index) = super::tabs::find_open_tab(&paths, &target) {
            self.session.tabs[index]
                .editor
                .replace_all_text_remote(text);
            self.session.tabs[index].dirty = true;
            if index == self.session.active_tab {
                self.trigger_parse();
            }
        }
        self.collab
            .queue_mirror_write(target, text.to_owned(), Instant::now());
        self.compile.note_edit(Instant::now());
    }

    /// Fetches an image by basename into the mirror (client only).
    ///
    /// The wire path carries the configured image-dir prefix because the server's
    /// `Get{File}` resolves only under the root and the image lives in that dir.
    fn collab_fetch_image(&mut self, name: &str) {
        if !self.collab.is_client() {
            return;
        }
        let image_dir = self
            .collab
            .image_dir_rel(self.settings.workspace.image_dir.trim());
        let path = image_rel_path(&image_dir, name);
        self.collab.get_file(&path);
    }

    /// Inserts the `#image(...)` snippet for one of our own uploaded images.
    fn collab_insert_image(&mut self, name: &str) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let image_dir = self
            .collab
            .image_dir_rel(self.settings.workspace.image_dir.trim());
        let rel = image_rel_path(&image_dir, name);
        let Some(target) = join_rel(&root, &rel) else {
            return;
        };
        if let Some(form) = self.image_form_at(self.compile.cursor_byte) {
            self.insert_image_snippet(&target, form);
        }
    }

    /// Applies one accepted remote change set to an open tab or the mirror.
    fn apply_remote_change_set(&mut self, set: &ChangeSet) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let Some(target) = join_rel(&root, &set.file) else {
            // A peer-supplied path that escapes the root is never written; ask for
            // a clean snapshot of the file instead (M1).
            log::warn!("refusing unsafe change-set path {:?}", set.file);
            self.toast_error(t("collab.toast.apply_failed"));
            self.request_file_resync(&set.file);
            return;
        };
        let paths: Vec<Option<PathBuf>> = self
            .session
            .tabs
            .iter()
            .map(|tab| tab.editor.path.clone())
            .collect();
        if let Some(index) = super::tabs::find_open_tab(&paths, &target) {
            let edits: Vec<CapturedEdit> = set.ops.iter().map(op_to_captured).collect();
            if self.session.tabs[index].editor.apply_remote_edits(&edits) {
                self.session.tabs[index].dirty = true;
                // Keep the mirror equal to the authoritative content (the open
                // tab's buffer is the overlay, the mirror the on-disk copy).
                // Share the editor's `Arc<String>` instead of copying a large
                // buffer on every remote keystroke.
                let text = self.session.tabs[index].editor.shared_text();
                self.collab
                    .queue_mirror_write_shared(target, text, Instant::now());
                if index == self.session.active_tab {
                    self.trigger_parse();
                } else {
                    self.compile.note_edit(Instant::now());
                }
            } else {
                // The crate already advanced its frontier, so the drop must not be
                // silent: resync the file (M1). The host has no `Sync` channel, so
                // it asks its own core for the authoritative text.
                self.toast_error(t("collab.toast.apply_failed"));
                self.request_file_resync(&set.file);
            }
            return;
        }
        // Closed file: apply to the mirror on disk, then recompile.
        let content = self
            .collab
            .pending_mirror_text(&target)
            .map(str::to_owned)
            .or_else(|| crate::platform::fs_ops::read_to_string(&target).ok())
            .unwrap_or_default();
        match apply_ops(&content, &set.ops) {
            Ok(next) => {
                self.collab.queue_mirror_write(target, next, Instant::now());
                self.compile.note_edit(Instant::now());
            }
            Err(reason) => {
                log::warn!("mirror apply failed for {}: {reason}", set.file);
                self.request_file_resync(&set.file);
            }
        }
    }

    /// Repairs one divergent file using the active role's resync channel.
    fn request_file_resync(&mut self, file: &str) {
        if self.collab.is_server() {
            self.collab.request_doc(file);
        } else {
            self.collab.request_sync(vec![file.to_owned()]);
        }
    }

    /// Replaces an open tab (or the mirror) with the server's authoritative text
    /// after the host's local edit was rejected or a remote apply failed.
    fn apply_authoritative_text(&mut self, rel: &str, text: Option<&str>) {
        let Some(text) = text else {
            log::warn!("server has no authoritative text for {rel}");
            return;
        };
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let Some(target) = join_rel(&root, rel) else {
            log::warn!("refusing unsafe resync path {rel:?}");
            return;
        };
        let paths: Vec<Option<PathBuf>> = self
            .session
            .tabs
            .iter()
            .map(|tab| tab.editor.path.clone())
            .collect();
        if let Some(index) = super::tabs::find_open_tab(&paths, &target) {
            self.session.tabs[index]
                .editor
                .replace_all_text_remote(text);
            self.session.tabs[index].dirty = true;
            if index == self.session.active_tab {
                self.trigger_parse();
            } else {
                self.compile.note_edit(Instant::now());
            }
        }
        self.collab
            .queue_mirror_write(target, text.to_owned(), Instant::now());
    }
}

/// The root-relative image path: the configured image dir plus the basename.
fn image_rel_path(image_dir: &str, name: &str) -> String {
    let dir = image_dir.trim_matches(|ch| ch == '/' || ch == '\\');
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Joins a root-relative forward-slash path onto `root`, rejecting every part
/// that would escape it.
///
/// Mirror paths are **untrusted input**: they arrive from the peer over the wire
/// (`FileText`, `FileBin`, `SyncState`), and the server-side `safe_join` is no
/// defence here because the client never sees it. Without this check `push` would
/// walk out of the workspace on `..`, an absolute path (`/etc/passwd`), or a
/// Windows drive prefix (`C:`), which makes `push` drop the accumulated path
/// outright and write anywhere on the client's disk.
pub(super) fn join_rel(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let normalized = rel.replace('\\', "/");
    if normalized.starts_with('/') {
        return None;
    }
    let mut path = root.to_path_buf();
    for part in normalized.split('/').filter(|part| !part.is_empty()) {
        if !matches!(
            std::path::Path::new(part).components().next(),
            Some(std::path::Component::Normal(_))
        ) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_flow_reducer_counts_files() {
        let mut flow = JoinFlow::Idle;
        flow.on_files(3);
        assert_eq!(flow, JoinFlow::Syncing { done: 0, total: 3 });
        flow.on_file();
        flow.on_file();
        assert_eq!(flow, JoinFlow::Syncing { done: 2, total: 3 });
        // Never overcounts past the advertised total.
        flow.on_file();
        flow.on_file();
        assert_eq!(flow, JoinFlow::Syncing { done: 3, total: 3 });
    }

    #[test]
    fn join_flow_reducer_updates_total_without_losing_progress() {
        let mut flow = JoinFlow::Syncing { done: 2, total: 4 };
        flow.on_files(10);
        assert_eq!(flow, JoinFlow::Syncing { done: 2, total: 10 });
        flow.on_files(1);
        assert_eq!(flow, JoinFlow::Syncing { done: 1, total: 1 });
    }

    #[test]
    fn progress_text_formats_done_of_total() {
        let text = progress_text(JoinFlow::Syncing { done: 3, total: 7 }).expect("syncing text");
        assert!(text.contains('3') && text.contains('7'), "got {text:?}");
    }

    #[test]
    fn collab_handshake_timeout_has_a_localized_toast() {
        let toast = join_error_text(&ClientError::HandshakeTimeout);
        assert_eq!(
            toast.message,
            t("collab.join.error.handshake_timeout").as_ref()
        );
        assert_eq!(toast.kind, crate::theme::SemanticKind::Error);
    }

    #[test]
    fn filter_services_skips_empty_and_sorts() {
        let make = |project: &str, user: &str| DiscoveredService {
            instance: format!("{project}-{user}"),
            project: project.to_owned(),
            user: user.to_owned(),
            needs_password: false,
            port: 39950,
            addresses: vec!["127.0.0.1:39950".parse().expect("addr")],
        };
        let mut blank = make("beta", "zoe");
        blank.project.clear();
        let services = vec![make("zeta", "ann"), blank, make("alpha", "bob")];
        let filtered = filter_services(&services);
        assert_eq!(
            filtered
                .iter()
                .map(|service| service.project.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn repaint_delay_covers_a_pending_join_and_a_browse() {
        // An idle controller asks for no timer at all.
        assert_eq!(repaint_delay(false, false, false), None);
        // A pending join and an open discovery browse both keep frames coming so
        // their channel results are drawn without user input.
        assert_eq!(
            repaint_delay(false, true, false),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            repaint_delay(false, false, true),
            Some(Duration::from_millis(100))
        );
        // A live session keeps its one-second tick.
        assert_eq!(
            repaint_delay(true, false, false),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn join_rel_keeps_root_relative_paths_inside_the_root() {
        let root = std::path::Path::new("/mirror/proj");
        assert_eq!(
            join_rel(root, "main.typ"),
            Some(PathBuf::from("/mirror/proj/main.typ"))
        );
        assert_eq!(
            join_rel(root, "sub/dir/a.typ"),
            Some(PathBuf::from("/mirror/proj/sub/dir/a.typ"))
        );
    }

    #[test]
    fn join_rel_refuses_every_escaping_path() {
        let root = std::path::Path::new("/mirror/proj");
        for bad in [
            "../evil.txt",
            "..\\..\\evil.txt",
            "sub/../../evil.txt",
            "/etc/passwd",
            "//server/share/x",
        ] {
            assert_eq!(join_rel(root, bad), None, "must refuse {bad:?}");
        }
        // A Windows drive prefix must never leave the root: on Windows it is a
        // `Prefix` component and is refused outright, elsewhere it is an ordinary
        // directory name that simply stays under the root.
        if let Some(path) = join_rel(root, "C:/Windows/win.ini") {
            assert!(
                path.starts_with(root),
                "escaped the root: {}",
                path.display()
            );
        }
    }

    #[test]
    fn status_text_maps_session_kinds() {
        assert_eq!(status_text(&Session::Off), None);
        assert!(server_status_text(3).contains('3'));
        let live = client_status_text("srv", "proj", Some(12), false);
        assert!(live.contains("srv") && live.contains("proj") && live.contains("12"));
        let slow = client_status_text("srv", "proj", Some(RTT_TIMEOUT_MS + 1), false);
        assert_ne!(slow, live, "a slow link uses the timeout copy");
        let stale = client_status_text("srv", "proj", Some(12), true);
        assert_ne!(stale, live, "a stale link uses the timeout copy");
    }

    #[test]
    fn ping_tick_pins_the_timeout_semantics() {
        // No outstanding ping: wait below the interval, send at it.
        assert_eq!(ping_tick(None, Duration::from_millis(500)), PingTick::Wait);
        assert_eq!(ping_tick(None, Duration::from_secs(2)), PingTick::Send);
        // An outstanding ping below the window waits and never resends, so
        // `ping_sent_at` keeps ageing towards the disconnect window.
        assert_eq!(
            ping_tick(Some(Duration::from_secs(2)), Duration::from_secs(10)),
            PingTick::Wait
        );
        // Past the disconnect window it times out.
        assert_eq!(
            ping_tick(Some(Duration::from_secs(5)), Duration::ZERO),
            PingTick::Timeout
        );
    }

    #[test]
    fn collab_heartbeat_pauses_while_syncing_or_server_busy() {
        let outstanding = Some(Duration::from_secs(10));
        assert_eq!(
            heartbeat_tick(false, false, outstanding, Duration::ZERO),
            None,
            "a joining client must not time out while the snapshot streams"
        );
        assert_eq!(
            heartbeat_tick(true, true, outstanding, Duration::ZERO),
            None,
            "a live client pauses while the server is busy with another snapshot"
        );
        assert_eq!(
            heartbeat_tick(true, false, outstanding, Duration::ZERO),
            Some(PingTick::Timeout)
        );
        assert_eq!(
            heartbeat_tick(true, false, None, PING_INTERVAL),
            Some(PingTick::Send)
        );
    }

    #[test]
    fn host_snippet_only_for_its_own_publish() {
        let mut controller = CollabController::default();
        // A remote client upload (`by != SERVER_SID`) inserts nothing on the host.
        let outcome = controller.inject_image_stored_for_test("remote.png", 5);
        assert!(
            outcome.insert_images.is_empty(),
            "a client upload must not insert a snippet on the host"
        );
        // The host's own publish inserts the snippet.
        let outcome = controller.inject_image_stored_for_test("host.png", SERVER_SID);
        assert_eq!(outcome.insert_images, vec!["host.png".to_owned()]);
    }

    #[test]
    fn mirror_writes_coalesce_until_the_debounce() {
        let mut controller = CollabController::default();
        let now = Instant::now();
        let path = PathBuf::from("mirror/main.typ");
        controller.queue_mirror_write(path.clone(), "one".to_owned(), now);
        controller.queue_mirror_write(
            path.clone(),
            "two".to_owned(),
            now + Duration::from_millis(10),
        );
        assert_eq!(controller.pending_mirror_text(&path), Some("two"));
        assert!(
            controller
                .take_mirror_writes(now + Duration::from_millis(50), false)
                .is_empty(),
            "the first burst must not be written before the debounce"
        );
        let writes = controller.take_mirror_writes(now + MIRROR_WRITE_DEBOUNCE, false);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, path);
        assert_eq!(writes[0].1.as_str(), "two");
        assert!(controller.pending_mirror_text(&path).is_none());
        assert!(
            controller.mirror_write_repaint(now).is_none(),
            "an empty queue has no repaint deadline"
        );
    }

    #[test]
    fn forced_mirror_flush_ignores_the_debounce() {
        let mut controller = CollabController::default();
        let now = Instant::now();
        let path = PathBuf::from("mirror/closed.typ");
        controller.queue_mirror_write(path.clone(), "latest".to_owned(), now);
        assert!(controller.mirror_write_repaint(now).is_some());
        let writes = controller.take_mirror_writes(now, true);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, path);
        assert_eq!(writes[0].1.as_str(), "latest");
    }

    #[test]
    fn session_reset_clears_cross_session_state() {
        let mut controller = CollabController::default();
        controller.set_open_files(vec!["a.typ".to_owned()]);
        controller.cursor_file = "a.typ".to_owned();
        controller.cursor_carets = vec![1];
        controller.history_can_undo = true;
        controller.reset_session_state();
        assert!(controller.last_open_files.is_empty());
        assert!(controller.cursor_file.is_empty());
        assert!(controller.cursor_carets.is_empty());
        assert!(!controller.history_can_undo);
    }

    #[test]
    fn image_rel_path_normalizes_the_directory() {
        assert_eq!(image_rel_path("image/", "x.png"), "image/x.png");
        assert_eq!(image_rel_path("/assets/", "x.png"), "assets/x.png");
        assert_eq!(image_rel_path("", "x.png"), "x.png");
    }
}
