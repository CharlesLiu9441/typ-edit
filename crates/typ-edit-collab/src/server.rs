//! TCP collaboration server (plan `§5.1`, `§5.3`–`§5.10`).
//!
//! ## Concurrency model
//!
//! A **listener** thread accepts sockets and spawns a **reader** and a **writer**
//! per connection. Readers forward whole frames; writers drain a bounded outbound
//! queue ([`MAX_QUEUE`] frames; overflow drops the connection). Neither touches
//! session state.
//!
//! Every piece of session state — authentication, the DocStore, the generation
//! frontier, undo/redo, the join snapshot and every broadcast — lives on a single
//! **core** thread driven by one `select!` over the accept, inbound and
//! host-command channels. That one serialization point is what makes the atomic
//! join (`§5.4`) and the R3 ordering (`§5.3`) correct: a join captures the
//! frontier, snapshots the DocStore and opens the connection's outbox in the same
//! step, with no interleaving. The host talks to the core through
//! [`CollabServer`]'s command channel and reads results on its event channel.

use crate::crypto::{
    KDF_ITERS, KEY_LEN, SessionCipher, SessionRole, derive_key, derive_session_key,
    random_challenge, random_salt, server_proof, verify_client_proof,
};
use crate::error::ServerError;
use crate::generation::{Queued, ReqId, ServerDecision, ServerState, Sid};
use crate::history::History;
use crate::ops::{self, ChangeSet};
use crate::protocol::{
    self, ClientHandshake, ClientMessage, CursorState, FileBinHeader, FileChunkHeader, FrameKind,
    GetWhat, HistoryOp, ImageUploadHeader, PROTOCOL_VER, RejectKind, ServerHandshake,
    ServerMessage, SyncFile, TreeEntry, UserInfo,
};
use crate::wire;
use crossbeam_channel::{
    Receiver, SendTimeoutError, Sender, TrySendError, bounded, select, unbounded,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io;
use std::io::Read as _;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use typ_edit_model::TextSnapshot;

/// Server-local participant id (the host's own edits).
pub const SERVER_SID: Sid = 0;

/// Messages per connection before the writer queue is considered overflowed.
const MAX_QUEUE: usize = 1024;

/// Pending `Save` requests waiting for their expected generation. The
/// generation is peer-supplied, so the queue has to stay bounded.
const MAX_PENDING_SAVES: usize = 64;

/// Frames buffered between a connection's reader and the core. A full queue
/// blocks that reader, so TCP backpressure reaches a flooding peer instead of
/// growing server memory.
const INBOUND_BOUND: usize = 4;

/// Connections accepted by the listener but not yet admitted by the core.
const ACCEPT_BOUND: usize = 64;

/// Server events buffered for the host; the core blocks if the host falls behind.
const EVENT_BOUND: usize = 1024;

/// Pending-join outbox limits. A client that joins but never sends `Ready`
/// must not grow server memory with broadcast frames.
const OUTBOX_MAX_FRAMES: usize = 1024;
const OUTBOX_MAX_BYTES: usize = 16 * 1024 * 1024;

/// How often the listener asks the core to sweep expired pending joins.
const TICK_INTERVAL: Duration = Duration::from_millis(250);

/// How long a raw TCP connection may stay unauthenticated before it is dropped.
/// This is independent of `idle_timeout` (which can be disabled) so a port scan
/// or a half-open client cannot hold a connection slot forever.
const UNAUTHENTICATED_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a pending-join snapshot frame may wait for the writer queue before
/// the connection is dropped. Live broadcasts still use `try_send`.
const SNAPSHOT_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the listener polls for the stop flag while non-blocking.
const LISTENER_POLL: Duration = Duration::from_millis(20);

/// How long shutdown waits for threads before detaching them.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

/// How often shutdown polls the thread handles.
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);

/// Largest file resolved as text (join snapshot and on-demand reads); larger
/// files are metadata-only.
const MAX_DOC_BYTES: u64 = 10 * 1024 * 1024;

/// Largest file served on demand as a binary frame.
const MAX_BIN_BYTES: u64 = 32 * 1024 * 1024;

/// One chunk of an on-demand chunked download. Small enough for frequent
/// progress updates, large enough to keep per-frame overhead low.
const DOWNLOAD_CHUNK: usize = 512 * 1024;

/// Configuration for a collaboration server.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Project root directory (the DocStore is initialized from it).
    pub root: PathBuf,
    /// Project display name (advertised in Config and mDNS).
    pub project: String,
    /// Main file relative path, if any.
    pub main_file: Option<String>,
    /// The server user's display name.
    pub server_user: String,
    /// TCP port to try first; the server probes this plus the next nine.
    pub port: u16,
    /// Directory where uploaded images are written (deduplicated).
    pub image_dir: PathBuf,
    /// Maximum concurrent connections; further joins are refused with
    /// `RejectKind::Full` (0 means unlimited).
    pub max_connections: usize,
    /// Drop a connection that sends nothing for this long (`None` disables the
    /// idle timeout). A live client pings every couple of seconds, so silence
    /// means the peer is gone or stalling; it also bounds slow-loris holds.
    pub idle_timeout: Option<Duration>,
    /// Drop a connection that has joined but never sent `Ready` within this
    /// window. Pending connections hold an outbox and are not live yet, so they
    /// must not be able to linger indefinitely.
    pub ready_timeout: Duration,
    /// Texts to seed the authoritative store with before the first connection is
    /// accepted. The app passes the host's dirty open buffers so a joiner never
    /// sees stale on-disk content; entries are root-relative and validated.
    pub initial_docs: Vec<(String, TextSnapshot)>,
}

/// Events the core publishes to the host.
#[derive(Clone, Debug)]
pub enum ServerEvent {
    /// A client finished joining and is now live.
    ClientJoined {
        /// The new session id.
        sid: Sid,
        /// The client's display name.
        user: String,
    },
    /// A client disconnected.
    ClientLeft {
        /// The departed session id.
        sid: Sid,
        /// The departed client's display name.
        user: String,
    },
    /// A change set was applied to the authoritative document.
    ChangeApplied {
        /// The change's origin (a client sid, or [`SERVER_SID`]).
        origin: Sid,
        /// The applied change.
        set: ChangeSet,
        /// Whether the server host already applied this change locally. Normal
        /// host edits are captured by the editor before they reach the server, so
        /// their echo must be skipped. A host undo/redo is *not* applied locally
        /// (the editor routes Ctrl+Z to the server), so it must be applied to the
        /// host's buffer from this event.
        already_applied: bool,
    },
    /// A file was written to disk.
    Saved {
        /// Relative path written.
        path: String,
        /// Whether the write succeeded.
        ok: bool,
    },
    /// An uploaded image was stored.
    ImageStored {
        /// The uploader's correlation id.
        upload_id: u64,
        /// The final deduplicated file name.
        name: String,
        /// The uploading session id.
        by: Sid,
    },
    /// The server-global undo/redo stack changed (for the host's menu).
    HistoryState {
        /// Whether an undo is available.
        can_undo: bool,
        /// Whether a redo is available.
        can_redo: bool,
    },
    /// A client's cursor changed (for the server host's remote-caret rendering).
    Cursors {
        /// The client's session id.
        sid: Sid,
        /// The client's display name.
        user: String,
        /// Relative file path.
        file: String,
        /// Byte carets.
        carets: Vec<u64>,
        /// Byte selections.
        selections: Vec<(u64, u64)>,
    },
    /// The server stopped and flushed.
    Stopped,
    /// The host's local change was rejected, or the host asked for a fresh copy:
    /// replace `path` with `text` (the authoritative content). `None` means the
    /// server has no readable text for that path.
    ResyncFile { path: String, text: Option<String> },
}

/// A running collaboration server.
pub struct CollabServer {
    port: u16,
    commands: Sender<CoreCommand>,
    events: Receiver<ServerEvent>,
    /// One-shot signal that the startup scan finished and the core loop is up.
    ready: Receiver<()>,
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    active: bool,
}

impl CollabServer {
    /// Bind a listener and start the server threads.
    pub fn start(config: ServerConfig, password: Option<String>) -> Result<Self, ServerError> {
        Self::start_inner(config, password, None)
    }

    /// Starts with a debug-only delay before the startup scan. Used to
    /// reproduce a server that is not ready yet when a client connects; the
    /// listener is already bound, so the client's handshake must wait.
    pub fn start_with_startup_delay(
        config: ServerConfig,
        password: Option<String>,
        delay: Duration,
    ) -> Result<Self, ServerError> {
        Self::start_inner(config, password, Some(delay))
    }

