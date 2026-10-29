//! Wire protocol: the `[kind][len][payload]` frame codec and the serde message
//! catalog from plan `§5.2`.
//!
//! Every frame is `[u8 kind][u32 big-endian length][payload]`. `kind = 0` carries
//! JSON; `kind = 1` carries a binary block laid out as
//! `[u16 header_len][header_json][raw bytes]` for image uploads and large file
//! transfers. Frames larger than [`MAX_FRAME`] are refused by both encoder and
//! decoder, and every malformed buffer produces a typed [`FrameError`] rather
//! than a panic.
//!
//! The message types are split the way the plan's tables are: handshake messages
//! ([`ClientHandshake`], [`ServerHandshake`]) are always plaintext and carry no
//! generation; in-band messages ([`ClientMessage`], [`ServerMessage`]) all carry
//! `g`, the app-state generation the message is written against.

use crate::error::FrameError;
use crate::ops::ChangeOp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Maximum accepted frame size (64 MiB); payloads beyond this are refused.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Bytes in the fixed frame header: kind (1) + length (4).
pub const FRAME_HEADER_LEN: usize = 5;

/// Wire protocol version exchanged in the handshake.
///
/// Bumped to 2 when the AEAD key became per connection (`derive_session_key`):
/// a v1 peer would otherwise seal with the shared password key and hit nonce
/// reuse against a v2 peer. Peers of different versions refuse each other with
/// `RejectKind::Version`.
pub const PROTOCOL_VER: u32 = 2;

/// The two payload encodings a frame kind selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// JSON payload (`kind = 0`).
    Json = 0,
    /// Binary block with a JSON header (`kind = 1`).
    Binary = 1,
}

impl FrameKind {
    fn from_byte(byte: u8) -> Result<Self, FrameError> {
        match byte {
            0 => Ok(Self::Json),
            1 => Ok(Self::Binary),
            other => Err(FrameError::UnknownKind(other)),
        }
    }
}

/// A frame header parsed off the wire, borrowing the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedFrame<'a> {
    /// The payload encoding.
    pub kind: FrameKind,
    /// The payload bytes (after the 5-byte header).
    pub payload: &'a [u8],
}

/// Encode a raw payload into a `[kind][len][payload]` frame.
pub fn encode_frame(kind: FrameKind, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_FRAME {
        return Err(FrameError::PayloadTooLarge {
            len: payload.len(),
            max: MAX_FRAME,
        });
    }
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.push(kind as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Parse a `[kind][len][payload]` frame, returning the payload slice.
pub fn decode_frame(bytes: &[u8]) -> Result<DecodedFrame<'_>, FrameError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Err(FrameError::ShortBuffer {
            needed: FRAME_HEADER_LEN,
            available: bytes.len(),
        });
    }
    let kind = FrameKind::from_byte(bytes[0])?;
    let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::PayloadTooLarge {
            len,
            max: MAX_FRAME,
        });
    }
    let end = FRAME_HEADER_LEN + len;
    if bytes.len() < end {
        return Err(FrameError::ShortBuffer {
            needed: end,
            available: bytes.len(),
        });
    }
    Ok(DecodedFrame {
        kind,
        payload: &bytes[FRAME_HEADER_LEN..end],
    })
}

/// Serialize `msg` to a bare JSON payload (no frame header).
pub fn to_json_payload<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    Ok(serde_json::to_vec(msg)?)
}

/// Deserialize a bare JSON payload (no frame header).
pub fn from_json_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    Ok(serde_json::from_slice(payload)?)
}

/// Serialize `msg` to JSON and wrap it in a JSON frame.
pub fn encode_json<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    encode_frame(FrameKind::Json, &to_json_payload(msg)?)
}

/// Parse a JSON frame and deserialize its payload.
pub fn decode_json<T: DeserializeOwned>(frame: &[u8]) -> Result<T, FrameError> {
    let decoded = decode_frame(frame)?;
    if decoded.kind != FrameKind::Json {
        return Err(FrameError::WrongKind {
            expected: FrameKind::Json as u8,
            actual: decoded.kind as u8,
        });
    }
    from_json_payload(decoded.payload)
}

/// Build a bare binary payload: `[u16 header_len][header_json][body]`.
pub fn encode_binary_payload<T: Serialize>(header: &T, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    let header_json = serde_json::to_vec(header)?;
    let header_len = u16::try_from(header_json.len()).map_err(|_| FrameError::HeaderTooLong {
        len: header_json.len(),
    })?;
    let mut payload = Vec::with_capacity(2 + header_json.len() + body.len());
    payload.extend_from_slice(&header_len.to_be_bytes());
    payload.extend_from_slice(&header_json);
    payload.extend_from_slice(body);
    Ok(payload)
}

