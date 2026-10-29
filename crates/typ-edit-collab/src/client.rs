//! TCP collaboration client (plan `§5.1`, `§5.3`, `§5.5`, `§5.10`–`§5.11`).
//!
//! ## Split of responsibility
//!
//! The client owns the *numbering and gating* half of the protocol: it runs
//! [`ClientState`] (R1 optimistic commits, R2 receive rules, the re-sync flow) and
//! keeps [`ClientEvent`]s flowing to the host. It never touches the project's
//! files — applying change-set content to open buffers and mirror files is the
//! host's job (Phase 4), so each applied change is surfaced as an event the host
//! applies in order. Symmetrically, the client does not build image-insertion
//! snippets: it reports `ImageAdded` and lets the host send the snippet.
//!
//! A **core** thread is the single serialization point, fed by one `select!` over
//! raw inbound frames (from the dedicated **reader** thread) and host commands.
//! A **writer** thread drains a bounded outbound queue. All socket writes and AEAD
//! sealing happen on the core, so the per-direction nonce counters advance in one
//! place.

use crate::crypto::{
    KDF_ITERS, SessionCipher, SessionRole, client_proof, derive_key, derive_session_key,
    verify_server_proof,
};
use crate::error::{ClientError, WireError};
use crate::generation::{ClientReceive, ClientState, MsgId, ReqId, Sid};
use crate::ops::ChangeSet;
use crate::protocol::{
    self, ClientHandshake, ClientMessage, CursorState, FileBinHeader, FileChunkHeader, FrameKind,
    GetWhat, HistoryOp, ImageUploadHeader, PROTOCOL_VER, RejectKind, ServerHandshake,
    ServerMessage, SyncFile, TreeEntry, UserInfo,
};
use crate::wire;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select, unbounded};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Outbound frames buffered before a slow client is dropped.
const MAX_QUEUE: usize = 1024;

/// Frames buffered between the reader and the client core. A full queue blocks
/// the reader, so a flooding server cannot grow client memory without bound.
const INBOUND_BOUND: usize = 4;

/// Events buffered for the host; the core blocks if the host falls behind.
const EVENT_BOUND: usize = 1024;

/// Per-address TCP connect timeout during multi-address fallback.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Handshake read timeout. A server that is still scanning a large project, or
/// whose core is streaming a join/resync snapshot, can legitimately take longer
/// than a few seconds to answer `Hello`/`Joined`; the old 5 s value surfaced the
/// raw Windows "did not properly respond" socket text as a failed join.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long shutdown waits for the client threads.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

/// How often shutdown polls the thread handles.
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);