    fn start_inner(
        config: ServerConfig,
        password: Option<String>,
        startup_delay: Option<Duration>,
    ) -> Result<Self, ServerError> {
        let (listener, port) = bind(config.port)?;
        listener.set_nonblocking(true).map_err(ServerError::Io)?;

        let (accepted_tx, accepted_rx) = bounded::<Accepted>(ACCEPT_BOUND);
        let (inbound_tx, inbound_rx) = bounded::<Inbound>(INBOUND_BOUND);
        let (cmd_tx, cmd_rx) = unbounded::<CoreCommand>();
        let (event_tx, event_rx) = bounded::<ServerEvent>(EVENT_BOUND);
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));

        let core_accepted = accepted_tx.clone();
        let core_inbound = inbound_tx.clone();
        let core_handle = thread::spawn(move || {
            if let Some(delay) = startup_delay {
                thread::sleep(delay);
            }
            // The startup scan must run on this thread: on a tree with build
            // artifacts it reads far too much to sit on the caller's thread
            // (the app calls `start` from its UI thread).
            let core = Core::new(config, password, event_tx, core_inbound);
            // Tell the CLI (and tests) that the listener is about to process
            // connections, so they do not race the scan with a handshake.
            let _ = ready_tx.send(());
            core_loop(core, accepted_rx, inbound_rx, cmd_rx, core_accepted)
        });

        let stop_flag = stop.clone();
        let listener_handle =
            thread::spawn(move || listener_loop(listener, accepted_tx, inbound_tx, stop_flag));

        Ok(Self {
            port,
            commands: cmd_tx,
            events: event_rx,
            ready: ready_rx,
            stop,
            handles: vec![core_handle, listener_handle],
            active: true,
        })
    }

    /// Waits until the startup scan has finished. The one-shot signal is
    /// consumed; a second call returns immediately only if it is still queued.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        self.ready.recv_timeout(timeout).is_ok()
    }

    /// The actual bound TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Receive the next server event, waiting up to `timeout`.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<ServerEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Receive the next server event if one is already queued.
    pub fn try_recv_event(&self) -> Option<ServerEvent> {
        self.events.try_recv().ok()
    }

    /// Apply a server-local user edit (R1 step 4): validate, apply, broadcast and
    /// record it exactly like a remote change, but with no network round trip.
    pub fn submit_local_change(&self, set: ChangeSet) {
        let _ = self.commands.send(CoreCommand::SubmitLocalChange(set));
    }

    /// Broadcast the server host's own cursors to every client.
    pub fn submit_local_cursors(&self, file: &str, carets: Vec<u64>, selections: Vec<(u64, u64)>) {
        let _ = self.commands.send(CoreCommand::SubmitLocalCursors {
            file: file.to_owned(),
            carets,
            selections,
        });
    }

    /// Undo/redo on the server-global stacks for the server host's own Ctrl+Z/Y.
    pub fn request_local_history(&self, op: HistoryOp) {
        let _ = self.commands.send(CoreCommand::LocalHistory(op));
    }

    /// Publish a server-host image (drop/paste) exactly like a client upload.
    pub fn publish_local_image(&self, name: &str, bytes: Vec<u8>) {
        let _ = self.commands.send(CoreCommand::PublishLocalImage {
            name: name.to_owned(),
            bytes,
        });
    }

    /// Seed or replace the authoritative in-memory content of `path`.
    pub fn set_doc(&self, path: &str, text: &str) {
        let _ = self.commands.send(CoreCommand::SetDoc {
            path: path.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Query the authoritative `(generation, content)` for `path`.
    pub fn query_doc(&self, path: &str) -> Option<(u64, Option<String>)> {
        let (reply, response) = bounded(1);
        self.commands
            .send(CoreCommand::QueryDoc {
                path: path.to_owned(),
                reply,
            })
            .ok()?;
        response.recv_timeout(Duration::from_secs(5)).ok()
    }

    /// Broadcast `ServerClosed`, flush dirty files to disk, and join the threads
    /// under a deadline.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Ask the core to send the authoritative text of `path` back through
    /// [`ServerEvent::ResyncFile`]. Used when the host could not apply a remote
    /// change; it is asynchronous so the UI thread never waits on the core.
    pub fn request_doc(&self, path: &str) {
        let _ = self.commands.send(CoreCommand::RequestDoc {
            path: path.to_owned(),
        });
    }

    fn shutdown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.commands.send(CoreCommand::Stop);
        let deadline = Instant::now() + SHUTDOWN_DEADLINE;
        let mut handles = std::mem::take(&mut self.handles);
        loop {
            let mut index = 0;
            while index < handles.len() {
                if handles[index].is_finished() {
                    let finished = handles.swap_remove(index);
                    let _ = finished.join();
                } else {
                    index += 1;
                }
            }
            if handles.is_empty() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(SHUTDOWN_POLL);
        }
        for handle in handles {
            log::warn!("collab server thread still running at the shutdown deadline; detaching");
            drop(handle);
        }
    }
}

impl Drop for CollabServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Try `port`, then `port+1`..`port+9`, binding all interfaces. Port `0` asks the
/// OS for an ephemeral port, whose actual value is returned.
fn bind(start: u16) -> Result<(TcpListener, u16), ServerError> {
    for offset in 0..10u16 {
        let Some(port) = start.checked_add(offset) else {
            break;
        };
        if let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) {
            let actual = listener
                .local_addr()
                .map(|addr| addr.port())
                .unwrap_or(port);
            return Ok((listener, actual));
        }
    }
    Err(ServerError::NoPort { start })
}

// ---------------------------------------------------------------------------
// Thread plumbing
// ---------------------------------------------------------------------------

type ConnId = u64;
type CursorFrame = (String, Vec<u64>, Vec<(u64, u64)>);

struct Accepted {
    stream: TcpStream,
    peer: SocketAddr,
}

enum Inbound {
    Frame {
        conn_id: ConnId,
        frame: Vec<u8>,
    },
    Closed {
        conn_id: ConnId,
    },
    /// The reader saw no bytes for the configured idle timeout.
    Idle {
        conn_id: ConnId,
    },
    /// Periodic wake-up so the core can sweep expired pending joins.
    Tick,
}

enum CoreCommand {
    SubmitLocalChange(ChangeSet),
    SubmitLocalCursors {
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    },
    LocalHistory(HistoryOp),
    PublishLocalImage {
        name: String,
        bytes: Vec<u8>,
    },
    SetDoc {
        path: String,
        text: String,
    },
    QueryDoc {
        path: String,
        reply: Sender<(u64, Option<String>)>,
    },
    RequestDoc {
        path: String,
    },
    Stop,
}

struct PendingSave {
    sid: Sid,
    path: String,
    g_expected: u64,
}

fn listener_loop(
    listener: TcpListener,
    accepted_tx: Sender<Accepted>,
    inbound_tx: Sender<Inbound>,
    stop: Arc<AtomicBool>,
) {
    let mut last_tick = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                let accepted = Accepted { stream, peer };
                match accepted_tx.try_send(accepted) {
                    Ok(()) => {}
                    Err(TrySendError::Full(accepted)) => {
                        // The core is behind; refuse this connection instead of
                        // queueing sockets without bound (and without threads).
                        let _ = accepted.stream.shutdown(Shutdown::Both);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Err(ref error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(LISTENER_POLL);
            }
            Err(_) => thread::sleep(LISTENER_POLL),
        }
        if last_tick.elapsed() >= TICK_INTERVAL {
            // Best effort: a full inbound queue already has work for the core.
            let _ = inbound_tx.try_send(Inbound::Tick);
            last_tick = Instant::now();
        }
    }
}

fn reader_loop(
    mut stream: TcpStream,
    conn_id: ConnId,
    inbound_tx: Sender<Inbound>,
    idle_timeout: Option<Duration>,
) {
    if let Some(idle) = idle_timeout.filter(|idle| !idle.is_zero()) {
        // Every frame resets the timeout, and a live client pings every couple of
        // seconds, so a read that times out means the peer went silent.
        if let Err(error) = stream.set_read_timeout(Some(idle)) {
            log::warn!("cannot set the idle timeout for conn {conn_id}: {error}");
        }
    }
    loop {
        match wire::read_frame(&mut stream) {
            Ok(frame) => {
                if inbound_tx.send(Inbound::Frame { conn_id, frame }).is_err() {
                    break;
                }
            }
            Err(ref error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let _ = inbound_tx.send(Inbound::Idle { conn_id });
                break;
            }
            Err(_) => {
                let _ = inbound_tx.send(Inbound::Closed { conn_id });
                break;
            }
        }
    }
}