/// Parse a bare binary payload into its header and raw body.
pub fn decode_binary_payload<T: DeserializeOwned>(
    payload: &[u8],
) -> Result<(T, Vec<u8>), FrameError> {
    if payload.len() < 2 {
        return Err(FrameError::ShortBuffer {
            needed: 2,
            available: payload.len(),
        });
    }
    let declared = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let header_end = 2 + declared;
    if payload.len() < header_end {
        return Err(FrameError::TruncatedHeader {
            declared,
            available: payload.len() - 2,
        });
    }
    let header = serde_json::from_slice(&payload[2..header_end])?;
    Ok((header, payload[header_end..].to_vec()))
}

/// Encode a binary frame: JSON header, then the raw body.
pub fn encode_binary<T: Serialize>(header: &T, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    encode_frame(FrameKind::Binary, &encode_binary_payload(header, body)?)
}

/// Parse a binary frame, returning the deserialized header and the raw body.
pub fn decode_binary<T: DeserializeOwned>(frame: &[u8]) -> Result<(T, Vec<u8>), FrameError> {
    let decoded = decode_frame(frame)?;
    if decoded.kind != FrameKind::Binary {
        return Err(FrameError::WrongKind {
            expected: FrameKind::Binary as u8,
            actual: decoded.kind as u8,
        });
    }
    decode_binary_payload(decoded.payload)
}

/// Handshake messages a client sends before the session is live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientHandshake {
    /// `Hello2`/`Join`: version, display name and, when a password is required,
    /// the client's HMAC proof.
    Join {
        ver: u32,
        user: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hmac: Option<Vec<u8>>,
    },
    /// Snapshot applied; request live broadcast.
    Ready,
}

/// Handshake messages the server sends before the session is live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerHandshake {
    /// First frame: version, project, whether a password is required and, if so,
    /// the KDF parameters and the per-connection challenge.
    Hello {
        ver: u32,
        project: String,
        server_user: String,
        needs_password: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        salt: Option<Vec<u8>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kdf_iters: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        challenge: Option<Vec<u8>>,
        user_count: u32,
    },
    /// Success: the first encrypted frame when a password is set (the client
    /// verifies `server_mac` before treating the session as established).
    Joined {
        sid: u64,
        generation: u64,
        project: String,
        server_user: String,
        users: Vec<UserInfo>,
        cursors: Vec<CursorState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_mac: Option<Vec<u8>>,
    },
    /// Failure path, always plaintext and followed by a close.
    Rejected { g: u64, error: RejectKind },
}

/// Why the server rejected a join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectKind {
    /// Password missing or wrong.
    Auth,
    /// Protocol version mismatch.
    Version,
    /// Session is full.
    Full,
}

/// A participant identity: session id plus display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub sid: u64,
    pub user: String,
}

/// A participant's cursor state for one file, in byte offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    pub sid: u64,
    pub file: String,
    pub carets: Vec<u64>,
    pub selections: Vec<(u64, u64)>,
}

/// One file-tree entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// One file's text inside a re-sync snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFile {
    pub path: String,
    pub text: String,
}

/// The `Get` request target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum GetWhat {
    Config,
    Tree,
    File {
        path: String,
    },
    /// Stream a file as bounded binary chunks. `allow_large` is only set after
    /// the user confirms a `FileTooLarge` prompt.
    ChunkedFile {
        path: String,
        allow_large: bool,
    },
    Cursors,
}

/// A `History` request kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOp {
    Undo,
    Redo,
}

/// Header of an `ImageUpload` binary frame (body is the raw image bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUploadHeader {
    pub g: u64,
    pub upload_id: u64,
    pub name: String,
}

/// Header of a `FileBin` binary frame (body is the raw file bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBinHeader {
    pub g: u64,
    pub path: String,
}

/// Header of one `FileChunk` binary frame (body is a slice of the file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunkHeader {
    pub g: u64,
    pub path: String,
    pub offset: u64,
    pub total: u64,
}

