//! UI-free LAN collaboration core for the editor.
//!
//! This crate is the pure, testable half of the LAN collaboration feature. It
//! owns the wire protocol, the password-authenticated encryption session, the
//! generation state machine that keeps peers convergent, the server-side
//! undo/redo stacks, the byte-offset text operations, and — since Phase 2 — the
//! TCP session and mDNS discovery layers. It deliberately has no UI, no `typst`
//! and no `egui`: the editor renders remote cursors and applies remote edits
//! through the app layer, which depends on this crate, not the other way round.
//!
//! Division of responsibility mirrors the plan (`§3`): [`protocol`] encodes and
//! decodes the frame format and all messages; [`crypto`] derives the session key
//! and seals/opens traffic; [`generation`] decides what a change set does to the
//! frontier; [`history`] is the server's global undo/redo discipline; [`ops`] is
//! the byte-offset insert/delete algebra shared by the other two; [`server`] and
//! [`client`] run the TCP sessions; [`discovery`] wraps mDNS.

mod client;
mod crypto;
mod discovery;
mod error;
mod generation;
mod history;
mod ops;
mod protocol;
mod server;
mod wire;

pub use client::{ClientEvent, CollabClient};
pub use crypto::{
    CHALLENGE_LEN, KDF_ITERS, KEY_LEN, NONCE_LEN, SALT_LEN, SessionCipher, SessionRole, TAG_LEN,
    client_proof, derive_key, derive_session_key, random_challenge, random_salt, server_proof,
    verify_client_proof, verify_server_proof,
};
pub use discovery::{
    Advertiser, Browser, DiscoveredService, DiscoveryEvent, SERVICE_TYPE, ServiceTxt, build_txt,
    parse_txt,
};
pub use error::{
    ClientError, CryptoError, DiscoveryError, FrameError, RejectReason, ServerError, WireError,
};
pub use generation::{
    ClientReceive, ClientState, Generation, LocalCommit, MsgId, Queued, ReqId, ServerDecision,
    ServerState, Sid, Unacked, validate_change_set,
};
pub use history::History;
pub use ops::{ChangeOp, ChangeSet, apply, invert_ops, validate};
pub use protocol::{
    ClientHandshake, ClientMessage, CursorState, DecodedFrame, FRAME_HEADER_LEN, FileBinHeader,
    FileChunkHeader, FrameKind, GetWhat, HistoryOp, ImageUploadHeader, MAX_FRAME, PROTOCOL_VER,
    RejectKind, ServerHandshake, ServerMessage, SyncFile, TreeEntry, UserInfo, decode_binary,
    decode_binary_payload, decode_frame, decode_json, encode_binary, encode_binary_payload,
    encode_frame, encode_json, from_json_payload, to_json_payload,
};
pub use server::{CollabServer, SERVER_SID, ServerConfig, ServerEvent};