fn writer_loop(mut stream: TcpStream, queue: Receiver<Vec<u8>>) {
    while let Ok(frame) = queue.recv() {
        if wire::write_frame(&mut stream, &frame).is_err() {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

// ---------------------------------------------------------------------------
// Core (single serialization point)
// ---------------------------------------------------------------------------

struct Conn {
    stream: TcpStream,
    outbound: Sender<Vec<u8>>,
    peer: SocketAddr,
    cipher: Option<SessionCipher>,
    read_encrypted: bool,
    write_encrypted: bool,
    challenge: [u8; 16],
    sid: Option<Sid>,
    user: String,
    live: bool,
    snapshot_g: u64,
    /// When the socket was accepted, for the unauthenticated handshake timeout.
    accepted_at: Instant,
    /// When this connection joined and started waiting for `Ready`.
    pending_since: Option<Instant>,
    outbox: Outbox,
    cursor: Option<CursorFrame>,
}

/// Frames buffered while a joined connection is still waiting for `Ready`.
///
/// Bounded by both frame count and bytes: broadcasts must not be able to grow a
/// pending connection's memory without limit.
#[derive(Default)]
struct Outbox {
    frames: VecDeque<(u64, Vec<u8>)>,
    bytes: usize,
}

impl Outbox {
    /// Queues one frame; returns false when a limit would be exceeded.
    fn push(&mut self, g: u64, bytes: Vec<u8>) -> bool {
        let next_bytes = self.bytes.saturating_add(bytes.len());
        if self.frames.len() >= OUTBOX_MAX_FRAMES || next_bytes > OUTBOX_MAX_BYTES {
            return false;
        }
        self.bytes = next_bytes;
        self.frames.push_back((g, bytes));
        true
    }

    /// Removes every queued frame, returning them in order.
    fn take(&mut self) -> VecDeque<(u64, Vec<u8>)> {
        self.bytes = 0;
        std::mem::take(&mut self.frames)
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }
}

struct Core {
    config: ServerConfig,
    password: Option<String>,
    salt: Vec<u8>,
    /// Password-derived key, computed once per server. The KDF is expensive and
    /// its inputs are server-wide constants, so recomputing it per join only
    /// lets a reconnect flood burn the core thread.
    password_key: Option<[u8; KEY_LEN]>,
    /// Authoritative content of files touched by change sets; untouched files
    /// resolve from disk on demand (`doc_text_opt`).
    docs: BTreeMap<String, String>,
    sizes: BTreeMap<String, u64>,
    dirs: BTreeSet<String>,
    server_gen: ServerState,
    history: History,
    conns: BTreeMap<ConnId, Conn>,
    next_sid: Sid,
    next_msg: u64,
    next_conn: ConnId,
    /// Reader-side sender used to spawn readers for admitted connections.
    inbound: Sender<Inbound>,
    idle_timeout: Option<Duration>,
    /// Deadline for a socket that has not completed the `Join` handshake.
    handshake_timeout: Duration,
    pending_saves: Vec<PendingSave>,
    events: Sender<ServerEvent>,
}

impl Core {
    fn new(
        config: ServerConfig,
        password: Option<String>,
        events: Sender<ServerEvent>,
        inbound: Sender<Inbound>,
    ) -> Self {
        let (mut sizes, mut dirs) = scan_root(&config.root);
        let handshake_timeout = config.ready_timeout.min(UNAUTHENTICATED_TIMEOUT);
        // Seed the authoritative store before the core accepts anything, so the
        // first join already sees the host's unsaved open buffers.
        let mut docs = BTreeMap::new();
        for (path, snapshot) in &config.initial_docs {
            let text = snapshot.materialize();
            if text.len() as u64 > MAX_DOC_BYTES {
                log::warn!("skipping oversized initial doc {path}");
                continue;
            }
            if safe_join(&config.root, path).is_none() {
                log::warn!("skipping unsafe initial doc {path:?}");
                continue;
            }
            docs.insert(path.clone(), text.clone());
            sizes.insert(path.clone(), text.len() as u64);
            let mut parent = Path::new(path).parent();
            while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
                dirs.insert(relative_string(dir));
                parent = dir.parent();
            }
        }
        let idle_timeout = config.idle_timeout;
        // One salt per server instance. The per-connection key now comes from the
        // challenge, so an RNG failure here no longer risks nonce reuse — but it
        // still weakens the password stretch, so it must not pass silently.
        let salt = if password.is_some() {
            match random_salt() {
                Ok(salt) => salt.to_vec(),
                Err(err) => {
                    log::error!(
                        "cannot generate the session salt: {err}; continuing with an empty salt"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let password_key = password
            .as_ref()
            .map(|password| derive_key(password, &salt, KDF_ITERS));
        Self {
            config,
            password,
            salt,
            password_key,
            docs,
            sizes,
            dirs,
            server_gen: ServerState::new(),
            history: History::new(),
            conns: BTreeMap::new(),
            next_sid: 1,
            next_msg: 0,
            next_conn: 1,
            inbound,
            idle_timeout,
            handshake_timeout,
            pending_saves: Vec::new(),
            events,
        }
    }

    fn on_command(&mut self, command: CoreCommand) {
        match command {
            CoreCommand::SubmitLocalChange(set) => self.submit_local_change(set),
            CoreCommand::SubmitLocalCursors {
                file,
                carets,
                selections,
            } => self.submit_local_cursors(file, carets, selections),
            CoreCommand::LocalHistory(op) => self.handle_history(op),
            CoreCommand::PublishLocalImage { name, bytes } => {
                self.store_image(SERVER_SID, 0, &name, &bytes);
            }
            CoreCommand::SetDoc { path, text } => {
                self.docs.insert(path, text);
            }
            CoreCommand::QueryDoc { path, reply } => {
                let g = self.server_gen.generation();
                let text = self.doc_text_opt(&path);
                let _ = reply.send((g, text));
            }
            CoreCommand::RequestDoc { path } => {
                let text = self.doc_text_opt(&path);
                let _ = self.events.send(ServerEvent::ResyncFile { path, text });
            }
            CoreCommand::Stop => {}
        }
    }

    // --- connection lifecycle ------------------------------------------------

    fn on_accept(&mut self, accepted: Accepted) {
        if self.config.max_connections > 0 && self.conns.len() >= self.config.max_connections {
            log::warn!(
                "refusing {}: at the {} connection limit",
                accepted.peer,
                self.config.max_connections
            );
            // No reader/writer threads are spawned for a refused connection:
            // the listener handed us the socket, so write the rejection here
            // with a short deadline and close it.
            let mut stream = accepted.stream;
            let g = self.server_gen.generation();
            if let Ok(payload) = protocol::to_json_payload(&ServerHandshake::Rejected {
                g,
                error: RejectKind::Full,
            }) && let Ok(frame) = wire::frame_with_cipher(FrameKind::Json, &payload, None)
            {
                // A 100-byte rejection fits an empty socket buffer immediately;
                // the short deadline only bounds a peer that will not read.
                let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                let _ = wire::write_frame(&mut stream, &frame);
            }
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        let challenge = match random_challenge() {
            Ok(challenge) => challenge,
            Err(_) => {
                log::warn!("rng unavailable; refusing connection");
                let _ = accepted.stream.shutdown(Shutdown::Both);
                return;
            }
        };
        let conn_id = self.next_conn;
        self.next_conn += 1;
        let (outbound, outbound_rx) = bounded::<Vec<u8>>(MAX_QUEUE);
        let (Ok(read_half), Ok(write_half)) =
            (accepted.stream.try_clone(), accepted.stream.try_clone())
        else {
            log::warn!("cannot clone the socket for {}", accepted.peer);
            let _ = accepted.stream.shutdown(Shutdown::Both);
            return;
        };
        let reader_inbound = self.inbound.clone();
        let idle_timeout = self.idle_timeout;
        thread::spawn(move || reader_loop(read_half, conn_id, reader_inbound, idle_timeout));
        thread::spawn(move || writer_loop(write_half, outbound_rx));
        let conn = Conn {
            stream: accepted.stream,
            outbound,
            peer: accepted.peer,
            cipher: None,
            read_encrypted: false,
            write_encrypted: false,
            challenge,
            sid: None,
            user: String::new(),
            live: false,
            snapshot_g: 0,
            accepted_at: Instant::now(),
            pending_since: None,
            outbox: Outbox::default(),
            cursor: None,
        };
        self.conns.insert(conn_id, conn);
        let hello = ServerHandshake::Hello {
            ver: PROTOCOL_VER,
            project: self.config.project.clone(),
            server_user: self.config.server_user.clone(),
            needs_password: self.password.is_some(),
            salt: self.password.as_ref().map(|_| self.salt.clone()),
            kdf_iters: self.password.as_ref().map(|_| KDF_ITERS),
            challenge: Some(challenge.to_vec()),
            user_count: self.live_users().len() as u32 + 1,
        };
        self.send_json_direct(conn_id, &hello);
    }

    fn on_inbound(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Frame { conn_id, frame } => self.on_frame(conn_id, &frame),
            Inbound::Closed { conn_id } => self.drop_conn(conn_id, "connection closed"),
            Inbound::Idle { conn_id } => self.drop_conn(conn_id, "idle timeout"),
            Inbound::Tick => self.sweep_pending(),
        }
    }

    /// Drops unauthenticated sockets and joined connections that never sent
    /// `Ready` within their deadlines.
    fn sweep_pending(&mut self) {
        let now = Instant::now();
        let expired: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, conn)| {
                if conn.sid.is_none() {
                    return pending_expired(Some(conn.accepted_at), now, self.handshake_timeout);
                }
                !conn.live && pending_expired(conn.pending_since, now, self.config.ready_timeout)
            })
            .map(|(id, _)| *id)
            .collect();
        for conn_id in expired {
            self.drop_conn(conn_id, "handshake or ready timeout");
        }
    }

    fn on_frame(&mut self, conn_id: ConnId, frame: &[u8]) {
        let parsed = {
            let Some(conn) = self.conns.get_mut(&conn_id) else {
                return;
            };
            let cipher = if conn.read_encrypted {
                conn.cipher.as_mut()
            } else {
                None
            };
            wire::parse_inbound(frame, cipher)
        };
        let (kind, payload) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                log::warn!("bad frame from conn {conn_id}: {error}");
                self.drop_conn(conn_id, "bad frame");
                return;
            }
        };
        let joined = self
            .conns
            .get(&conn_id)
            .is_some_and(|conn| conn.sid.is_some());
        if !joined {
            if kind != FrameKind::Json {
                self.drop_conn(conn_id, "non-JSON before join");
                return;
            }
            match protocol::from_json_payload::<ClientHandshake>(&payload) {
                Ok(ClientHandshake::Join { ver, user, hmac }) => {
                    self.on_join(conn_id, ver, user, hmac);
                }
                Ok(ClientHandshake::Ready) => {}
                Err(_) => self.drop_conn(conn_id, "bad handshake"),
            }
            return;
        }
        let live = self.conns.get(&conn_id).is_some_and(|conn| conn.live);
        if !live {
            // Before `Ready` the client has not applied the snapshot. Accept the
            // Ready handshake and nothing else: other messages would queue work
            // for a peer that may never become live.
            if kind == FrameKind::Json
                && matches!(
                    protocol::from_json_payload::<ClientHandshake>(&payload),
                    Ok(ClientHandshake::Ready)
                )
            {
                self.on_ready(conn_id);
            } else {
                self.drop_conn(conn_id, "message before ready");
            }
            return;
        }
        if kind == FrameKind::Binary {
            match protocol::decode_binary_payload::<ImageUploadHeader>(&payload) {
                Ok((header, bytes)) => self.on_image_upload(conn_id, header, bytes),
                Err(_) => self.drop_conn(conn_id, "bad binary frame"),
            }
            return;
        }
        match protocol::from_json_payload::<ClientMessage>(&payload) {
            Ok(message) => self.on_message(conn_id, message),
            Err(_) => match protocol::from_json_payload::<ClientHandshake>(&payload) {
                Ok(ClientHandshake::Ready) => self.on_ready(conn_id),
                _ => self.drop_conn(conn_id, "bad client message"),
            },
        }
    }

    fn on_join(&mut self, conn_id: ConnId, ver: u32, user: String, hmac: Option<Vec<u8>>) {
        if ver != PROTOCOL_VER {
            self.reject(conn_id, RejectKind::Version);
            return;
        }
        let server_mac = if let Some(password_key) = self.password_key {
            let challenge = match self.conns.get(&conn_id) {
                Some(conn) => conn.challenge,
                None => return,
            };
            let verified = hmac
                .as_deref()
                .is_some_and(|proof| verify_client_proof(&password_key, &challenge, &user, proof));
            if !verified {
                log::info!("authentication failed for {user}");
                self.reject(conn_id, RejectKind::Auth);
                return;
            }
            // The AEAD key is per connection: the password key is shared by every
            // connection and each cipher restarts its nonce counter at zero, so
            // sealing with it would reuse `(key, nonce)` pairs across sockets
            // (`derive_session_key`). Both sides derive it from the same challenge.
            let session_key = derive_session_key(&password_key, &challenge);
            if let Some(conn) = self.conns.get_mut(&conn_id) {
                conn.cipher = Some(SessionCipher::new(&session_key, SessionRole::Server));
                conn.read_encrypted = true;
                conn.write_encrypted = true;
            }
            server_proof(&password_key, &challenge, &user)
                .ok()
                .map(|proof| proof.to_vec())
        } else {
            None
        };
        self.atomic_join(conn_id, user, server_mac);
    }

    fn reject(&mut self, conn_id: ConnId, error: RejectKind) {
        let g = self.server_gen.generation();
        self.send_json_direct(conn_id, &ServerHandshake::Rejected { g, error });
        // The writer owns flushing `Rejected`; dropping the entry closes its
        // queue so the writer drains the frame and closes the socket itself.
        self.conns.remove(&conn_id);
    }

    fn atomic_join(&mut self, conn_id: ConnId, user: String, server_mac: Option<Vec<u8>>) {
        let sid = self.next_sid;
        self.next_sid += 1;
        let g = self.server_gen.generation();

        // Capture the frontier and the path list, then stream one file at a
        // time. The core still owns the snapshot (so it stays atomic at `g`),
        // but we no longer deep-copy every file's text before sending it.
        let users = self.live_users();
        let cursors = self.current_cursors();
        let paths: Vec<String> = self.sizes.keys().cloned().collect();
        let tree = self.tree_message(g);
        let config = ServerMessage::Config {
            g,
            name: self.config.project.clone(),
            main_file: self.config.main_file.clone(),
            server_user: self.config.server_user.clone(),
            image_dir: Some(self.image_dir_rel()),
        };

        // Register as pending and open the outbox in the same step.
        let Some(conn) = self.conns.get_mut(&conn_id) else {
            return;
        };
        conn.sid = Some(sid);
        conn.user = user.clone();
        conn.live = false;
        conn.snapshot_g = g;
        // The snapshot transfer has its own 10 s per-frame send deadline; the
        // client's `Ready` budget starts only after `SnapshotDone` (below).
        conn.pending_since = None;
        conn.outbox.clear();
        // The core is about to stream the whole snapshot and cannot answer
        // pings; tell live clients before any potentially blocking send. The
        // joining client is excluded (its app pauses until `Live`).
        self.broadcast(g, &ServerMessage::Busy { g, busy: true }, Some(sid));

        let joined = ServerHandshake::Joined {
            sid,
            generation: g,
            project: self.config.project.clone(),
            server_user: self.config.server_user.clone(),
            users,
            cursors,
            server_mac,
        };
        let history = ServerMessage::HistoryState {
            g,
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
        };
        if !self.send_json_direct(conn_id, &joined)
            || !self.send_json_direct(conn_id, &history)
            || !self.send_json_direct(conn_id, &config)
            || !self.send_json_direct(conn_id, &tree)
        {
            self.fail_join_snapshot(conn_id, sid, g);
            return;
        }
        for path in paths {
            let Some(text) = self.doc_text_opt(&path) else {
                self.send_file(conn_id, g, &path);
                continue;
            };
            if !self.send_json_direct(conn_id, &ServerMessage::FileText { g, path, text }) {
                self.fail_join_snapshot(conn_id, sid, g);
                return;
            }
        }
        if !self.send_json_direct(conn_id, &ServerMessage::SnapshotDone { g }) {
            self.fail_join_snapshot(conn_id, sid, g);
            return;
        }
        self.broadcast(g, &ServerMessage::Busy { g, busy: false }, Some(sid));
        // The snapshot transfer is not part of the client's `Ready` budget: the
        // deadline starts once `SnapshotDone` is queued. Failure paths above
        // drop the connection explicitly instead of waiting for a sweep.
        if let Some(conn) = self.conns.get_mut(&conn_id) {
            conn.pending_since = Some(Instant::now());
        }
        log::info!("client {sid} ({user}) joined at g={g}");
    }

    /// A failed snapshot send must not leave live peers paused forever.
    fn fail_join_snapshot(&mut self, conn_id: ConnId, sid: u64, g: u64) {
        self.broadcast(g, &ServerMessage::Busy { g, busy: false }, Some(sid));
        self.drop_conn(conn_id, "join snapshot failed");
    }

    fn on_ready(&mut self, conn_id: ConnId) {
        let drain = {
            let Some(conn) = self.conns.get_mut(&conn_id) else {
                return;
            };
            if conn.sid.is_none() || conn.live {
                return;
            }
            conn.live = true;
            conn.pending_since = None;
            conn.outbox.take()
        };
        let snapshot_g = self.conns.get(&conn_id).map_or(0, |conn| conn.snapshot_g);
        for (g, bytes) in drain {
            if g < snapshot_g {
                log::warn!("dropping outbox frame g={g} below snapshot g={snapshot_g}");
                continue;
            }
            let sent = self
                .conns
                .get(&conn_id)
                .is_some_and(|conn| conn.outbound.try_send(bytes).is_ok());
            if !sent {
                // Frames queued while this client was pending cannot be recovered
                // by the client (it never saw them), so a full queue here costs the
                // connection instead of leaving it silently incomplete.
                log::warn!("cannot flush the pending outbox for conn {conn_id}; dropping it");
                self.drop_conn(conn_id, "pending outbox flush failed");
                return;
            }
        }
        if let Some(conn) = self.conns.get(&conn_id) {
            self.events
                .send(ServerEvent::ClientJoined {
                    sid: conn.sid.unwrap_or(SERVER_SID),
                    user: conn.user.clone(),
                })
                .ok();
        }
        self.broadcast_users();
    }

    fn drop_conn(&mut self, conn_id: ConnId, reason: &str) {
        let Some(conn) = self.conns.remove(&conn_id) else {
            return;
        };
        let _ = conn.stream.shutdown(Shutdown::Both);
        if let Some(sid) = conn.sid {
            // A save this client asked for can never be answered usefully now.
            self.pending_saves.retain(|save| save.sid != sid);
            log::info!(
                "client {sid} ({}) at {} left: {reason}",
                conn.user,
                conn.peer
            );
            // A pending connection never appeared in the host's user list, so
            // only a live departure is announced.
            if conn.live {
                self.events
                    .send(ServerEvent::ClientLeft {
                        sid,
                        user: conn.user.clone(),
                    })
                    .ok();
                self.broadcast_users();
            }
        }
    }

    // --- client messages -----------------------------------------------------

    fn on_message(&mut self, conn_id: ConnId, message: ClientMessage) {
        let Some(sid) = self.conns.get(&conn_id).and_then(|conn| conn.sid) else {
            return;
        };
        let live = self.conns.get(&conn_id).is_some_and(|conn| conn.live);
        match message {
            ClientMessage::Changes { g, msg, file, ops } => {
                if !live {
                    log::warn!("F12: change from pending client {sid} ignored");
                    return;
                }
                self.handle_change(sid, g, msg, ChangeSet::new(file, ops));
            }
            ClientMessage::History { op, .. } => {
                if !live {
                    log::warn!("F12: history from pending client {sid} ignored");
                    return;
                }
                self.handle_history(op);
            }
            ClientMessage::Cursors {
                file,
                carets,
                selections,
                ..
            } => self.on_cursors(conn_id, sid, file, carets, selections),
            ClientMessage::Get { what, .. } => self.handle_get(conn_id, what),
            ClientMessage::Save {
                g_expected, path, ..
            } => self.handle_save(conn_id, sid, path, g_expected),
            ClientMessage::Sync { req, files, .. } => self.handle_sync(conn_id, req, files),
            ClientMessage::PeerStale { about, .. } => self.route_you_are_stale(about),
            ClientMessage::Ping { id, .. } => {
                let g = self.server_gen.generation();
                self.send_json_direct(conn_id, &ServerMessage::Pong { g, id });
            }
            ClientMessage::Bye { .. } => self.drop_conn(conn_id, "client bye"),
            ClientMessage::ImageUpload { .. } => {}
        }
    }

    fn handle_change(&mut self, sid: Sid, g: u64, msg: u64, set: ChangeSet) {
        match self.server_gen.receive(g, true) {
            ServerDecision::Ignored => {}
            ServerDecision::Desync => {
                let g = self.server_gen.generation();
                self.send_to_sid(sid, &ServerMessage::Desync { g });
            }
            ServerDecision::Buffered => {
                self.server_gen.buffer(
                    g,
                    Queued {
                        origin: sid,
                        msg,
                        set,
                    },
                );
            }
            ServerDecision::Overflow => {
                // The sender named more future generations than the buffer holds:
                // buffering cannot converge it, and the buffer must stay bounded.
                log::warn!("dropping {sid}: out-of-order buffer overflow");
                if let Some(conn_id) = self.conn_by_sid(sid) {
                    self.drop_conn(conn_id, "out-of-order buffer overflow");
                }
            }
            ServerDecision::Apply => {
                if !self.commit_change(sid, msg, set, true, false) {
                    let g = self.server_gen.generation();
                    self.send_to_sid(sid, &ServerMessage::Desync { g });
                    return;
                }
                self.step_and_drain();
            }
        }
    }

    fn submit_local_change(&mut self, set: ChangeSet) {
        let file = set.file.clone();
        let msg = self.next_msg;
        self.next_msg += 1;
        if self.commit_change(SERVER_SID, msg, set, true, true) {
            self.step_and_drain();
        } else {
            // The host has no `Desync` channel, so hand it the authoritative
            // text as an explicit resync request instead of diverging silently.
            let text = self.doc_text_opt(&file);
            self.events
                .send(ServerEvent::ResyncFile { path: file, text })
                .ok();
        }
    }

    /// Validate then apply one change set at the frontier, broadcast it (to every
    /// peer including the origin) and update history. Returns false if the set
    /// failed atomic validation (the caller decides on `Desync`).
    fn commit_change(
        &mut self,
        origin: Sid,
        msg: u64,
        set: ChangeSet,
        record: bool,
        already_applied: bool,
    ) -> bool {
        if safe_join(&self.config.root, &set.file).is_none() {
            log::warn!("change to unsafe path {:?} rejected", set.file);
            return false;
        }
        let g = self.server_gen.generation();
        let content = self.doc_text(&set.file);
        if let Err(reason) = ops::validate(&content, &set.ops) {
            log::warn!("change to {} rejected: {reason}", set.file);
            return false;
        }
        let next = match ops::apply(&content, &set.ops) {
            Ok(next) => next,
            Err(reason) => {
                log::warn!("change to {} failed to apply: {reason}", set.file);
                return false;
            }
        };
        self.docs.insert(set.file.clone(), next);
        let out = ServerMessage::Changes {
            g,
            origin,
            msg,
            file: set.file.clone(),
            ops: set.ops.clone(),
        };
        self.broadcast(g, &out, None);
        self.events
            .send(ServerEvent::ChangeApplied {
                origin,
                set: set.clone(),
                already_applied,
            })
            .ok();
        if record {
            self.history.record(set);
            // HistoryState describes the stacks, so it must be sent after the
            // record; a history operation updates them in `handle_history` and
            // broadcasts there instead.
            self.broadcast_history_state();
        }
        true
    }

    /// Commits the run of buffered change sets sitting on top of the frontier.
    ///
    /// The caller has just committed the set at the frontier, so the frontier
    /// steps first; from then on it only advances over a set that applied. A
    /// buffered set that fails validation is dropped and its origin is told to
    /// re-sync: advancing over it would leave every other peer waiting for a
    /// generation that never arrives.
    fn step_and_drain(&mut self) {
        self.server_gen.advance();
        while let Some(queued) = self.server_gen.next_queued() {
            if self.commit_change(queued.origin, queued.msg, queued.set, true, false) {
                self.server_gen.advance();
            } else {
                self.server_gen.discard_frontier();
                let g = self.server_gen.generation();
                self.send_to_sid(queued.origin, &ServerMessage::Desync { g });
            }
        }
        self.flush_pending_saves();
    }

    fn handle_history(&mut self, op: HistoryOp) {
        // Peek first: the stacks only move once the set actually applied, so a
        // failed undo cannot leave `redo` describing a change that is still in
        // the content (found by the audit as L1).
        let (set, is_undo) = match op {
            HistoryOp::Undo => match self.history.peek_undo() {
                Some((inverse, _original)) => (inverse, true),
                None => {
                    self.broadcast_history_state();
                    return;
                }
            },
            HistoryOp::Redo => match self.history.peek_redo() {
                Some(set) => (set, false),
                None => {
                    self.broadcast_history_state();
                    return;
                }
            },
        };
        let msg = self.next_msg;
        self.next_msg += 1;
        let file = set.file.clone();
        if !self.commit_change(SERVER_SID, msg, set, false, false) {
            log::warn!("history change failed to apply");
            let text = self.doc_text_opt(&file);
            self.events
                .send(ServerEvent::ResyncFile { path: file, text })
                .ok();
            self.broadcast_history_state();
            return;
        }
        if is_undo {
            self.history.commit_undo();
        } else {
            self.history.commit_redo();
        }
        // The stacks moved only now; broadcast the post-operation availability.
        self.broadcast_history_state();
        self.step_and_drain();
    }

    fn broadcast_history_state(&mut self) {
        let g = self.server_gen.generation();
        let can_undo = self.history.can_undo();
        let can_redo = self.history.can_redo();
        let state = ServerMessage::HistoryState {
            g,
            can_undo,
            can_redo,
        };
        self.broadcast(g, &state, None);
        self.events
            .send(ServerEvent::HistoryState { can_undo, can_redo })
            .ok();
    }

    fn on_cursors(
        &mut self,
        conn_id: ConnId,
        sid: Sid,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    ) {
        let content = self.doc_text(&file);
        let carets: Vec<u64> = carets
            .into_iter()
            .map(|offset| clamp_offset(&content, offset))
            .collect();
        let selections: Vec<(u64, u64)> = selections
            .into_iter()
            .map(|(start, end)| (clamp_offset(&content, start), clamp_offset(&content, end)))
            .collect();
        let Some(user) = self.conns.get(&conn_id).map(|conn| conn.user.clone()) else {
            return;
        };
        if let Some(conn) = self.conns.get_mut(&conn_id) {
            conn.cursor = Some((file.clone(), carets.clone(), selections.clone()));
        }
        let g = self.server_gen.generation();
        let msg = ServerMessage::Cursors {
            g,
            sid,
            user,
            file,
            carets,
            selections,
        };
        self.broadcast(g, &msg, Some(sid));
        if let ServerMessage::Cursors {
            user,
            file,
            carets,
            selections,
            ..
        } = &msg
        {
            self.events
                .send(ServerEvent::Cursors {
                    sid,
                    user: user.clone(),
                    file: file.clone(),
                    carets: carets.clone(),
                    selections: selections.clone(),
                })
                .ok();
        }
    }

    fn handle_get(&mut self, conn_id: ConnId, what: GetWhat) {
        let g = self.server_gen.generation();
        match what {
            GetWhat::Config => {
                let msg = ServerMessage::Config {
                    g,
                    name: self.config.project.clone(),
                    main_file: self.config.main_file.clone(),
                    server_user: self.config.server_user.clone(),
                    image_dir: Some(self.image_dir_rel()),
                };
                self.send_json_direct(conn_id, &msg);
            }
            GetWhat::Tree => {
                let msg = self.tree_message(g);
                self.send_json_direct(conn_id, &msg);
            }
            GetWhat::Cursors => {
                for state in self.current_cursors() {
                    let user = self
                        .conns
                        .values()
                        .find(|conn| conn.sid == Some(state.sid))
                        .map(|conn| conn.user.clone())
                        .unwrap_or_default();
                    let msg = ServerMessage::Cursors {
                        g,
                        sid: state.sid,
                        user,
                        file: state.file,
                        carets: state.carets,
                        selections: state.selections,
                    };
                    self.send_json_direct(conn_id, &msg);
                }
            }
            GetWhat::File { path } => self.send_file(conn_id, g, &path),
            GetWhat::ChunkedFile { path, allow_large } => {
                self.send_chunked_file(conn_id, g, &path, allow_large);
            }
        }
    }

    fn send_file(&mut self, conn_id: ConnId, g: u64, path: &str) {
        if let Some(text) = self.doc_text_opt(path) {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileText {
                    g,
                    path: path.to_owned(),
                    text,
                },
            );
            return;
        }
        let Some(full) = safe_join(&self.config.root, path) else {
            return;
        };
        let Ok(metadata) = std::fs::metadata(&full) else {
            return;
        };
        if !metadata.is_file() || metadata.len() > MAX_BIN_BYTES {
            log::warn!("refusing oversized file {path} ({} bytes)", metadata.len());
            return;
        }
        let Ok(bytes) = std::fs::read(&full) else {
            return;
        };
        let header = FileBinHeader {
            g,
            path: path.to_owned(),
        };
        match protocol::encode_binary_payload(&header, &bytes) {
            Ok(payload) => {
                self.send_direct(conn_id, FrameKind::Binary, &payload);
            }
            Err(error) => log::warn!("file bin encode failed: {error}"),
        }
    }

    fn send_chunked_file(&mut self, conn_id: ConnId, g: u64, path: &str, allow_large: bool) {
        // The read itself can be slow (a large text file is read before we know
        // whether it is text), so pause peers before touching the filesystem.
        self.broadcast(g, &ServerMessage::Busy { g, busy: true }, None);
        if let Some(text) = self.doc_text_opt(path) {
            self.send_chunked_bytes(conn_id, g, path, text.as_bytes());
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        }
        let Some(full) = safe_join(&self.config.root, path) else {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileUnavailable {
                    g,
                    path: path.to_owned(),
                },
            );
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        };
        let Ok(metadata) = std::fs::metadata(&full) else {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileUnavailable {
                    g,
                    path: path.to_owned(),
                },
            );
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        };
        if !metadata.is_file() {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileUnavailable {
                    g,
                    path: path.to_owned(),
                },
            );
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        }
        let total = metadata.len();
        if !allow_large && total > MAX_BIN_BYTES {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileTooLarge {
                    g,
                    path: path.to_owned(),
                    size: total,
                },
            );
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        }
        let Ok(mut file) = File::open(&full) else {
            self.send_json_direct(
                conn_id,
                &ServerMessage::FileUnavailable {
                    g,
                    path: path.to_owned(),
                },
            );
            self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
            return;
        };

        if total == 0 {
            self.send_chunk(conn_id, g, path, 0, 0, &[]);
        } else {
            let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
            let mut offset = 0u64;
            loop {
                match file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if !self.send_chunk(conn_id, g, path, offset, total, &buffer[..read]) {
                            break;
                        }
                        offset += read as u64;
                    }
                    Err(error) => {
                        log::warn!("cannot read {path} for chunked download: {error}");
                        self.send_json_direct(
                            conn_id,
                            &ServerMessage::FileUnavailable {
                                g,
                                path: path.to_owned(),
                            },
                        );
                        break;
                    }
                }
            }
        }
        self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
    }

    fn send_chunked_bytes(&mut self, conn_id: ConnId, g: u64, path: &str, bytes: &[u8]) {
        let total = bytes.len() as u64;
        if bytes.is_empty() {
            self.send_chunk(conn_id, g, path, 0, 0, &[]);
            return;
        }
        for (index, chunk) in bytes.chunks(DOWNLOAD_CHUNK).enumerate() {
            let offset = index as u64 * DOWNLOAD_CHUNK as u64;
            if !self.send_chunk(conn_id, g, path, offset, total, chunk) {
                break;
            }
        }
    }

    fn send_chunk(
        &mut self,
        conn_id: ConnId,
        g: u64,
        path: &str,
        offset: u64,
        total: u64,
        bytes: &[u8],
    ) -> bool {
        let header = FileChunkHeader {
            g,
            path: path.to_owned(),
            offset,
            total,
        };
        match protocol::encode_binary_payload(&header, bytes) {
            Ok(payload) => self.send_chunk_direct(conn_id, &payload),
            Err(error) => {
                log::warn!("file chunk encode failed: {error}");
                false
            }
        }
    }

    fn send_chunk_direct(&mut self, conn_id: ConnId, payload: &[u8]) -> bool {
        let Some((bytes, outbound)) = ({
            let Some(conn) = self.conns.get_mut(&conn_id) else {
                return false;
            };
            let cipher = if conn.write_encrypted {
                conn.cipher.as_mut()
            } else {
                None
            };
            match wire::frame_with_cipher(FrameKind::Binary, payload, cipher) {
                Ok(bytes) => Some((bytes, conn.outbound.clone())),
                Err(_) => None,
            }
        }) else {
            log::warn!("chunk encode failed for conn {conn_id}");
            return false;
        };
        match outbound.send_timeout(bytes, SNAPSHOT_SEND_TIMEOUT) {
            Ok(()) => true,
            Err(SendTimeoutError::Timeout(_)) | Err(SendTimeoutError::Disconnected(_)) => {
                log::warn!("chunk send to conn {conn_id} failed");
                self.drop_conn(conn_id, "chunk send failed");
                false
            }
        }
    }

    fn handle_sync(&mut self, conn_id: ConnId, req: ReqId, files: Vec<String>) {
        let g = self.server_gen.generation();
        // Building and serializing a large resync snapshot blocks the core just
        // like a join snapshot. Pause every live heartbeat (the requester
        // included) around the work so neither side mistakes it for a dead link.
        self.broadcast(g, &ServerMessage::Busy { g, busy: true }, None);
        let out: Vec<SyncFile> = files
            .into_iter()
            .filter_map(|path| self.doc_text_opt(&path).map(|text| SyncFile { path, text }))
            .collect();
        self.send_json_direct(conn_id, &ServerMessage::SyncState { g, req, files: out });
        self.broadcast(g, &ServerMessage::Busy { g, busy: false }, None);
    }

    fn handle_save(&mut self, conn_id: ConnId, sid: Sid, path: String, g_expected: u64) {
        if self.server_gen.generation() >= g_expected {
            let ok = self.do_save(&path);
            let g = self.server_gen.generation();
            self.send_json_direct(
                conn_id,
                &ServerMessage::Saved {
                    g,
                    path: path.clone(),
                    ok,
                },
            );
            self.events.send(ServerEvent::Saved { path, ok }).ok();
        } else if self.pending_saves.len() < MAX_PENDING_SAVES {
            self.pending_saves.push(PendingSave {
                sid,
                path,
                g_expected,
            });
        } else {
            // The expected generation is peer-supplied, so the queue stays bounded:
            // answer the save as failed rather than holding it forever.
            log::warn!("pending-save queue full; refusing {path} from sid {sid}");
            let g = self.server_gen.generation();
            self.send_json_direct(
                conn_id,
                &ServerMessage::Saved {
                    g,
                    path: path.clone(),
                    ok: false,
                },
            );
        }
    }

    fn flush_pending_saves(&mut self) {
        let current = self.server_gen.generation();
        let mut waiting = Vec::new();
        for save in std::mem::take(&mut self.pending_saves) {
            if current >= save.g_expected {
                let ok = self.do_save(&save.path);
                let g = self.server_gen.generation();
                self.send_to_sid(
                    save.sid,
                    &ServerMessage::Saved {
                        g,
                        path: save.path.clone(),
                        ok,
                    },
                );
                self.events
                    .send(ServerEvent::Saved {
                        path: save.path,
                        ok,
                    })
                    .ok();
            } else {
                waiting.push(save);
            }
        }
        self.pending_saves = waiting;
    }

    fn do_save(&mut self, path: &str) -> bool {
        // A missing, binary or oversized path has no authoritative text. Writing
        // an empty string here would silently truncate the user's file.
        let Some(text) = self.doc_text_opt(path) else {
            log::warn!("refusing to save {path}: no readable authoritative text");
            return false;
        };
        let Some(full) = safe_join(&self.config.root, path) else {
            log::warn!("refusing unsafe save path {path}");
            return false;
        };
        match write_atomic(&full, text.as_bytes()) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("save {path} failed: {error}");
                false
            }
        }
    }

    fn on_image_upload(&mut self, conn_id: ConnId, header: ImageUploadHeader, bytes: Vec<u8>) {
        let Some(sid) = self.conns.get(&conn_id).and_then(|conn| conn.sid) else {
            return;
        };
        self.store_image(sid, header.upload_id, &header.name, &bytes);
    }

    /// Deduplicate, write and broadcast an image on behalf of `by`.
    fn store_image(&mut self, by: Sid, upload_id: u64, name: &str, bytes: &[u8]) {
        if bytes.len() as u64 > MAX_BIN_BYTES {
            log::warn!("refusing oversized image {name} ({} bytes)", bytes.len());
            return;
        }
        let name = unique_image_name(&self.config.image_dir, name);
        let full = self.config.image_dir.join(&name);
        if let Err(error) = write_atomic(&full, bytes) {
            log::warn!("image write failed: {error}");
            return;
        }
        // The tree is root-relative: an image stored under `image/` must be
        // listed as `image/name`, not just `name`.
        let dir = self.image_dir_rel();
        let rel = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        self.sizes.insert(rel, bytes.len() as u64);
        let g = self.server_gen.generation();
        self.broadcast(
            g,
            &ServerMessage::ImageAdded {
                g,
                upload_id,
                name: name.clone(),
                by,
            },
            None,
        );
        self.events
            .send(ServerEvent::ImageStored {
                upload_id,
                name,
                by,
            })
            .ok();
    }

    /// Broadcast the server host's own cursors (the host is a participant).
    fn submit_local_cursors(
        &mut self,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    ) {
        let content = self.doc_text(&file);
        let carets: Vec<u64> = carets
            .into_iter()
            .map(|offset| clamp_offset(&content, offset))
            .collect();
        let selections: Vec<(u64, u64)> = selections
            .into_iter()
            .map(|(start, end)| (clamp_offset(&content, start), clamp_offset(&content, end)))
            .collect();
        let g = self.server_gen.generation();
        let msg = ServerMessage::Cursors {
            g,
            sid: SERVER_SID,
            user: self.config.server_user.clone(),
            file,
            carets,
            selections,
        };
        self.broadcast(g, &msg, None);
    }

    fn route_you_are_stale(&mut self, about: Sid) {
        if let Some(conn_id) = self.conn_by_sid(about) {
            let g = self.server_gen.generation();
            self.send_json_direct(conn_id, &ServerMessage::YouAreStale { g });
        } else {
            log::warn!("PeerStale for unknown sid {about}");
        }
    }

    // --- outbound helpers ----------------------------------------------------

    fn send_json_direct<T: Serialize>(&mut self, conn_id: ConnId, message: &T) -> bool {
        match protocol::to_json_payload(message) {
            Ok(payload) => self.send_direct(conn_id, FrameKind::Json, &payload),
            Err(error) => {
                log::warn!("serialize failed: {error}");
                false
            }
        }
    }

    fn send_direct(&mut self, conn_id: ConnId, kind: FrameKind, payload: &[u8]) -> bool {
        let Some((bytes, live, outbound)) = ({
            let Some(conn) = self.conns.get_mut(&conn_id) else {
                return false;
            };
            let cipher = if conn.write_encrypted {
                conn.cipher.as_mut()
            } else {
                None
            };
            match wire::frame_with_cipher(kind, payload, cipher) {
                Ok(bytes) => Some((bytes, conn.live, conn.outbound.clone())),
                Err(_) => None,
            }
        }) else {
            log::warn!("encode failed for conn {conn_id}");
            return false;
        };
        if live {
            match outbound.try_send(bytes) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    // Silently dropping a frame the peer needs (a `Changes` or a
                    // `Desync`) leaves it diverged with nothing to notice, so a
                    // wedged writer costs the connection.
                    log::warn!("outbound queue full for conn {conn_id}; dropping it");
                    self.drop_conn(conn_id, "outbound queue full");
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        } else {
            // A join snapshot must not be truncated by a transient writer stall:
            // block the core until the writer catches up, then fail explicitly.
            match outbound.send_timeout(bytes, SNAPSHOT_SEND_TIMEOUT) {
                Ok(()) => true,
                Err(SendTimeoutError::Timeout(_)) | Err(SendTimeoutError::Disconnected(_)) => {
                    log::warn!("snapshot send to conn {conn_id} failed");
                    self.drop_conn(conn_id, "snapshot send failed");
                    false
                }
            }
        }
    }

    /// Broadcast path: queued in the outbox while the connection is pending.
    fn deliver(&mut self, conn_id: ConnId, kind: FrameKind, payload: &[u8], g: u64) -> bool {
        let Some(conn) = self.conns.get_mut(&conn_id) else {
            return false;
        };
        if conn.sid.is_none() {
            return true;
        }
        let cipher = if conn.write_encrypted {
            conn.cipher.as_mut()
        } else {
            None
        };
        let Ok(bytes) = wire::frame_with_cipher(kind, payload, cipher) else {
            return false;
        };
        if conn.live {
            match conn.outbound.try_send(bytes) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    log::warn!("outbound queue full for conn {conn_id}");
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        } else if conn.outbox.push(g, bytes) {
            true
        } else {
            log::warn!("pending outbox full for conn {conn_id}; dropping it");
            false
        }
    }

    fn broadcast(&mut self, g: u64, message: &ServerMessage, except: Option<Sid>) {
        let Ok(payload) = protocol::to_json_payload(message) else {
            log::warn!("broadcast serialize failed");
            return;
        };
        let targets: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, conn)| conn.sid.is_some() && conn.sid != except)
            .map(|(id, _)| *id)
            .collect();
        for id in targets {
            if !self.deliver(id, FrameKind::Json, &payload, g) {
                self.drop_conn(id, "outbound failure");
            }
        }
    }

    fn send_to_sid<T: Serialize>(&mut self, sid: Sid, message: &T) {
        if let Some(conn_id) = self.conn_by_sid(sid) {
            self.send_json_direct(conn_id, message);
        }
    }

    fn broadcast_users(&mut self) {
        let g = self.server_gen.generation();
        let list = self.live_users();
        self.broadcast(g, &ServerMessage::Users { g, list }, None);
    }

    // --- state helpers -------------------------------------------------------

    fn conn_by_sid(&self, sid: Sid) -> Option<ConnId> {
        self.conns
            .iter()
            .find(|(_, conn)| conn.sid == Some(sid))
            .map(|(id, _)| *id)
    }

    /// The configured image directory as a root-relative, forward-slash path.
    fn image_dir_rel(&self) -> String {
        self.config
            .image_dir
            .strip_prefix(&self.config.root)
            .map(relative_string)
            .unwrap_or_default()
    }

    fn live_users(&self) -> Vec<UserInfo> {
        let mut list: Vec<UserInfo> = self
            .conns
            .values()
            .filter(|conn| conn.live)
            .filter_map(|conn| {
                conn.sid.map(|sid| UserInfo {
                    sid,
                    user: conn.user.clone(),
                })
            })
            .collect();
        list.sort_by_key(|user| user.sid);
        list
    }

    fn current_cursors(&self) -> Vec<CursorState> {
        self.conns
            .values()
            .filter_map(|conn| {
                let sid = conn.sid?;
                let (file, carets, selections) = conn.cursor.clone()?;
                Some(CursorState {
                    sid,
                    file,
                    carets,
                    selections,
                })
            })
            .collect()
    }

    fn doc_text(&self, path: &str) -> String {
        self.doc_text_opt(path).unwrap_or_default()
    }

    fn doc_text_opt(&self, path: &str) -> Option<String> {
        if let Some(text) = self.docs.get(path) {
            return Some(text.clone());
        }
        let full = safe_join(&self.config.root, path)?;
        let metadata = std::fs::metadata(&full).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_DOC_BYTES {
            return None;
        }
        let bytes = std::fs::read(&full).ok()?;
        String::from_utf8(bytes).ok()
    }

    fn tree_message(&self, g: u64) -> ServerMessage {
        let mut entries: Vec<TreeEntry> = Vec::new();
        for dir in &self.dirs {
            entries.push(TreeEntry {
                path: dir.clone(),
                is_dir: true,
                size: 0,
            });
        }
        for (path, size) in &self.sizes {
            entries.push(TreeEntry {
                path: path.clone(),
                is_dir: false,
                size: *size,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        ServerMessage::Tree { g, entries }
    }

    fn stop(&mut self) {
        let g = self.server_gen.generation();
        self.broadcast(g, &ServerMessage::ServerClosed { g }, None);
        self.flush_all();
        // Dropping the connection entries drops their outbound senders, so each
        // writer drains any queued frames (including `ServerClosed`) and then
        // closes its socket, which in turn unblocks its reader.
        self.conns.clear();
        self.events.send(ServerEvent::Stopped).ok();
    }

    fn flush_all(&mut self) {
        let docs: Vec<(String, String)> = self
            .docs
            .iter()
            .map(|(path, text)| (path.clone(), text.clone()))
            .collect();
        for (path, text) in docs {
            let Some(full) = safe_join(&self.config.root, &path) else {
                continue;
            };
            let disk = std::fs::read(&full)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            if disk.as_deref() != Some(text.as_str())
                && let Err(error) = write_atomic(&full, text.as_bytes())
            {
                log::warn!("flush {path} failed: {error}");
            }
        }
    }
}

fn core_loop(
    mut core: Core,
    accepted_rx: Receiver<Accepted>,
    inbound_rx: Receiver<Inbound>,
    command_rx: Receiver<CoreCommand>,
    _accepted_keepalive: Sender<Accepted>,
) {
    loop {
        select! {
            recv(accepted_rx) -> message => {
                if let Ok(accepted) = message { core.on_accept(accepted); }
            }
            recv(inbound_rx) -> message => {
                if let Ok(inbound) = message { core.on_inbound(inbound); }
            }
            recv(command_rx) -> message => match message {
                Ok(CoreCommand::Stop) => { core.stop(); break; }
                Ok(command) => core.on_command(command),
                Err(_) => { core.stop(); break; }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn scan_root(root: &Path) -> (BTreeMap<String, u64>, BTreeSet<String>) {
    let mut sizes = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    walk(root, root, &mut sizes, &mut dirs);
    (sizes, dirs)
}

fn walk(root: &Path, dir: &Path, sizes: &mut BTreeMap<String, u64>, dirs: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if entry
            .file_type()
            .map(|file_type| file_type.is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .map(relative_string)
            .unwrap_or_default();
        if metadata.is_dir() {
            dirs.insert(rel);
            walk(root, &path, sizes, dirs);
        } else if metadata.is_file() {
            sizes.insert(rel, metadata.len());
        }
    }
}

/// Whether a joined connection has waited too long for its `Ready` handshake.
fn pending_expired(pending_since: Option<Instant>, now: Instant, timeout: Duration) -> bool {
    pending_since.is_some_and(|since| now.saturating_duration_since(since) >= timeout)
}

fn relative_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Join a root-relative path, rejecting every path that would escape `root`.
///
/// Only a plain `Component::Normal` part may be appended: `..` (`ParentDir`)
/// walks out of the root, and a Windows drive prefix such as `C:` (`Prefix`)
/// makes `PathBuf::push` drop the accumulated path and continue from the prefix
/// — so `"C:/Windows/win.ini"` would resolve outside the shared root. Absolute
/// paths (`/etc/passwd`, `\\server\share`) are rejected by the leading-slash
/// test after normalization. Hidden components (`.git/config`, `.env`) are
/// rejected too: the tree never lists them, so a peer must not be able to read or
/// overwrite them by naming the path directly.
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let normalized = rel.replace('\\', "/");
    if normalized.starts_with('/') {
        return None;
    }
    let mut path = root.to_path_buf();
    for part in normalized.split('/').filter(|part| !part.is_empty()) {
        if part.starts_with('.') {
            return None;
        }
        if !matches!(
            Path::new(part).components().next(),
            Some(Component::Normal(_))
        ) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let tmp = path.with_file_name(format!(".{stem}.{seq}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn unique_image_name(dir: &Path, name: &str) -> String {
    let base = sanitize_image_name(name);
    if !dir.join(&base).exists() {
        return base;
    }
    let path = Path::new(&base);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = path.extension().and_then(|s| s.to_str());
    for index in 1..=10_000 {
        let candidate = match ext {
            Some(ext) => format!("{stem}-{index}.{ext}"),
            None => format!("{stem}-{index}"),
        };
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{stem}-{}", std::process::id())
}

/// Keep an uploaded image name to a filesystem- and Typst-safe whitelist.
///
/// The result is embedded in a `#image("…")` snippet on every peer, so quotes,
/// newlines and path separators must not survive. Path traversal is stripped by
/// taking the final component first.
fn sanitize_image_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image");
    let mut safe: String = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() || safe.starts_with('.') {
        safe.insert_str(0, "image-");
    }
    safe
}

/// Clamp a byte offset into `content` to the nearest preceding char boundary.
fn clamp_offset(content: &str, offset: u64) -> u64 {
    let mut offset = offset.min(content.len() as u64) as usize;
    while offset > 0 && !content.is_char_boundary(offset) {
        offset -= 1;
    }
    offset as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_keeps_plain_root_relative_paths() {
        let root = Path::new("/srv/project");
        assert_eq!(
            safe_join(root, "main.typ"),
            Some(PathBuf::from("/srv/project/main.typ"))
        );
        assert_eq!(
            safe_join(root, "sub/dir/a.typ"),
            Some(PathBuf::from("/srv/project/sub/dir/a.typ"))
        );
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute_paths() {
        let root = Path::new("/srv/project");
        for bad in [
            "../evil",
            "..\\\\..\\\\evil",
            "sub/../../evil",
            "/",
            "/etc/passwd",
            "\\\\server\\share\\x",
        ] {
            assert_eq!(safe_join(root, bad), None, "must reject {bad:?}");
        }
    }

    /// A drive prefix must never escape the root. On Windows it is a `Prefix`
    /// component and is rejected outright; on other platforms `C:` is an
    /// ordinary directory name and simply stays under the root.
    #[test]
    fn safe_join_never_escapes_via_a_drive_prefix() {
        let root = Path::new("/srv/project");
        if let Some(path) = safe_join(root, "C:/Windows/win.ini") {
            assert!(
                path.starts_with(root),
                "escaped the root: {}",
                path.display()
            );
        }
    }

    #[test]
    fn safe_join_rejects_hidden_paths() {
        let root = Path::new("/srv/project");
        for bad in [
            ".git/config",
            "sub/.env",
            ".typ-edit/snapshot.json",
            ".typ-edit/local/project.lock",
        ] {
            assert_eq!(safe_join(root, bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn uploaded_image_names_are_sanitized() {
        assert_eq!(sanitize_image_name("../../evil.png"), "evil.png");
        assert_eq!(sanitize_image_name("a\"b\n.typ"), "a_b_.typ");
        assert_eq!(sanitize_image_name(".hidden"), "image-.hidden");
        assert_eq!(sanitize_image_name(""), "image");
    }

    #[test]
    fn pending_join_expires_only_after_the_deadline() {
        let now = Instant::now();
        let timeout = Duration::from_millis(100);
        assert!(!pending_expired(None, now, timeout));
        assert!(!pending_expired(Some(now), now, timeout));
        assert!(pending_expired(
            Some(now - Duration::from_millis(150)),
            now,
            timeout
        ));
    }

    #[test]
    fn pending_outbox_is_bounded_by_bytes() {
        let mut outbox = Outbox::default();
        let frame = vec![0u8; 4 * 1024 * 1024];
        for g in 0..4 {
            assert!(outbox.push(g, frame.clone()));
        }
        assert!(
            !outbox.push(4, frame),
            "the byte cap must reject one frame past 16 MiB"
        );
        assert_eq!(outbox.take().len(), 4);
        assert!(outbox.take().is_empty());
    }

    #[test]
    fn pending_outbox_is_bounded_by_frames() {
        let mut outbox = Outbox::default();
        for g in 0..OUTBOX_MAX_FRAMES {
            assert!(outbox.push(g as u64, vec![0u8]));
        }
        assert!(!outbox.push(0, vec![0u8]), "the frame cap must apply");
        assert_eq!(outbox.take().len(), OUTBOX_MAX_FRAMES);
    }
}
