use std::fmt;

/// Why a change set was rejected by [`crate::ops::validate`].
///
/// Rejection is all-or-nothing: a change set that reports any of these is never
/// partially applied, so callers can treat a rejection as "the document has
/// diverged" and resynchronise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// A byte offset, or the span it implies, lies outside the text.
    OutOfBounds { at: u64, len: u64 },
    /// A byte offset does not fall on a UTF-8 character boundary.
    NotCharBoundary { at: u64 },
    /// A `Delete` op's recorded text does not match the text at its span.
    DeleteTextMismatch { at: u64 },
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { at, len } => {
                write!(f, "offset {at} out of bounds for text of length {len}")
            }
            Self::NotCharBoundary { at } => {
                write!(f, "offset {at} is not a UTF-8 character boundary")
            }
            Self::DeleteTextMismatch { at } => {
                write!(f, "delete text at {at} does not match the document")
            }
        }
    }
}

impl std::error::Error for RejectReason {}

/// Errors from the wire frame codec in [`crate::protocol`].
#[derive(Debug)]
pub enum FrameError {
    /// The encoded payload exceeds [`crate::protocol::MAX_FRAME`].
    PayloadTooLarge { len: usize, max: usize },
    /// A binary frame's JSON header is longer than the `u16` length prefix allows.
    HeaderTooLong { len: usize },
    /// A binary frame's declared header length exceeds the bytes available.
    TruncatedHeader { declared: usize, available: usize },
    /// The buffer is shorter than the frame header or the declared payload.
    ShortBuffer { needed: usize, available: usize },
    /// The frame kind byte is not a known value.
    UnknownKind(u8),
    /// The payload is not valid JSON for the expected message.
    Json(serde_json::Error),
    /// A message was decoded from a frame kind it does not use.
    WrongKind { expected: u8, actual: u8 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLarge { len, max } => {
                write!(
                    f,
                    "payload of {len} bytes exceeds the {max}-byte frame limit"
                )
            }
            Self::HeaderTooLong { len } => write!(f, "binary header of {len} bytes exceeds u16"),
            Self::TruncatedHeader {
                declared,
                available,
            } => write!(
                f,
                "binary header declares {declared} bytes but only {available} are present"
            ),
            Self::ShortBuffer { needed, available } => {
                write!(
                    f,
                    "frame needs {needed} bytes but only {available} are present"
                )
            }
            Self::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
            Self::Json(error) => write!(f, "invalid JSON payload: {error}"),
            Self::WrongKind { expected, actual } => {
                write!(f, "expected frame kind {expected} but got kind {actual}")
            }
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Errors from the crypto layer in [`crate::crypto`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// Salt or challenge bytes could not be produced from the OS RNG.
    Randomness,
    /// The supplied key could not initialise the MAC (never happens for HMAC).
    InvalidKey,
    /// AEAD sealing failed.
    Seal,
    /// AEAD opening failed: the ciphertext or tag did not authenticate.
    Open,
    /// The per-direction nonce counter is exhausted; the session must end.
    NonceExhausted,
    /// A sealed payload is too short to hold a nonce and an authentication tag.
    ShortPayload { len: usize },
    /// The payload's direction tag did not match the inbound direction.
    WrongDirection,
    /// The payload's counter did not match the expected inbound counter.
    UnexpectedNonce { expected: u64, found: u64 },
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Randomness => write!(f, "OS randomness unavailable"),
            Self::InvalidKey => write!(f, "invalid MAC key"),
            Self::Seal => write!(f, "failed to seal payload"),
            Self::Open => write!(f, "failed to open payload (authentication failed)"),
            Self::NonceExhausted => write!(f, "session nonce counter exhausted"),
            Self::ShortPayload { len } => write!(f, "sealed payload of {len} bytes is too short"),
            Self::WrongDirection => write!(f, "payload was sealed for the other direction"),
            Self::UnexpectedNonce { expected, found } => {
                write!(f, "expected nonce counter {expected} but got {found}")
            }
        }
    }
}

impl std::error::Error for CryptoError {}

/// Errors from a live session's frame transport (encode / decode / seal / open).
#[derive(Debug)]
pub enum WireError {
    /// The underlying socket failed.
    Io(std::io::Error),
    /// The frame bytes were malformed.
    Frame(FrameError),
    /// The AEAD seal/open failed.
    Crypto(CryptoError),
    /// A payload was not valid JSON for the expected message.
    Json(serde_json::Error),
    /// The peer sent a message that is not valid at this point in the session.
    UnexpectedMessage,
    /// The connection closed before a complete frame arrived.
    Closed,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "socket error: {error}"),
            Self::Frame(error) => write!(f, "frame error: {error}"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
            Self::Json(error) => write!(f, "invalid JSON payload: {error}"),
            Self::UnexpectedMessage => write!(f, "unexpected message"),
            Self::Closed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Crypto(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WireError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for WireError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<CryptoError> for WireError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

impl From<serde_json::Error> for WireError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Errors from starting or connecting a collaboration session.
#[derive(Debug)]
pub enum ClientError {
    /// The transport or handshake failed.
    Wire(WireError),
    /// No candidate address accepted the connection.
    Unreachable,
    /// A TCP connection was accepted, but the server did not complete the
    /// handshake before the deadline (for example a large startup scan or a
    /// long join/resync snapshot is still holding the server core).
    HandshakeTimeout,
    /// The server requires a password but none was supplied.
    PasswordRequired,
    /// The server's challenge-response proof did not verify.
    ServerProofFailed,
    /// The server rejected the join.
    Rejected(crate::protocol::RejectKind),
    /// The server's first message was not a `Hello`.
    Handshake,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wire(error) => write!(f, "session error: {error}"),
            Self::Unreachable => write!(f, "no server address was reachable"),
            Self::HandshakeTimeout => write!(f, "the server did not respond in time"),
            Self::PasswordRequired => write!(f, "the server requires a password"),
            Self::ServerProofFailed => write!(f, "the server's identity proof failed"),
            Self::Rejected(kind) => write!(f, "the server rejected the join: {kind:?}"),
            Self::Handshake => write!(f, "the server sent an unexpected handshake message"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wire(error) => Some(error),
            _ => None,
        }
    }
}

impl From<WireError> for ClientError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

impl From<std::io::Error> for ClientError {
    fn from(error: std::io::Error) -> Self {
        Self::Wire(WireError::Io(error))
    }
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        Self::Wire(WireError::Frame(error))
    }
}

/// Errors from starting or operating the collaboration server.
#[derive(Debug)]
pub enum ServerError {
    /// No port in the requested `port..port+10` range could be bound.
    NoPort { start: u16 },
    /// The listener socket failed.
    Io(std::io::Error),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPort { start } => {
                write!(f, "no free port in {start}..{}", start.saturating_add(9))
            }
            Self::Io(error) => write!(f, "listener error: {error}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Errors from mDNS service registration or browsing.
#[derive(Debug)]
pub enum DiscoveryError {
    /// The underlying `mdns-sd` daemon failed.
    Mdns(mdns_sd::Error),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mdns(error) => write!(f, "mdns error: {error}"),
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mdns(error) => Some(error),
        }
    }
}

impl From<mdns_sd::Error> for DiscoveryError {
    fn from(error: mdns_sd::Error) -> Self {
        Self::Mdns(error)
    }
}
