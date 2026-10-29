//! Password-derived encryption for a collaboration session (plan `§5.7`).
//!
//! The password key is `PBKDF2-HMAC-SHA256(password, salt, [`KDF_ITERS`], 32
//! bytes)`. Mutual authentication uses HMAC-SHA256 over `challenge ‖ <role> ‖
//! user`, with distinct `"c"`/`"s"` role bytes so a client proof can never be
//! replayed as a server proof. Established traffic is sealed with
//! ChaCha20-Poly1305 using a deterministic nonce of `[4B direction tag][8B
//! per-direction counter]`, which makes nonce reuse impossible within a
//! direction and detects reordering.
//!
//! The AEAD key of a connection is [`derive_session_key`]: the password key is
//! shared by every connection of one server, while both the key and the nonce
//! counter would restart at zero per connection — so each connection extracts
//! its own key from its random challenge (HKDF-SHA256).

use crate::error::CryptoError;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Salt length in bytes.
pub const SALT_LEN: usize = 16;
/// Derived key length in bytes.
pub const KEY_LEN: usize = 32;
/// PBKDF2 iteration count (constant, advertised in `Hello`).
pub const KDF_ITERS: u32 = 200_000;
/// Per-connection challenge length in bytes.
pub const CHALLENGE_LEN: usize = 16;
/// AEAD nonce length in bytes.
pub const NONCE_LEN: usize = 12;
/// AEAD authentication tag length in bytes.
pub const TAG_LEN: usize = 16;

/// Direction tag for client → server payloads.
const DIR_CLIENT_TO_SERVER: [u8; 4] = [0, 0, 0, 1];
/// Direction tag for server → client payloads.
const DIR_SERVER_TO_CLIENT: [u8; 4] = [0, 0, 0, 2];

/// The role a peer plays, which fixes its send/receive direction tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRole {
    Client,
    Server,
}

/// SHA-256 digest length in bytes, the PRK length of [`hkdf_sha256`].
const SHA256_LEN: usize = 32;

/// HKDF `info` for the session key: a domain separator so the derived AEAD key
/// can never collide with an authentication proof.
const SESSION_KEY_INFO: &[u8] = b"typ-edit/collab/session-key/v2";

/// Derive the 32-byte **password key** from `password`, `salt` and `iters`
/// (PBKDF2-HMAC-SHA256).
///
/// Every connection of one server derives the same password key; the
/// per-connection AEAD key is [`derive_session_key`].
///
/// # Panics
///
/// Never: PBKDF2 accepts any key length.
pub fn derive_key(password: &str, salt: &[u8], iters: u32) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iters, &mut key);
    key
}

/// HMAC-SHA256 keyed by `key`. Infallible: HMAC accepts a key of any length.
fn hmac(key: &[u8]) -> Hmac<Sha256> {
    new_mac(key).expect("HMAC accepts a key of any length")
}

/// HKDF-SHA256 (RFC 5869): extract-then-expand `len` bytes of output keying
/// material from `ikm` under `salt` and `info`.
///
/// A zero-length `salt` means a `HashLen`-byte zero salt (RFC 5869 §2.2), and an
/// empty `info` is allowed.
fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    // Extract: PRK = HMAC-Hash(salt, IKM).
    let zeros = [0u8; SHA256_LEN];
    let salt = if salt.is_empty() { &zeros[..] } else { salt };
    let mut prk = [0u8; SHA256_LEN];
    let mut extract = hmac(salt);
    extract.update(ikm);
    prk.copy_from_slice(&extract.finalize().into_bytes());

    // Expand: T(n) = HMAC-Hash(PRK, T(n-1) | info | n); OKM = first `len` bytes.
    let mut okm = Vec::with_capacity(len + SHA256_LEN);
    let mut block: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while okm.len() < len {
        let mut expand = hmac(&prk);
        expand.update(&block);
        expand.update(info);
        expand.update(&[counter]);
        block = expand.finalize().into_bytes().to_vec();
        okm.extend_from_slice(&block);
        counter += 1;
    }
    okm.truncate(len);
    okm
}

/// Derive the per-connection AEAD session key from the password key and that
/// connection's challenge (HKDF-SHA256).
///
/// [`derive_key`] depends only on the password and the server-wide salt, so it is
/// the same for every connection of a server; [`SessionCipher`]'s nonce is
/// `[direction tag][counter]` starting at zero. Sealing with the shared password
/// key would therefore reuse `(key, nonce)` pairs across connections — two
/// sockets would put their first frame under the same pair, which breaks
/// ChaCha20-Poly1305 completely (the XOR of two ciphertexts reveals the XOR of
/// the plaintexts, and the Poly1305 one-time key can be recovered to forge
/// frames). Extracting with the per-connection challenge gives every connection
/// its own key. Both peers compute it from the same challenge, so it needs no
/// extra wire field.
pub fn derive_session_key(password_key: &[u8; KEY_LEN], challenge: &[u8]) -> [u8; KEY_LEN] {
    let okm = hkdf_sha256(password_key, challenge, SESSION_KEY_INFO, KEY_LEN);
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&okm);
    key
}