/// Events the client publishes to the host.
#[derive(Clone, Debug)]
pub enum ClientEvent {
    /// The join succeeded; the snapshot messages follow.
    Joined {
        sid: Sid,
        generation: u64,
        project: String,
        server_user: String,
        users: Vec<UserInfo>,
        cursors: Vec<CursorState>,
    },
    /// The whole snapshot has been delivered; the host may apply it and call
    /// [`CollabClient::ready`].
    SnapshotComplete,
    /// A change set was applied at the frontier; apply it in order.
    Changes {
        origin: Sid,
        msg: MsgId,
        set: ChangeSet,
    },
    /// A remote cursor update.
    Cursors {
        sid: Sid,
        user: String,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    },
    /// The participant list changed.
    Users { list: Vec<UserInfo> },
    /// Undo/redo availability changed.
    HistoryState { can_undo: bool, can_redo: bool },
    /// The server's configuration.
    Config {
        name: String,
        main_file: Option<String>,
        server_user: String,
        /// Root-relative image directory chosen by the server.
        image_dir: Option<String>,
    },
    /// The file tree.
    Tree { entries: Vec<TreeEntry> },
    /// A text file's content.
    FileText { path: String, text: String },
    /// A binary file's content.
    FileBin { path: String, bytes: Vec<u8> },
    /// One chunk of a chunked file download. Chunks arrive in order; `total`
    /// is the complete file length.
    FileChunk {
        path: String,
        offset: u64,
        total: u64,
        bytes: Vec<u8>,
    },
    /// The server refused a normal download because the file exceeds the
    /// ordinary size limit; the app asks the user before retrying as large.
    FileTooLarge { path: String, size: u64 },
    /// The server has no readable file at this path.
    FileUnavailable { path: String },
    /// A re-sync snapshot: replace each `files` entry, then apply `applied`.
    SyncState {
        req: ReqId,
        generation: u64,
        files: Vec<SyncFile>,
        applied: Vec<ChangeSet>,
    },
    /// A re-sync started; local edits are dropped until it completes.
    Resyncing { files: Vec<String> },
    /// A local edit was dropped because a re-sync is in flight.
    LocalEditDropped,
    /// A save completed.
    Saved { path: String, ok: bool },
    /// An image was stored by the server.
    ImageAdded {
        upload_id: u64,
        name: String,
        by: Sid,
    },
    /// A ping round trip completed.
    Pong { id: u64, rtt: Duration },
    /// The server is busy with a snapshot/resync; pause the heartbeat timeout.
    Busy { busy: bool },
    /// The server rejected a change set; a re-sync is starting.
    Desync,
    /// The server reported us stale; a re-sync is starting.
    YouAreStale,
    /// The join was rejected.
    Rejected { error: RejectKind },
    /// The server closed the session.
    ServerClosed,
    /// The connection ended.
    Disconnected,
}

enum ClientCommand {
    Changes(ChangeSet),
    Cursors {
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    },
    History(HistoryOp),
    Save(String),
    Upload {
        upload_id: u64,
        name: String,
        bytes: Vec<u8>,
    },
    Get(GetWhat),
    Sync(Vec<String>),
    SetOpenFiles(Vec<String>),
    Ready,
    Ping,
    Disconnect,
}

enum Inbound {
    Frame(Vec<u8>),
    Closed,
}

/// A connected collaboration client.
pub struct CollabClient {
    sid: Sid,
    generation: Arc<AtomicU64>,
    commands: Sender<ClientCommand>,
    events: Receiver<ClientEvent>,
    next_upload: AtomicU64,
    handles: Vec<JoinHandle<()>>,
    active: bool,
}

impl CollabClient {
    /// Connect to the first reachable address and complete the handshake.
    ///
    /// Each address is tried with a [`CONNECT_TIMEOUT`]; the first TCP success is
    /// used, and a handshake failure on it is terminal (no further fallback).
    ///
    /// A candidate that refuses, times out or has no route is simply not
    /// reachable: the loop moves on, and an exhausted list reports
    /// [`ClientError::Unreachable`]. A per-candidate socket error is never
    /// surfaced — on Windows a connect timeout is `os error 10060`, whose OS text
    /// ("a connection attempt failed because the connected party did not
    /// properly respond…") tells a user nothing and would reach them as
    /// `session error: socket error: …`.
    pub fn connect(
        addresses: &[SocketAddr],
        user: &str,
        password: Option<&str>,
    ) -> Result<Self, ClientError> {
        for address in addresses {
            match TcpStream::connect_timeout(address, CONNECT_TIMEOUT) {
                Ok(stream) => return Self::handshake(stream, user, password),
                // Not reachable at this address; try the next candidate.
                Err(_) => continue,
            }
        }
        Err(ClientError::Unreachable)
    }

    /// The assigned session id.
    pub fn sid(&self) -> Sid {
        self.sid
    }

    /// The current local frontier.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Receive the next event, waiting up to `timeout`.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<ClientEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Receive the next event if one is already queued.
    pub fn try_recv_event(&self) -> Option<ClientEvent> {
        self.events.try_recv().ok()
    }

