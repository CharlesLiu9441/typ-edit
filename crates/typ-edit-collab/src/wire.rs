//! Byte-level frame transport shared by the server and the client.
//!
//! Reader and writer threads only move opaque `[kind][len][payload]` frames.
//! Sealing and opening happen on each peer's single serialization point (the
//! server's core thread, the client's core thread), never on an I/O thread, so
//! the AEAD nonce counters advance in one place. This module holds the small
//! `Read`/`Write` helpers and the seal/open frame wrappers.

use crate::crypto::SessionCipher;
use crate::error::WireError;
use crate::protocol::{self, FRAME_HEADER_LEN, FrameKind, MAX_FRAME};
use std::io::{self, Read, Write};

/// Read one complete frame (5-byte header plus payload) from `reader`.
pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length exceeds MAX_FRAME",
        ));
    }
    let mut frame = vec![0u8; FRAME_HEADER_LEN + len];
    frame[..FRAME_HEADER_LEN].copy_from_slice(&header);
    reader.read_exact(&mut frame[FRAME_HEADER_LEN..])?;
    Ok(frame)
}

/// Write one complete frame and flush it.
pub(crate) fn write_frame(writer: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    writer.write_all(frame)?;
    writer.flush()
}

/// Frame `payload`, sealing it when the session is encrypted.
pub(crate) fn frame_with_cipher(
    kind: FrameKind,
    payload: &[u8],
    cipher: Option<&mut SessionCipher>,
) -> Result<Vec<u8>, WireError> {
    match cipher {
        Some(cipher) => Ok(protocol::encode_frame(kind, &cipher.seal(payload)?)?),
        None => Ok(protocol::encode_frame(kind, payload)?),
    }
}

/// Parse a received frame, opening its payload when the session is encrypted.
pub(crate) fn parse_inbound(
    frame: &[u8],
    cipher: Option<&mut SessionCipher>,
) -> Result<(FrameKind, Vec<u8>), WireError> {
    let decoded = protocol::decode_frame(frame)?;
    match cipher {
        Some(cipher) => Ok((decoded.kind, cipher.open(decoded.payload)?)),
        None => Ok((decoded.kind, decoded.payload.to_vec())),
    }
}