/// The client's proof: `HMAC(key, challenge ‖ "c" ‖ user)`.
pub fn client_proof(key: &[u8], challenge: &[u8], user: &str) -> Result<[u8; 32], CryptoError> {
    proof(key, b'c', challenge, user)
}

/// The server's proof: `HMAC(key, challenge ‖ "s" ‖ user)`.
pub fn server_proof(key: &[u8], challenge: &[u8], user: &str) -> Result<[u8; 32], CryptoError> {
    proof(key, b's', challenge, user)
}

/// Constant-time verification of a client proof.
pub fn verify_client_proof(key: &[u8], challenge: &[u8], user: &str, proof: &[u8]) -> bool {
    verify(key, b'c', challenge, user, proof)
}

/// Constant-time verification of a server proof.
pub fn verify_server_proof(key: &[u8], challenge: &[u8], user: &str, proof: &[u8]) -> bool {
    verify(key, b's', challenge, user, proof)
}

fn proof(key: &[u8], role: u8, challenge: &[u8], user: &str) -> Result<[u8; 32], CryptoError> {
    let mut mac = new_mac(key)?;
    mac.update(challenge);
    mac.update(&[role]);
    mac.update(user.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn verify(key: &[u8], role: u8, challenge: &[u8], user: &str, expected: &[u8]) -> bool {
    let Ok(mut mac) = new_mac(key) else {
        return false;
    };
    mac.update(challenge);
    mac.update(&[role]);
    mac.update(user.as_bytes());
    mac.verify_slice(expected).is_ok()
}

fn new_mac(key: &[u8]) -> Result<Hmac<Sha256>, CryptoError> {
    <Hmac<Sha256> as KeyInit>::new_from_slice(key).map_err(|_| CryptoError::InvalidKey)
}

/// A random 16-byte salt for the KDF.
pub fn random_salt() -> Result<[u8; SALT_LEN], CryptoError> {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| CryptoError::Randomness)?;
    Ok(salt)
}

/// A random per-connection 16-byte challenge.
pub fn random_challenge() -> Result<[u8; CHALLENGE_LEN], CryptoError> {
    let mut challenge = [0u8; CHALLENGE_LEN];
    getrandom::fill(&mut challenge).map_err(|_| CryptoError::Randomness)?;
    Ok(challenge)
}

/// An established AEAD session for one direction pair.
pub struct SessionCipher {
    cipher: ChaCha20Poly1305,
    send_tag: [u8; 4],
    recv_tag: [u8; 4],
    send_counter: u64,
    recv_counter: u64,
}

impl SessionCipher {
    /// Create a session for `role` under `key`.
    pub fn new(key: &[u8; KEY_LEN], role: SessionRole) -> Self {
        let (send_tag, recv_tag) = match role {
            SessionRole::Client => (DIR_CLIENT_TO_SERVER, DIR_SERVER_TO_CLIENT),
            SessionRole::Server => (DIR_SERVER_TO_CLIENT, DIR_CLIENT_TO_SERVER),
        };
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            send_tag,
            recv_tag,
            send_counter: 0,
            recv_counter: 0,
        }
    }

    /// Seal `plaintext`, returning `[12B nonce][ciphertext ‖ tag]`.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.send_counter == u64::MAX {
            return Err(CryptoError::NonceExhausted);
        }
        let nonce = nonce_bytes(self.send_tag, self.send_counter);
        let ciphertext = self
            .cipher
            .encrypt(&Nonce::from(nonce), plaintext)
            .map_err(|_| CryptoError::Seal)?;
        self.send_counter += 1;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Open a payload produced by [`Self::seal`] on the peer.
    ///
    /// The direction tag and counter are checked before decryption, so replayed
    /// or reordered payloads fail closed and the receive counter only advances
    /// on success.
    pub fn open(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if payload.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::ShortPayload { len: payload.len() });
        }
        let (nonce, ciphertext) = payload.split_at(NONCE_LEN);
        if nonce[0..4] != self.recv_tag {
            return Err(CryptoError::WrongDirection);
        }
        let mut counter_bytes = [0u8; 8];
        counter_bytes.copy_from_slice(&nonce[4..12]);
        let found = u64::from_be_bytes(counter_bytes);
        if found != self.recv_counter {
            return Err(CryptoError::UnexpectedNonce {
                expected: self.recv_counter,
                found,
            });
        }
        let mut nonce_array = [0u8; NONCE_LEN];
        nonce_array.copy_from_slice(nonce);
        let plaintext = self
            .cipher
            .decrypt(&Nonce::from(nonce_array), ciphertext)
            .map_err(|_| CryptoError::Open)?;
        self.recv_counter += 1;
        Ok(plaintext)
    }

    /// The number of payloads sealed so far on this session's send direction.
    pub fn send_counter(&self) -> u64 {
        self.send_counter
    }

    /// The number of payloads opened so far on this session's receive direction.
    pub fn recv_counter(&self) -> u64 {
        self.recv_counter
    }
}