    /// Send a locally applied change set (R1). The host must have applied it.
    pub fn send_changes(&self, set: ChangeSet) {
        let _ = self.commands.send(ClientCommand::Changes(set));
    }

    /// Send this client's cursor for `file`.
    pub fn send_cursors(&self, file: &str, carets: Vec<u64>, selections: Vec<(u64, u64)>) {
        let _ = self.commands.send(ClientCommand::Cursors {
            file: file.to_owned(),
            carets,
            selections,
        });
    }

    /// Request a server-side undo or redo.
    pub fn request_history(&self, op: HistoryOp) {
        let _ = self.commands.send(ClientCommand::History(op));
    }

    /// Request that `path` be written to disk, deferring until unacked clears.
    pub fn save_file(&self, path: &str) {
        let _ = self.commands.send(ClientCommand::Save(path.to_owned()));
    }

    /// Upload an image, returning the correlation id to match `ImageAdded`.
    pub fn upload_image(&self, name: &str, bytes: Vec<u8>) -> u64 {
        let upload_id = self.next_upload.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.send(ClientCommand::Upload {
            upload_id,
            name: name.to_owned(),
            bytes,
        });
        upload_id
    }

    /// Request the server configuration.
    pub fn get_config(&self) {
        let _ = self.commands.send(ClientCommand::Get(GetWhat::Config));
    }

    /// Request the file tree.
    pub fn get_tree(&self) {
        let _ = self.commands.send(ClientCommand::Get(GetWhat::Tree));
    }

    /// Request a file's content.
    pub fn get_file(&self, path: &str) {
        let _ = self.commands.send(ClientCommand::Get(GetWhat::File {
            path: path.to_owned(),
        }));
    }

    /// Stream a file as chunks. `allow_large` must only be set after the user
    /// confirmed a `FileTooLarge` prompt.
    pub fn get_chunked_file(&self, path: &str, allow_large: bool) {
        let _ = self.commands.send(ClientCommand::Get(GetWhat::ChunkedFile {
            path: path.to_owned(),
            allow_large,
        }));
    }

    /// Request every participant's cursor.
    pub fn get_cursors(&self) {
        let _ = self.commands.send(ClientCommand::Get(GetWhat::Cursors));
    }

    /// Request a re-sync of `files` (unioned with dirty/buffered/open files).
    pub fn request_sync(&self, files: Vec<String>) {
        let _ = self.commands.send(ClientCommand::Sync(files));
    }

    /// Tell the client which files the host has open, for re-sync requests.
    pub fn set_open_files(&self, files: Vec<String>) {
        let _ = self.commands.send(ClientCommand::SetOpenFiles(files));
    }

    /// Send `Ready` once the host has applied the join snapshot.
    pub fn ready(&self) {
        let _ = self.commands.send(ClientCommand::Ready);
    }

    /// Send a ping and await the `Pong` event.
    pub fn ping(&self) {
        let _ = self.commands.send(ClientCommand::Ping);
    }

    /// Send `Bye` and stop the client threads.
    pub fn disconnect(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let _ = self.commands.send(ClientCommand::Disconnect);
        join_with_deadline(std::mem::take(&mut self.handles));
    }

    fn handshake(
        stream: TcpStream,
        user: &str,
        password: Option<&str>,
    ) -> Result<Self, ClientError> {
        Self::handshake_with_timeout(stream, user, password, HANDSHAKE_TIMEOUT)
    }

    fn handshake_with_timeout(
        mut stream: TcpStream,
        user: &str,
        password: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        stream
            .set_nodelay(true)
            .map_err(|error| ClientError::Wire(WireError::Io(error)))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| ClientError::Wire(WireError::Io(error)))?;