/// In-band client → server messages; every one carries its generation `g`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Changes {
        g: u64,
        msg: u64,
        file: String,
        ops: Vec<ChangeOp>,
    },
    History {
        g: u64,
        op: HistoryOp,
    },
    Cursors {
        g: u64,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    },
    Get {
        g: u64,
        what: GetWhat,
    },
    Save {
        g: u64,
        g_expected: u64,
        path: String,
    },
    ImageUpload {
        g: u64,
        upload_id: u64,
        name: String,
        bytes: Vec<u8>,
    },
    Sync {
        g: u64,
        req: u64,
        files: Vec<String>,
    },
    PeerStale {
        g: u64,
        about: u64,
    },
    Ping {
        g: u64,
        id: u64,
    },
    Bye {
        g: u64,
    },
}

/// In-band server → client messages; every one carries its generation `g`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Changes {
        g: u64,
        origin: u64,
        msg: u64,
        file: String,
        ops: Vec<ChangeOp>,
    },
    HistoryState {
        g: u64,
        can_undo: bool,
        can_redo: bool,
    },
    Cursors {
        g: u64,
        sid: u64,
        user: String,
        file: String,
        carets: Vec<u64>,
        selections: Vec<(u64, u64)>,
    },
    Users {
        g: u64,
        list: Vec<UserInfo>,
    },
    Config {
        g: u64,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        main_file: Option<String>,
        server_user: String,
        /// Root-relative directory where uploaded images are stored. The
        /// server owns it; clients must not guess from their own settings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_dir: Option<String>,
    },
    Tree {
        g: u64,
        entries: Vec<TreeEntry>,
    },
    FileText {
        g: u64,
        path: String,
        text: String,
    },
    FileBin {
        g: u64,
        path: String,
        bytes: Vec<u8>,
    },
    FileTooLarge {
        g: u64,
        path: String,
        size: u64,
    },
    FileUnavailable {
        g: u64,
        path: String,
    },
    Saved {
        g: u64,
        path: String,
        ok: bool,
    },
    ImageAdded {
        g: u64,
        upload_id: u64,
        name: String,
        by: u64,
    },
    SyncState {
        g: u64,
        req: u64,
        files: Vec<SyncFile>,
    },
    YouAreStale {
        g: u64,
    },
    Desync {
        g: u64,
    },
    Pong {
        g: u64,
        id: u64,
    },
    /// The server is about to spend a while building or streaming a snapshot
    /// (join or resync) and may not answer pings promptly. Live clients pause
    /// their heartbeat timeout while `busy` is true; the app also has a safety
    /// deadline.
    Busy {
        g: u64,
        busy: bool,
    },
    ServerClosed {
        g: u64,
    },
    /// End of the join snapshot (plan `§5.4`): the last message before the
    /// connection turns live. It exists because the base catalog has no
    /// terminator, yet the client must know when it has applied every snapshot
    /// file before it may send `Ready`.
    SnapshotDone {
        g: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ops() -> Vec<ChangeOp> {
        vec![
            ChangeOp::Insert {
                at: 3,
                text: "abc".to_owned(),
            },
            ChangeOp::Delete {
                at: 0,
                text: "\u{4f60}".to_owned(),
            },
        ]
    }

    fn client_handshakes() -> Vec<ClientHandshake> {
        vec![
            ClientHandshake::Join {
                ver: 1,
                user: "ann".to_owned(),
                hmac: Some(vec![1, 2, 3]),
            },
            ClientHandshake::Join {
                ver: 1,
                user: "bob".to_owned(),
                hmac: None,
            },
            ClientHandshake::Ready,
        ]
    }

    fn server_handshakes() -> Vec<ServerHandshake> {
        vec![
            ServerHandshake::Hello {
                ver: 1,
                project: "p".to_owned(),
                server_user: "srv".to_owned(),
                needs_password: true,
                salt: Some(vec![9; 16]),
                kdf_iters: Some(200_000),
                challenge: Some(vec![7; 16]),
                user_count: 2,
            },
            ServerHandshake::Hello {
                ver: 1,
                project: "p".to_owned(),
                server_user: "srv".to_owned(),
                needs_password: false,
                salt: None,
                kdf_iters: None,
                challenge: None,
                user_count: 0,
            },
            ServerHandshake::Joined {
                sid: 3,
                generation: 5,
                project: "p".to_owned(),
                server_user: "srv".to_owned(),
                users: vec![UserInfo {
                    sid: 3,
                    user: "ann".to_owned(),
                }],
                cursors: vec![CursorState {
                    sid: 3,
                    file: "a.typ".to_owned(),
                    carets: vec![0, 4],
                    selections: vec![(1, 2)],
                }],
                server_mac: Some(vec![4; 32]),
            },
            ServerHandshake::Rejected {
                g: 0,
                error: RejectKind::Auth,
            },
        ]
    }

    fn client_messages() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Changes {
                g: 5,
                msg: 1,
                file: "a.typ".to_owned(),
                ops: sample_ops(),
            },
            ClientMessage::History {
                g: 5,
                op: HistoryOp::Undo,
            },
            ClientMessage::Cursors {
                g: 5,
                file: "a.typ".to_owned(),
                carets: vec![1],
                selections: vec![(0, 1)],
            },
            ClientMessage::Get {
                g: 5,
                what: GetWhat::Config,
            },
            ClientMessage::Get {
                g: 5,
                what: GetWhat::Tree,
            },
            ClientMessage::Get {
                g: 5,
                what: GetWhat::File {
                    path: "x.typ".to_owned(),
                },
            },
            ClientMessage::Get {
                g: 5,
                what: GetWhat::ChunkedFile {
                    path: "big.png".to_owned(),
                    allow_large: true,
                },
            },
            ClientMessage::Get {
                g: 5,
                what: GetWhat::Cursors,
            },
            ClientMessage::Save {
                g: 5,
                g_expected: 4,
                path: "a.typ".to_owned(),
            },
            ClientMessage::ImageUpload {
                g: 5,
                upload_id: 9,
                name: "img.png".to_owned(),
                bytes: vec![0, 1, 2],
            },
            ClientMessage::Sync {
                g: 5,
                req: 2,
                files: vec!["a.typ".to_owned()],
            },
            ClientMessage::PeerStale { g: 5, about: 7 },
            ClientMessage::Ping { g: 5, id: 42 },
            ClientMessage::Bye { g: 5 },
        ]
    }

    fn server_messages() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Changes {
                g: 6,
                origin: 1,
                msg: 3,
                file: "a.typ".to_owned(),
                ops: sample_ops(),
            },
            ServerMessage::HistoryState {
                g: 6,
                can_undo: true,
                can_redo: false,
            },
            ServerMessage::Cursors {
                g: 6,
                sid: 1,
                user: "ann".to_owned(),
                file: "a.typ".to_owned(),
                carets: vec![2],
                selections: vec![(0, 2)],
            },
            ServerMessage::Users {
                g: 6,
                list: vec![UserInfo {
                    sid: 1,
                    user: "ann".to_owned(),
                }],
            },
            ServerMessage::Config {
                g: 6,
                name: "proj".to_owned(),
                main_file: Some("main.typ".to_owned()),
                server_user: "srv".to_owned(),
                image_dir: Some("image".to_owned()),
            },
            ServerMessage::Config {
                g: 6,
                name: "proj".to_owned(),
                main_file: None,
                server_user: "srv".to_owned(),
                image_dir: Some("assets".to_owned()),
            },
            ServerMessage::Tree {
                g: 6,
                entries: vec![TreeEntry {
                    path: "a.typ".to_owned(),
                    is_dir: false,
                    size: 12,
                }],
            },
            ServerMessage::FileText {
                g: 6,
                path: "a.typ".to_owned(),
                text: "hi".to_owned(),
            },
            ServerMessage::FileBin {
                g: 6,
                path: "img.png".to_owned(),
                bytes: vec![9, 8, 7],
            },
            ServerMessage::FileTooLarge {
                g: 6,
                path: "big.png".to_owned(),
                size: 42,
            },
            ServerMessage::FileUnavailable {
                g: 6,
                path: "gone.png".to_owned(),
            },
            ServerMessage::Saved {
                g: 6,
                path: "a.typ".to_owned(),
                ok: true,
            },
            ServerMessage::ImageAdded {
                g: 6,
                upload_id: 9,
                name: "img.png".to_owned(),
                by: 1,
            },
            ServerMessage::SyncState {
                g: 6,
                req: 2,
                files: vec![SyncFile {
                    path: "a.typ".to_owned(),
                    text: "hi".to_owned(),
                }],
            },
            ServerMessage::YouAreStale { g: 6 },
            ServerMessage::Desync { g: 6 },
            ServerMessage::Pong { g: 6, id: 42 },
            ServerMessage::Busy { g: 6, busy: true },
            ServerMessage::ServerClosed { g: 6 },
        ]
    }

    #[test]
    fn client_handshake_json_round_trip() {
        for msg in client_handshakes() {
            let frame = encode_json(&msg).expect("encode");
            let back: ClientHandshake = decode_json(&frame).expect("decode");
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn server_handshake_json_round_trip() {
        for msg in server_handshakes() {
            let frame = encode_json(&msg).expect("encode");
            let back: ServerHandshake = decode_json(&frame).expect("decode");
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn client_message_json_round_trip() {
        for msg in client_messages() {
            let frame = encode_json(&msg).expect("encode");
            let back: ClientMessage = decode_json(&frame).expect("decode");
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn server_message_json_round_trip() {
        for msg in server_messages() {
            let frame = encode_json(&msg).expect("encode");
            let back: ServerMessage = decode_json(&frame).expect("decode");
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn binary_frame_round_trip() {
        let header = ImageUploadHeader {
            g: 5,
            upload_id: 9,
            name: "img.png".to_owned(),
        };
        let body = vec![0u8, 1, 2, 3, 255];
        let frame = encode_binary(&header, &body).expect("encode");
        let (back, raw): (ImageUploadHeader, Vec<u8>) = decode_binary(&frame).expect("decode");
        assert_eq!(back, header);
        assert_eq!(raw, body);
    }

    #[test]
    fn binary_frame_empty_body_round_trip() {
        let header = FileBinHeader {
            g: 1,
            path: "empty.bin".to_owned(),
        };
        let frame = encode_binary(&header, &[]).expect("encode");
        let (back, raw): (FileBinHeader, Vec<u8>) = decode_binary(&frame).expect("decode");
        assert_eq!(back, header);
        assert!(raw.is_empty());
    }

    #[test]
    fn file_chunk_frame_round_trip() {
        let header = FileChunkHeader {
            g: 3,
            path: "image/pic.png".to_owned(),
            offset: 512,
            total: 2048,
        };
        let body = vec![1u8, 2, 3];
        let frame = encode_binary(&header, &body).expect("encode");
        let (back, raw): (FileChunkHeader, Vec<u8>) = decode_binary(&frame).expect("decode");
        assert_eq!(back, header);
        assert_eq!(raw, body);
    }

    #[test]
    fn short_frame_header_is_rejected() {
        let err = decode_frame(&[0, 0, 0]).expect_err("short");
        assert!(matches!(
            err,
            FrameError::ShortBuffer {
                needed: 5,
                available: 3
            }
        ));
    }

    #[test]
    fn declared_length_past_buffer_is_rejected() {
        let err = decode_frame(&[0, 0, 0, 0, 8, 1, 2]).expect_err("short payload");
        assert!(matches!(
            err,
            FrameError::ShortBuffer {
                needed: 13,
                available: 7
            }
        ));
    }

    #[test]
    fn oversize_declared_length_is_rejected() {
        let mut frame = vec![0u8, 0, 0, 0, 0];
        frame[0] = 0;
        frame[1..5].copy_from_slice(&((MAX_FRAME + 1) as u32).to_be_bytes());
        let err = decode_frame(&frame).expect_err("oversize");
        assert!(matches!(err, FrameError::PayloadTooLarge { .. }));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let err = decode_frame(&[7, 0, 0, 0, 0]).expect_err("kind");
        assert!(matches!(err, FrameError::UnknownKind(7)));
    }

    #[test]
    fn wrong_kind_for_json_is_rejected() {
        let frame = encode_binary(
            &FileBinHeader {
                g: 0,
                path: "x".to_owned(),
            },
            &[],
        )
        .expect("encode");
        let err = decode_json::<ServerMessage>(&frame).expect_err("wrong kind");
        assert!(matches!(
            err,
            FrameError::WrongKind {
                expected: 0,
                actual: 1
            }
        ));
    }

    #[test]
    fn truncated_binary_header_is_rejected() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&100u16.to_be_bytes());
        payload.extend_from_slice(b"{}");
        let frame = encode_frame(FrameKind::Binary, &payload).expect("encode");
        let err = decode_binary::<FileBinHeader>(&frame).expect_err("truncated");
        assert!(matches!(
            err,
            FrameError::TruncatedHeader {
                declared: 100,
                available: 2
            }
        ));
    }

    #[test]
    fn invalid_json_payload_is_rejected() {
        let frame = encode_frame(FrameKind::Json, b"{not json").expect("encode");
        let err = decode_json::<ServerMessage>(&frame).expect_err("bad json");
        assert!(matches!(err, FrameError::Json(_)));
    }
}