fn nonce_bytes(tag: [u8; 4], counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[0..4].copy_from_slice(&tag);
    nonce[4..12].copy_from_slice(&counter.to_be_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a hex string (RFC 5869 vectors are written in hex).
    fn unhex(hex: &str) -> Vec<u8> {
        assert!(
            hex.len().is_multiple_of(2),
            "hex length must be even: {hex}"
        );
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit"))
            .collect()
    }

    fn key() -> [u8; KEY_LEN] {
        derive_key(
            "correct horse battery staple",
            b"salty-salty-salt",
            KDF_ITERS,
        )
    }

    #[test]
    fn kdf_is_deterministic_and_key_sized() {
        let a = derive_key("pw", b"0123456789abcdef", 1000);
        let b = derive_key("pw", b"0123456789abcdef", 1000);
        assert_eq!(a, b);
        assert_eq!(a.len(), KEY_LEN);
    }

    #[test]
    fn kdf_changes_with_salt_and_password() {
        let base = derive_key("pw", b"0123456789abcdef", 1000);
        assert_ne!(base, derive_key("pw", b"fedcba9876543210", 1000));
        assert_ne!(base, derive_key("px", b"0123456789abcdef", 1000));
    }

    #[test]
    fn hmac_proofs_accept_and_reject_both_directions() {
        let k = key();
        let challenge = b"sixteen-byte-chg";
        let client = client_proof(&k, challenge, "ann").expect("client proof");
        let server = server_proof(&k, challenge, "ann").expect("server proof");

        assert!(verify_client_proof(&k, challenge, "ann", &client));
        assert!(verify_server_proof(&k, challenge, "ann", &server));

        assert!(!verify_client_proof(&k, challenge, "bob", &client));
        assert!(!verify_client_proof(
            &k,
            b"different-challe",
            "ann",
            &client
        ));
        assert!(!verify_server_proof(&k, challenge, "ann", &client));
    }

    #[test]
    fn client_and_server_proofs_differ_by_domain_separation() {
        let k = key();
        let challenge = b"sixteen-byte-chg";
        assert_ne!(
            client_proof(&k, challenge, "ann").expect("c"),
            server_proof(&k, challenge, "ann").expect("s")
        );
    }

    #[test]
    fn aead_round_trip_across_roles() {
        let k = key();
        let mut client = SessionCipher::new(&k, SessionRole::Client);
        let mut server = SessionCipher::new(&k, SessionRole::Server);

        let sealed = client.seal(b"hello").expect("seal");
        assert_eq!(server.open(&sealed).expect("open"), b"hello");

        let sealed = server.seal(b"world").expect("seal");
        assert_eq!(client.open(&sealed).expect("open"), b"world");
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let k = key();
        let mut client = SessionCipher::new(&k, SessionRole::Client);
        let mut server = SessionCipher::new(&k, SessionRole::Server);
        let mut sealed = client.seal(b"hello").expect("seal");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert_eq!(server.open(&sealed), Err(CryptoError::Open));
    }

    #[test]
    fn short_payload_fails_closed() {
        let k = key();
        let mut server = SessionCipher::new(&k, SessionRole::Server);
        assert!(matches!(
            server.open(&[0u8; 8]),
            Err(CryptoError::ShortPayload { len: 8 })
        ));
    }

    #[test]
    fn wrong_direction_is_rejected() {
        let k = key();
        let mut client = SessionCipher::new(&k, SessionRole::Client);
        let mut other_client = SessionCipher::new(&k, SessionRole::Client);
        let sealed = client.seal(b"hello").expect("seal");
        assert_eq!(other_client.open(&sealed), Err(CryptoError::WrongDirection));
    }

    #[test]
    fn replayed_payload_is_rejected() {
        let k = key();
        let mut client = SessionCipher::new(&k, SessionRole::Client);
        let mut server = SessionCipher::new(&k, SessionRole::Server);
        let sealed = client.seal(b"hello").expect("seal");
        assert!(server.open(&sealed).is_ok());
        assert!(matches!(
            server.open(&sealed),
            Err(CryptoError::UnexpectedNonce { found: 0, .. })
        ));
    }

    #[test]
    fn nonce_counters_never_repeat_per_direction() {
        let k = key();
        let mut client = SessionCipher::new(&k, SessionRole::Client);
        let mut seen = std::collections::HashSet::new();
        for expected in 0..256u64 {
            let sealed = client.seal(b"x").expect("seal");
            let nonce: [u8; NONCE_LEN] = sealed[..NONCE_LEN].try_into().expect("nonce");
            assert!(seen.insert(nonce), "nonce repeated at counter {expected}");
            assert_eq!(
                u64::from_be_bytes(nonce[4..12].try_into().expect("ctr")),
                expected
            );
            assert_eq!(&nonce[0..4], &DIR_CLIENT_TO_SERVER);
        }
        assert_eq!(client.send_counter(), 256);
    }

    #[test]
    fn random_salt_and_challenge_have_expected_length() {
        let salt = random_salt().expect("salt");
        let challenge = random_challenge().expect("challenge");
        assert_eq!(salt.len(), SALT_LEN);
        assert_eq!(challenge.len(), CHALLENGE_LEN);
    }

    /// The extract/expand implementation must reproduce the published RFC 5869
    /// vectors byte for byte: A.1 (basic), A.2 (80-byte inputs, multi-block
    /// expand) and A.3 (zero-length salt and info, the empty-salt branch).
    #[test]
    fn hkdf_sha256_matches_rfc5869_sha256_vectors() {
        assert_eq!(
            hkdf_sha256(
                &[0x0b; 22],
                &unhex("000102030405060708090a0b0c"),
                &unhex("f0f1f2f3f4f5f6f7f8f9"),
                42
            ),
            unhex(concat!(
                "3cb25f25faacd57a90434f64d0362f2a",
                "2d2d0a90cf1a5a4c5db02d56ecc4c5bf",
                "34007208d5b887185865"
            )),
            "RFC 5869 A.1"
        );

        let long: Vec<u8> = (0x00..0x50u8).collect();
        let long_salt: Vec<u8> = (0x60..0xb0u8).collect();
        let long_info: Vec<u8> = (0xb0..=0xffu8).collect();
        assert_eq!(
            hkdf_sha256(&long, &long_salt, &long_info, 82),
            unhex(concat!(
                "b11e398dc80327a1c8e7f78c596a4934",
                "4f012eda2d4efad8a050cc4c19afa97c",
                "59045a99cac7827271cb41c65e590e09",
                "da3275600c2f09b8367793a9aca3db71",
                "cc30c58179ec3e87c14c01d5c1f3434f",
                "1d87"
            )),
            "RFC 5869 A.2"
        );

        assert_eq!(
            hkdf_sha256(&[0x0b; 22], &[], &[], 42),
            unhex(concat!(
                "8da4e775a563c18f715f802a063c5a31",
                "b8a11f5c5ee1879ec3454e5f3c738d2d",
                "9d201395faa4b61a96c8"
            )),
            "RFC 5869 A.3 (empty salt and info)"
        );
    }

    #[test]
    fn session_key_is_per_challenge_and_deterministic() {
        let password_key = key();
        let first = derive_session_key(&password_key, b"challenge-conn-1");
        let second = derive_session_key(&password_key, b"challenge-conn-2");
        assert_ne!(first, second, "each connection must get its own key");
        // Both peers derive the same key from the same challenge...
        assert_eq!(
            first,
            derive_session_key(&password_key, b"challenge-conn-1")
        );
        // ...and the session key is not the password key itself.
        assert_ne!(first, password_key);
    }

    /// The bug this fix pins: two connections of one server used to seal their
    /// first frame under the same `(key, nonce)` pair.
    #[test]
    fn two_connections_never_share_a_sealed_frame() {
        let password_key = key();
        let mut server_a = SessionCipher::new(
            &derive_session_key(&password_key, b"challenge-conn-1"),
            SessionRole::Server,
        );
        let sealed = server_a.seal(b"hello from connection 1").expect("seal");

        // Connection 2's peer has the right direction tag and a fresh counter,
        // so only the key stops it from opening connection 1's frame.
        let mut client_b = SessionCipher::new(
            &derive_session_key(&password_key, b"challenge-conn-2"),
            SessionRole::Client,
        );
        assert!(client_b.open(&sealed).is_err());

        // Same plaintext and same nonce, different key: the frames must differ,
        // so no keystream is reused across connections.
        let mut server_b = SessionCipher::new(
            &derive_session_key(&password_key, b"challenge-conn-2"),
            SessionRole::Server,
        );
        let other = server_b.seal(b"hello from connection 1").expect("seal");
        assert_ne!(sealed, other);
    }
}