        let hello_frame = read_handshake_frame(&mut stream)?;
        let hello: ServerHandshake = protocol::decode_json(&hello_frame)?;
        let (ver, needs_password, salt, kdf_iters, challenge) = match hello {
            ServerHandshake::Hello {
                ver,
                needs_password,
                salt,
                kdf_iters,
                challenge,
                ..
            } => (ver, needs_password, salt, kdf_iters, challenge),
            // An over-limit server refuses with a plaintext `Rejected` before
            // sending `Hello`, so surface it as the typed rejection the UI knows.
            ServerHandshake::Rejected { error, .. } => {
                return Err(ClientError::Rejected(error));
            }
            _ => return Err(ClientError::Handshake),
        };
        if ver != PROTOCOL_VER {
            return Err(ClientError::Rejected(RejectKind::Version));
        }

        let mut cipher = None;
        let mut key = None;
        let mut hmac = None;
        if needs_password {
            let password = password.ok_or(ClientError::PasswordRequired)?;
            let salt = salt.ok_or(ClientError::Handshake)?;
            let challenge = challenge.ok_or(ClientError::Handshake)?;
            let iters = kdf_iters.unwrap_or(KDF_ITERS);
            let password_key = derive_key(password, &salt, iters);
            let proof = client_proof(&password_key, &challenge, user)
                .map_err(|error| ClientError::Wire(WireError::Crypto(error)))?;
            hmac = Some(proof.to_vec());
            // The AEAD key is per connection: the password key alone is shared by
            // every connection of the server, which would reuse `(key, nonce)`
            // pairs across sockets (`derive_session_key`).
            let session_key = derive_session_key(&password_key, &challenge);
            cipher = Some(SessionCipher::new(&session_key, SessionRole::Client));
            key = Some((password_key, challenge));
        }

        let join = ClientHandshake::Join {
            ver: PROTOCOL_VER,
            user: user.to_owned(),
            hmac,
        };
        wire::write_frame(&mut stream, &protocol::encode_json(&join)?)?;

        let joined_frame = read_handshake_frame(&mut stream)?;
        let decoded = protocol::decode_frame(&joined_frame)?;
        // `Joined` is encrypted when a password is set, but a failure `Rejected`
        // is always plaintext (`§5.7`), so fall back to the raw bytes if opening
        // fails.
        let payload = match cipher.as_mut() {
            Some(cipher) => cipher
                .open(decoded.payload)
                .unwrap_or_else(|_| decoded.payload.to_vec()),
            None => decoded.payload.to_vec(),
        };
        let joined: ServerHandshake = protocol::from_json_payload(&payload)?;
        let (sid, generation, joined_project, joined_server_user, users, cursors, server_mac) =
            match joined {
                ServerHandshake::Rejected { error, .. } => {
                    return Err(ClientError::Rejected(error));
                }
                ServerHandshake::Joined {
                    sid,
                    generation,
                    project,
                    server_user,
                    users,
                    cursors,
                    server_mac,
                } => (
                    sid,
                    generation,
                    project,
                    server_user,
                    users,
                    cursors,
                    server_mac,
                ),
                _ => return Err(ClientError::Handshake),
            };

        if let Some((derived, challenge)) = &key {
            let proof_ok = server_mac
                .as_deref()
                .is_some_and(|mac| verify_server_proof(derived, challenge, user, mac));
            if !proof_ok {
                return Err(ClientError::ServerProofFailed);
            }
        }

        let _ = stream.set_read_timeout(None);

        // Spawn the reader, writer and core threads.
        let (writer_tx, writer_rx) = bounded::<Vec<u8>>(MAX_QUEUE);
        let (inbound_tx, inbound_rx) = bounded::<Inbound>(INBOUND_BOUND);
        // Host commands are trusted and low-rate; keeping them unbounded avoids
        // blocking the UI thread on a busy core.
        let (command_tx, command_rx) = unbounded::<ClientCommand>();
        let (event_tx, event_rx) = bounded::<ClientEvent>(EVENT_BOUND);
        let generation_atomic = Arc::new(AtomicU64::new(generation));

        let read_half = stream
            .try_clone()
            .map_err(|error| ClientError::Wire(WireError::Io(error)))?;
        let reader_handle = thread::spawn(move || reader_loop(read_half, inbound_tx));
        let write_half = stream
            .try_clone()
            .map_err(|error| ClientError::Wire(WireError::Io(error)))?;
        let writer_handle = thread::spawn(move || writer_loop(write_half, writer_rx));

        let mut state = ClientState::new(sid);
        state.set_generation(generation);
        let core = ClientCore {
            state,
            cipher,
            writer: writer_tx,
            events: event_tx,
            generation: generation_atomic.clone(),
            pending_join: Some(ClientEvent::Joined {
                sid,
                generation,
                project: joined_project,
                server_user: joined_server_user,
                users,
                cursors,
            }),
            next_req: 0,
            next_ping: 0,
            pending_pings: HashMap::new(),
            local_files: BTreeMap::new(),
            open_files: BTreeSet::new(),
            resync_files: Vec::new(),
            resync_req: None,
            pending_saves: Vec::new(),
        };
        let core_handle = thread::spawn(move || core_loop(core, inbound_rx, command_rx));

        drop(stream);
        Ok(Self {
            sid,
            generation: generation_atomic,
            commands: command_tx,
            events: event_rx,
            next_upload: AtomicU64::new(0),
            handles: vec![core_handle, reader_handle, writer_handle],
            active: true,
        })
    }
}

/// Reads one handshake frame, turning a read deadline into the typed timeout
/// error the UI can localize instead of exposing the OS socket text.
fn read_handshake_frame(stream: &mut TcpStream) -> Result<Vec<u8>, ClientError> {
    wire::read_frame(stream).map_err(|error| {
        if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            ClientError::HandshakeTimeout
        } else {
            ClientError::Wire(WireError::Io(error))
        }
    })
}

impl Drop for CollabClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn reader_loop(mut stream: TcpStream, inbound_tx: Sender<Inbound>) {
    loop {
        match wire::read_frame(&mut stream) {
            Ok(frame) => {
                if inbound_tx.send(Inbound::Frame(frame)).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = inbound_tx.send(Inbound::Closed);
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

struct ClientCore {
    state: ClientState,
    cipher: Option<SessionCipher>,
    writer: Sender<Vec<u8>>,
    events: Sender<ClientEvent>,
    generation: Arc<AtomicU64>,
    pending_join: Option<ClientEvent>,
    next_req: ReqId,
    next_ping: u64,
    pending_pings: HashMap<u64, Instant>,
    local_files: BTreeMap<MsgId, String>,
    open_files: BTreeSet<String>,
    resync_files: Vec<String>,
    resync_req: Option<ReqId>,
    pending_saves: Vec<String>,
}

impl ClientCore {
    fn emit(&self, event: ClientEvent) {
        let _ = self.events.send(event);
    }

    fn sync_generation(&self) {
        self.generation
            .store(self.state.generation(), Ordering::Relaxed);
    }

    fn send_message(&mut self, message: &ClientMessage) -> bool {
        match protocol::to_json_payload(message) {
            Ok(payload) => self.send_payload(FrameKind::Json, &payload),
            Err(error) => {
                log::warn!("client serialize failed: {error}");
                false
            }
        }
    }

    fn send_payload(&mut self, kind: FrameKind, payload: &[u8]) -> bool {
        let cipher = self.cipher.as_mut();
        match wire::frame_with_cipher(kind, payload, cipher) {
            Ok(bytes) => match self.writer.try_send(bytes) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    log::warn!("client outbound queue full");
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            },
            Err(error) => {
                log::warn!("client encode failed: {error}");
                false
            }
        }
    }

    // --- commands ------------------------------------------------------------

    /// Returns false when the client should stop.
    fn on_command(&mut self, command: ClientCommand) -> bool {
        match command {
            ClientCommand::Changes(set) => {
                if self.state.is_resyncing() {
                    self.emit(ClientEvent::LocalEditDropped);
                } else {
                    let commit = self.state.local_commit();
                    self.local_files.insert(commit.msg, set.file.clone());
                    self.sync_generation();
                    let file = set.file.clone();
                    let message = ClientMessage::Changes {
                        g: commit.g,
                        msg: commit.msg,
                        file,
                        ops: set.ops,
                    };
                    self.send_message(&message);
                }
            }
            ClientCommand::Cursors {
                file,
                carets,
                selections,
            } => {
                let g = self.state.generation();
                self.send_message(&ClientMessage::Cursors {
                    g,
                    file,
                    carets,
                    selections,
                });
            }
            ClientCommand::History(op) => {
                let g = self.state.generation();
                self.send_message(&ClientMessage::History { g, op });
            }
            ClientCommand::Save(path) => {
                if self.state.unacked().is_empty() {
                    self.send_save(path);
                } else {
                    self.pending_saves.push(path);
                }
            }
            ClientCommand::Upload {
                upload_id,
                name,
                bytes,
            } => {
                let g = self.state.generation();
                let header = ImageUploadHeader { g, upload_id, name };
                match protocol::encode_binary_payload(&header, &bytes) {
                    Ok(payload) => {
                        self.send_payload(FrameKind::Binary, &payload);
                    }
                    Err(error) => log::warn!("image encode failed: {error}"),
                }
            }
            ClientCommand::Get(what) => {
                let g = self.state.generation();
                self.send_message(&ClientMessage::Get { g, what });
            }
            ClientCommand::Sync(files) => self.begin_resync(Some(files)),
            ClientCommand::SetOpenFiles(files) => {
                self.open_files = files.into_iter().collect();
            }
            ClientCommand::Ready => {
                self.send_handshake(&ClientHandshake::Ready);
            }
            ClientCommand::Ping => {
                let id = self.next_ping;
                self.next_ping += 1;
                self.pending_pings.insert(id, Instant::now());
                let g = self.state.generation();
                self.send_message(&ClientMessage::Ping { g, id });
            }
            ClientCommand::Disconnect => {
                let g = self.state.generation();
                self.send_message(&ClientMessage::Bye { g });
                return false;
            }
        }
        true
    }

    fn send_save(&mut self, path: String) {
        let g = self.state.generation();
        self.send_message(&ClientMessage::Save {
            g,
            g_expected: g,
            path,
        });
    }

    fn send_handshake(&mut self, handshake: &ClientHandshake) {
        match protocol::to_json_payload(handshake) {
            Ok(payload) => {
                self.send_payload(FrameKind::Json, &payload);
            }
            Err(error) => log::warn!("handshake serialize failed: {error}"),
        }
    }

    fn flush_pending_saves(&mut self) {
        if !self.state.unacked().is_empty() || self.pending_saves.is_empty() {
            return;
        }
        for path in std::mem::take(&mut self.pending_saves) {
            self.send_save(path);
        }
    }

    // --- inbound -------------------------------------------------------------

    fn on_inbound(&mut self, inbound: Inbound) -> bool {
        match inbound {
            Inbound::Frame(frame) => {
                self.on_frame(&frame);
                true
            }
            Inbound::Closed => {
                self.emit(ClientEvent::Disconnected);
                false
            }
        }
    }

    fn on_frame(&mut self, frame: &[u8]) {
        let parsed = wire::parse_inbound(frame, self.cipher.as_mut());
        let (kind, payload) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                log::warn!("client bad frame: {error}");
                return;
            }
        };
        if kind == FrameKind::Binary {
            if let Ok((header, bytes)) =
                protocol::decode_binary_payload::<FileChunkHeader>(&payload)
            {
                self.emit(ClientEvent::FileChunk {
                    path: header.path,
                    offset: header.offset,
                    total: header.total,
                    bytes,
                });
                return;
            }
            match protocol::decode_binary_payload::<FileBinHeader>(&payload) {
                Ok((header, bytes)) => {
                    self.emit(ClientEvent::FileBin {
                        path: header.path,
                        bytes,
                    });
                }
                Err(error) => log::warn!("client bad binary frame: {error}"),
            }
            return;
        }
        match protocol::from_json_payload::<ServerMessage>(&payload) {
            Ok(message) => self.on_message(message),
            Err(error) => log::warn!("client bad message: {error}"),
        }
    }

    fn on_message(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Changes {
                g,
                origin,
                msg,
                file,
                ops,
            } => self.on_changes(origin, msg, g, ChangeSet::new(file, ops)),
            ServerMessage::Cursors {
                sid,
                user,
                file,
                carets,
                selections,
                ..
            } => self.emit(ClientEvent::Cursors {
                sid,
                user,
                file,
                carets,
                selections,
            }),
            ServerMessage::Users { list, .. } => self.emit(ClientEvent::Users { list }),
            ServerMessage::HistoryState {
                can_undo, can_redo, ..
            } => self.emit(ClientEvent::HistoryState { can_undo, can_redo }),
            ServerMessage::Config {
                name,
                main_file,
                server_user,
                image_dir,
                ..
            } => self.emit(ClientEvent::Config {
                name,
                main_file,
                server_user,
                image_dir,
            }),
            ServerMessage::Tree { entries, .. } => self.emit(ClientEvent::Tree { entries }),
            ServerMessage::FileText { path, text, .. } => {
                self.emit(ClientEvent::FileText { path, text });
            }
            ServerMessage::FileBin { path, bytes, .. } => {
                self.emit(ClientEvent::FileBin { path, bytes });
            }
            ServerMessage::FileTooLarge { path, size, .. } => {
                self.emit(ClientEvent::FileTooLarge { path, size });
            }
            ServerMessage::FileUnavailable { path, .. } => {
                self.emit(ClientEvent::FileUnavailable { path });
            }
            ServerMessage::Saved { path, ok, .. } => self.emit(ClientEvent::Saved { path, ok }),
            ServerMessage::ImageAdded {
                upload_id,
                name,
                by,
                ..
            } => self.emit(ClientEvent::ImageAdded {
                upload_id,
                name,
                by,
            }),
            ServerMessage::SyncState { g, req, files, .. } => self.on_sync_state(req, g, files),
            ServerMessage::YouAreStale { .. } => {
                self.emit(ClientEvent::YouAreStale);
                self.begin_resync(None);
            }
            ServerMessage::Desync { .. } => {
                self.emit(ClientEvent::Desync);
                self.begin_resync(None);
            }
            ServerMessage::Pong { id, .. } => {
                if let Some(start) = self.pending_pings.remove(&id) {
                    self.emit(ClientEvent::Pong {
                        id,
                        rtt: start.elapsed(),
                    });
                }
            }
            ServerMessage::Busy { busy, .. } => self.emit(ClientEvent::Busy { busy }),
            ServerMessage::ServerClosed { .. } => {
                self.state.abandon();
                self.sync_generation();
                self.emit(ClientEvent::ServerClosed);
            }
            ServerMessage::SnapshotDone { .. } => self.emit(ClientEvent::SnapshotComplete),
        }
    }

    fn on_changes(&mut self, origin: Sid, msg: MsgId, g: u64, set: ChangeSet) {
        match self.state.receive(origin, msg, g, set) {
            ClientReceive::Applied(applied) => {
                self.sync_generation();
                for queued in applied {
                    self.emit(ClientEvent::Changes {
                        origin: queued.origin,
                        msg: queued.msg,
                        set: queued.set,
                    });
                }
                self.flush_pending_saves();
            }
            ClientReceive::Confirmed => {
                self.local_files.remove(&msg);
                self.sync_generation();
                self.flush_pending_saves();
            }
            ClientReceive::Buffered | ClientReceive::IgnoredStale => {}
            ClientReceive::PeerStale { about } => {
                let g = self.state.generation();
                self.send_message(&ClientMessage::PeerStale { g, about });
            }
        }
    }

    fn on_sync_state(&mut self, req: ReqId, g: u64, files: Vec<SyncFile>) {
        if !self.state.accept_sync_state(req, g) {
            log::warn!("ignoring stale SyncState req={req} g={g}");
            if let Some(inflight) = self.resync_req {
                let files = self.resync_files.clone();
                let g = self.state.generation();
                self.send_message(&ClientMessage::Sync {
                    g,
                    req: inflight,
                    files,
                });
            }
            return;
        }
        let applied = self.state.finish_resync(g);
        self.sync_generation();
        self.local_files.clear();
        self.resync_req = None;
        self.emit(ClientEvent::SyncState {
            req,
            generation: g,
            files,
            applied: applied.into_iter().map(|queued| queued.set).collect(),
        });
        self.flush_pending_saves();
    }

    fn begin_resync(&mut self, requested: Option<Vec<String>>) {
        if self.state.is_resyncing() {
            if let Some(requested) = requested {
                for file in requested {
                    if !self.resync_files.contains(&file) {
                        self.resync_files.push(file);
                    }
                }
            }
            return;
        }
        let req = self.next_req;
        self.next_req += 1;
        let own_files: Vec<String> = std::mem::take(&mut self.local_files)
            .into_values()
            .collect();
        let (req, _floor) = self.state.start_resync(req);
        self.sync_generation();

        let mut files: BTreeSet<String> = BTreeSet::new();
        files.extend(self.state.dirty_files().iter().cloned());
        files.extend(self.state.buffered_files());
        files.extend(self.open_files.iter().cloned());
        files.extend(own_files);
        if let Some(requested) = requested {
            files.extend(requested);
        }
        let files: Vec<String> = files.into_iter().collect();
        self.resync_files = files.clone();
        self.resync_req = Some(req);
        self.emit(ClientEvent::Resyncing {
            files: files.clone(),
        });
        let g = self.state.generation();
        self.send_message(&ClientMessage::Sync { g, req, files });
    }
}

fn core_loop(
    mut core: ClientCore,
    inbound_rx: Receiver<Inbound>,
    command_rx: Receiver<ClientCommand>,
) {
    if let Some(event) = core.pending_join.take() {
        core.emit(event);
    }
    loop {
        select! {
            recv(inbound_rx) -> message => {
                if let Ok(inbound) = message
                    && !core.on_inbound(inbound)
                {
                    break;
                }
            }
            recv(command_rx) -> message => match message {
                Ok(command) => {
                    if !core.on_command(command) { break; }
                }
                Err(_) => break,
            }
        }
    }
}

fn join_with_deadline(mut handles: Vec<JoinHandle<()>>) {
    let deadline = Instant::now() + SHUTDOWN_DEADLINE;
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
        log::warn!("collab client thread still running at the shutdown deadline; detaching");
        drop(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn handshake_read_timeout_is_typed() {
        // A listening socket with no accepting thread models a server that is
        // still scanning or whose core is blocked: TCP succeeds, but no Hello
        // arrives. The client must not surface the OS "did not properly
        // respond" text as a generic wire error.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind silent listener");
        let address = listener.local_addr().expect("listener address");
        let stream = TcpStream::connect(address).expect("tcp connect");

        let started = Instant::now();
        let result =
            CollabClient::handshake_with_timeout(stream, "ann", None, Duration::from_millis(50));
        let Err(error) = result else {
            panic!("a silent server must not complete the handshake");
        };
        assert!(
            matches!(error, ClientError::HandshakeTimeout),
            "a read deadline must be a typed handshake timeout, got {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the test timeout must not wait for the production 30 s deadline"
        );
    }
}
