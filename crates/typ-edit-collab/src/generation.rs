//! The generation state machine (plan `§5.3`).
//!
//! `G` counts committed change sets. A `Changes` message carries the generation
//! the sender wrote it against; applying it advances `G` by one. Both peers run
//! the receive rules below, but from opposite sides:
//!
//! * [`ClientState`] implements the client's R1 (local optimistic commit), R2
//!   (receive, own-echo confirmation, buffering and the re-sync flow).
//! * [`ServerState`] implements R3 (server receive: apply at the frontier, buffer
//!   ahead of it, reject behind it, then drain its own buffer).
//! * [`validate_change_set`] is R5: the atomic, all-or-nothing validation both
//!   sides run before applying.
//!
//! The machine tracks numbering and buffering only; the caller owns the text and
//! applies the change sets this module hands back, in order.

use crate::error::RejectReason;
use crate::ops::{self, ChangeOp, ChangeSet};
use std::collections::{BTreeMap, BTreeSet};

/// A committed change-set count (the frontier version).
pub type Generation = u64;
/// A per-connection monotonically increasing message id.
pub type MsgId = u64;
/// A session id assigned by the server.
pub type Sid = u64;
/// A per-connection re-sync request id.
pub type ReqId = u64;

/// Cap on buffered out-of-order change sets, per peer state machine.
///
/// A well-behaved peer is at most one optimistic commit ahead, so a handful of
/// buffered sets is the normal maximum; without a cap a sender naming ever newer
/// generations would grow the map (core-thread memory) without limit.
pub const MAX_BUFFERED: usize = 1024;

/// A change set tagged with its origin and per-connection message id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// The peer that produced the change set.
    pub origin: Sid,
    /// That peer's per-connection message id.
    pub msg: MsgId,
    /// The change itself.
    pub set: ChangeSet,
}

/// A locally applied change set awaiting the server's own-echo confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unacked {
    /// The local message id sent with the change set.
    pub msg: MsgId,
    /// The frontier the change set was applied against.
    pub g_base: Generation,
}

/// The result of a local optimistic commit (R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalCommit {
    /// The message id to send with the change set.
    pub msg: MsgId,
    /// The frontier (`g`) the change set was applied against.
    pub g: Generation,
}

/// What [`ClientState::receive`] decided for an inbound change set (R2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientReceive {
    /// Stored for later (ahead of the frontier, or during a re-sync).
    Buffered,
    /// Own echo: an unacked local change is confirmed; nothing is applied.
    Confirmed,
    /// Applied at the frontier; the caller must apply these sets in order.
    /// The first is the received set, the rest drained from the buffer.
    Applied(Vec<Queued>),
    /// Stale (`g < G`) while optimistically ahead; ignored, not reported.
    IgnoredStale,
    /// Stale (`g < G`) with nothing unacked; the origin is out of date.
    PeerStale { about: Sid },
}

/// The client-side generation machine.
#[derive(Clone, Debug)]
pub struct ClientState {
    sid: Sid,
    generation: Generation,
    next_msg: MsgId,
    buffered: BTreeMap<Generation, Queued>,
    unacked: Vec<Unacked>,
    dirty_files: BTreeSet<String>,
    resyncing: bool,
    sync_floor: Generation,
    sync_req: Option<ReqId>,
}

impl ClientState {
    /// A fresh client at generation 0 for session id `sid`.
    pub fn new(sid: Sid) -> Self {
        Self {
            sid,
            generation: 0,
            next_msg: 0,
            buffered: BTreeMap::new(),
            unacked: Vec::new(),
            dirty_files: BTreeSet::new(),
            resyncing: false,
            sync_floor: 0,
            sync_req: None,
        }
    }

    /// This client's session id.
    pub fn sid(&self) -> Sid {
        self.sid
    }

    /// The current frontier.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Adopt `g` as the frontier (used when a join snapshot establishes it).
    pub fn set_generation(&mut self, g: Generation) {
        self.generation = g;
    }

    /// Whether the client is in a re-sync.
    pub fn is_resyncing(&self) -> bool {
        self.resyncing
    }

    /// Files whose changes were discarded or ignored and so need re-syncing.
    pub fn dirty_files(&self) -> &BTreeSet<String> {
        &self.dirty_files
    }

    /// The unconfirmed local change sets.
    pub fn unacked(&self) -> &[Unacked] {
        &self.unacked
    }

    /// Record a local optimistic commit (R1). The caller has already applied the
    /// change to its buffer; this numbers it, advances the frontier and remembers
    /// it as unacked. The caller sends `Changes { g: commit.g, msg: commit.msg }`.
    pub fn local_commit(&mut self) -> LocalCommit {
        let g = self.generation;
        let msg = self.next_msg;
        self.next_msg += 1;
        self.generation += 1;
        self.unacked.push(Unacked { msg, g_base: g });
        LocalCommit { msg, g }
    }

    /// Receive a change set from `origin` (R2).
    ///
    /// During a re-sync every inbound change set is buffered without applying.
    /// Otherwise an own echo confirms a local commit, a set at the frontier is
    /// applied and the buffer is drained, a set ahead is buffered, and a set
    /// behind is discarded (recording the file as dirty in both stale branches).
    pub fn receive(
        &mut self,
        origin: Sid,
        msg: MsgId,
        g: Generation,
        set: ChangeSet,
    ) -> ClientReceive {
        if self.resyncing {
            return self.buffer_client(origin, msg, g, set);
        }
        if origin == self.sid {
            self.unacked.retain(|pending| pending.msg != msg);
            return ClientReceive::Confirmed;
        }
        if g == self.generation {
            let mut applied = Vec::new();
            let mut current = Queued { origin, msg, set };
            loop {
                self.generation += 1;
                applied.push(current);
                match self.buffered.remove(&self.generation) {
                    Some(next) => current = next,
                    None => break,
                }
            }
            return ClientReceive::Applied(applied);
        }
        if g > self.generation {
            return self.buffer_client(origin, msg, g, set);
        }
        self.dirty_files.insert(set.file.clone());
        if self.unacked.is_empty() {
            ClientReceive::PeerStale { about: origin }
        } else {
            ClientReceive::IgnoredStale
        }
    }

    /// Stores an out-of-order set, or reports the peer as stale when the buffer
    /// is full: a peer that far ahead cannot be caught up by buffering, and an
    /// unbounded buffer would let a server it does not control exhaust memory.
    fn buffer_client(
        &mut self,
        origin: Sid,
        msg: MsgId,
        g: Generation,
        set: ChangeSet,
    ) -> ClientReceive {
        if self.buffered.len() >= MAX_BUFFERED && !self.buffered.contains_key(&g) {
            self.dirty_files.insert(set.file.clone());
            return ClientReceive::PeerStale { about: origin };
        }
        self.buffered.insert(g, Queued { origin, msg, set });
        ClientReceive::Buffered
    }

    /// Enter a re-sync (plan `§5.5`), returning `(req, sync_floor)`.
    ///
    /// The floor is the highest unmet base generation, captured **before**
    /// unacked is cleared. A repeat call while already re-syncing is a no-op
    /// (single-flight, F17) and keeps the in-flight request and floor.
    pub fn start_resync(&mut self, req: ReqId) -> (ReqId, Generation) {
        if self.resyncing {
            return (self.sync_req.unwrap_or(req), self.sync_floor);
        }
        self.sync_floor = self
            .unacked
            .iter()
            .map(|pending| pending.g_base)
            .max()
            .unwrap_or(0);
        self.unacked.clear();
        self.sync_req = Some(req);
        self.resyncing = true;
        (req, self.sync_floor)
    }

    /// Whether a `SyncState` should be accepted: it must match the in-flight
    /// request and be at least as new as the captured floor (F15/F16).
    pub fn accept_sync_state(&self, req: ReqId, g: Generation) -> bool {
        self.resyncing && self.sync_req == Some(req) && g >= self.sync_floor
    }

    /// Apply an accepted snapshot at generation `g`, ending the re-sync.
    ///
    /// Clears the dirty set, drops buffered sets older than `g` (their changes
    /// are already in the snapshot), then applies the contiguous run from `g`
    /// upward, returning it in order.
    pub fn finish_resync(&mut self, g: Generation) -> Vec<Queued> {
        self.generation = g;
        self.resyncing = false;
        self.sync_req = None;
        self.dirty_files.clear();
        self.buffered.retain(|queued_g, _| *queued_g >= g);
        let mut applied = Vec::new();
        while let Some(next) = self.buffered.remove(&self.generation) {
            self.generation += 1;
            applied.push(next);
        }
        applied
    }

    /// Files referenced by change sets currently held in the buffer.
    pub fn buffered_files(&self) -> BTreeSet<String> {
        self.buffered
            .values()
            .map(|queued| queued.set.file.clone())
            .collect()
    }

    /// Discard all buffered and unconfirmed state on session termination (F18).
    ///
    /// The frontier is left untouched so a fresh join can start cleanly.
    pub fn abandon(&mut self) {
        self.buffered.clear();
        self.unacked.clear();
        self.dirty_files.clear();
        self.resyncing = false;
        self.sync_req = None;
    }
}

/// What [`ServerState::receive`] decided for an inbound change set (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerDecision {
    /// Sender is pending or re-syncing; ignored (F12).
    Ignored,
    /// Whether `g < G`: reject and tell the origin to re-sync.
    Desync,
    /// `g > G`: stored until the gap fills.
    Buffered,
    /// `g > G` and the buffer is full: the sender is dropped rather than
    /// buffering unbounded generations.
    Overflow,
    /// `g == G`: validate, apply and broadcast, then [`ServerState::advance`].
    Apply,
}

/// The server-side generation machine.
#[derive(Clone, Debug, Default)]
pub struct ServerState {
    generation: Generation,
    buffered: BTreeMap<Generation, Queued>,
}

impl ServerState {
    /// A fresh server at generation 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The authoritative frontier.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// How many out-of-order change sets are buffered.
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Classify an inbound change set. `eligible` is false for senders that are
    /// pending (not yet `Ready`) or re-syncing — those are ignored (F12).
    pub fn receive(&self, g: Generation, eligible: bool) -> ServerDecision {
        if !eligible {
            return ServerDecision::Ignored;
        }
        if g == self.generation {
            ServerDecision::Apply
        } else if g > self.generation {
            // A well-behaved peer is at most one optimistic commit ahead; a
            // sender naming unbounded future generations would otherwise grow
            // the buffer without limit.
            if self.buffered.len() >= MAX_BUFFERED && !self.buffered.contains_key(&g) {
                ServerDecision::Overflow
            } else {
                ServerDecision::Buffered
            }
        } else {
            ServerDecision::Desync
        }
    }

    /// Store an ahead-of-frontier change set (the caller acts on [`ServerDecision::Buffered`]).
    pub fn buffer(&mut self, g: Generation, entry: Queued) {
        self.buffered.insert(g, entry);
    }

    /// The buffered set sitting at the frontier, if any: the next one to commit.
    ///
    /// The set at the frontier itself is always already committed, so the caller
    /// hands this to its `commit_change` and only then calls [`Self::advance`].
    pub fn next_queued(&self) -> Option<Queued> {
        self.buffered.get(&self.generation).cloned()
    }

    /// Steps the frontier past the set that was just committed at it, dropping
    /// that set from the buffer so it is never handed out twice.
    ///
    /// The frontier only ever moves over a change set that applied: stepping it
    /// before (or over) a set that failed validation would put it ahead of every
    /// peer's expectation, and their next change would wait forever for a
    /// generation that never arrives.
    pub fn advance(&mut self) {
        self.buffered.remove(&self.generation);
        self.generation += 1;
    }

    /// Drops the buffered set at the frontier without moving the frontier, for a
    /// set that failed validation: the frontier stays where the peers are, and
    /// the drain is not blocked by a set that can never apply.
    pub fn discard_frontier(&mut self) {
        self.buffered.remove(&self.generation);
    }
}

/// Validate a change set against `content` (R5). See [`crate::ops::validate`].
pub fn validate_change_set(content: &str, ops: &[ChangeOp]) -> Result<(), RejectReason> {
    ops::validate(content, ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::ChangeSet;
    use std::collections::HashMap;

    fn insert(file: &str, at: u64, text: &str) -> ChangeSet {
        ChangeSet::new(
            file,
            vec![ChangeOp::Insert {
                at,
                text: text.to_owned(),
            }],
        )
    }

    #[test]
    fn local_commit_advances_and_records_unacked() {
        let mut client = ClientState::new(1);
        let commit = client.local_commit();
        assert_eq!(commit.msg, 0);
        assert_eq!(commit.g, 0);
        assert_eq!(client.generation(), 1);
        assert_eq!(client.unacked().len(), 1);
        assert_eq!(client.unacked()[0].g_base, 0);
    }

    #[test]
    fn own_echo_confirms_and_removes_unacked() {
        let mut client = ClientState::new(1);
        client.local_commit();
        let outcome = client.receive(1, 0, 0, insert("a.typ", 0, "x"));
        assert_eq!(outcome, ClientReceive::Confirmed);
        assert!(client.unacked().is_empty());
        assert_eq!(client.generation(), 1);
    }

    #[test]
    fn apply_at_frontier_then_drain_across_a_gap() {
        let mut client = ClientState::new(1);
        assert_eq!(
            client.receive(2, 2, 2, insert("a.typ", 0, "c")),
            ClientReceive::Buffered
        );
        assert_eq!(
            client.receive(2, 1, 1, insert("a.typ", 0, "b")),
            ClientReceive::Buffered
        );
        let mut doc = HashMap::new();
        doc.insert("a.typ".to_owned(), String::new());
        match client.receive(2, 0, 0, insert("a.typ", 0, "a")) {
            ClientReceive::Applied(sets) => {
                assert_eq!(sets.len(), 3);
                for queued in &sets {
                    let content = doc.get(&queued.set.file).cloned().unwrap_or_default();
                    let next = ops::apply(&content, &queued.set.ops).expect("valid");
                    doc.insert(queued.set.file.clone(), next);
                }
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(client.generation(), 3);
        assert_eq!(doc["a.typ"], "cba");
    }

    #[test]
    fn stale_with_unacked_is_ignored_but_records_dirty_file() {
        let mut client = ClientState::new(1);
        client.local_commit();
        let outcome = client.receive(2, 0, 0, insert("lost.typ", 0, "y"));
        assert_eq!(outcome, ClientReceive::IgnoredStale);
        assert!(client.dirty_files().contains("lost.typ"));
    }

    #[test]
    fn stale_without_unacked_reports_peer_stale_and_records_dirty_file() {
        let mut client = ClientState::new(1);
        client.receive(2, 0, 0, insert("a.typ", 0, "a"));
        assert_eq!(client.generation(), 1);
        let outcome = client.receive(3, 0, 0, insert("b.typ", 0, "b"));
        assert_eq!(outcome, ClientReceive::PeerStale { about: 3 });
        assert!(client.dirty_files().contains("b.typ"));
        assert!(!client.dirty_files().contains("a.typ"));
    }

    #[test]
    fn resyncing_buffers_every_inbound_change() {
        let mut client = ClientState::new(1);
        client.start_resync(7);
        assert_eq!(
            client.receive(2, 0, 0, insert("a.typ", 0, "a")),
            ClientReceive::Buffered
        );
        assert_eq!(
            client.receive(1, 1, 5, insert("b.typ", 0, "b")),
            ClientReceive::Buffered
        );
        assert_eq!(client.generation(), 0);
        assert!(client.unacked().is_empty());
    }

    #[test]
    fn sync_floor_is_captured_before_unacked_is_cleared() {
        let mut client = ClientState::new(1);
        client.local_commit();
        client.local_commit();
        client.local_commit();
        let (req, floor) = client.start_resync(7);
        assert_eq!(req, 7);
        assert_eq!(floor, 2);
        assert!(client.unacked().is_empty());
    }

    #[test]
    fn sync_state_acceptance_rejects_too_old_or_mismatched() {
        let mut client = ClientState::new(1);
        client.local_commit();
        client.local_commit();
        client.local_commit();
        client.start_resync(7);
        assert!(!client.accept_sync_state(7, 1));
        assert!(!client.accept_sync_state(8, 9));
        assert!(client.accept_sync_state(7, 2));
        assert!(client.accept_sync_state(7, 5));
    }

    #[test]
    fn repeated_resync_is_single_flight() {
        let mut client = ClientState::new(1);
        client.local_commit();
        let (first, floor) = client.start_resync(7);
        let (second, floor_again) = client.start_resync(9);
        assert_eq!(first, second);
        assert_eq!(floor, floor_again);
        assert!(client.accept_sync_state(7, 0));
        assert!(!client.accept_sync_state(9, 0));
    }

    #[test]
    fn finish_resync_drops_older_and_replays_contiguous_buffer() {
        let mut client = ClientState::new(1);
        client.local_commit();
        client.start_resync(7);
        client.receive(2, 5, 5, insert("a.typ", 0, "old"));
        client.receive(2, 6, 6, insert("a.typ", 0, "next"));
        client.receive(2, 7, 8, insert("a.typ", 0, "far"));
        let applied = client.finish_resync(6);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].set, insert("a.typ", 0, "next"));
        assert_eq!(client.generation(), 7);
        assert!(client.dirty_files().is_empty());
        assert_eq!(client.buffered.len(), 1);
        assert!(client.buffered.contains_key(&8));
    }

    #[test]
    fn concurrent_conflict_trace_converges_at_the_state_level() {
        let mut a = ClientState::new(1);
        let mut b = ClientState::new(2);
        for _ in 0..5 {
            a.local_commit();
            b.local_commit();
        }
        for msg in 0..5 {
            a.receive(1, msg, 0, insert("a.typ", 0, "seed"));
            b.receive(2, msg, 0, insert("a.typ", 0, "seed"));
        }
        assert_eq!(a.generation(), 5);
        assert_eq!(b.generation(), 5);
        assert!(a.unacked().is_empty());

        let commit_a = a.local_commit();
        let commit_b = b.local_commit();
        assert_eq!(commit_a.g, 5);
        assert_eq!(commit_b.g, 5);
        assert_eq!(a.generation(), 6);
        assert_eq!(b.generation(), 6);

        let mut server = ServerState::new();
        for _ in 0..5 {
            assert_eq!(
                server.receive(server.generation(), true),
                ServerDecision::Apply
            );
            server.advance();
        }
        assert_eq!(server.generation(), 5);

        assert_eq!(server.receive(5, true), ServerDecision::Apply);
        server.advance();
        assert_eq!(server.receive(5, true), ServerDecision::Desync);

        assert_eq!(
            a.receive(1, commit_a.msg, 5, insert("a.typ", 0, "A")),
            ClientReceive::Confirmed
        );
        assert_eq!(
            b.receive(1, commit_a.msg, 5, insert("a.typ", 0, "A")),
            ClientReceive::IgnoredStale
        );
        assert!(b.dirty_files().contains("a.typ"));

        let (_req, floor) = b.start_resync(1);
        assert_eq!(floor, 5);
        assert!(b.accept_sync_state(1, 6));
        let applied = b.finish_resync(6);
        assert!(applied.is_empty());
        assert_eq!(a.generation(), 6);
        assert_eq!(b.generation(), 6);
        assert_eq!(server.generation(), 6);
    }

    #[test]
    fn server_applies_at_frontier_and_drains_buffer() {
        let mut server = ServerState::new();
        server.buffer(
            1,
            Queued {
                origin: 2,
                msg: 9,
                set: insert("a.typ", 0, "b"),
            },
        );
        assert_eq!(server.buffered_len(), 1);
        assert_eq!(server.receive(0, true), ServerDecision::Apply);
        // The set at the frontier (0) is committed: step past it, then hand the
        // buffered set now sitting at the frontier (1) to the caller.
        server.advance();
        assert_eq!(server.generation(), 1);
        let queued = server.next_queued().expect("buffered at the frontier");
        assert_eq!(queued.origin, 2);
        server.advance();
        assert_eq!(server.generation(), 2);
        assert!(server.next_queued().is_none());
    }

    #[test]
    fn server_frontier_does_not_move_over_a_rejected_buffered_set() {
        let mut server = ServerState::new();
        server.buffer(
            1,
            Queued {
                origin: 2,
                msg: 9,
                set: insert("a.typ", 0, "b"),
            },
        );
        assert_eq!(server.receive(0, true), ServerDecision::Apply);
        server.advance();
        // The buffered set failed validation: it is dropped, but the frontier
        // stays where the peers are (1), so their next change is not stranded.
        server.discard_frontier();
        assert_eq!(server.generation(), 1);
        assert!(server.next_queued().is_none());
        assert_eq!(server.buffered_len(), 0);
    }

    #[test]
    fn server_reports_overflow_instead_of_buffering_without_bound() {
        let mut server = ServerState::new();
        for g in 1..=MAX_BUFFERED as u64 {
            assert_eq!(server.receive(g, true), ServerDecision::Buffered);
            server.buffer(
                g,
                Queued {
                    origin: 2,
                    msg: g,
                    set: insert("a.typ", 0, "b"),
                },
            );
        }
        assert_eq!(server.buffered_len(), MAX_BUFFERED);
        // One more distinct future generation is refused...
        assert_eq!(
            server.receive(MAX_BUFFERED as u64 + 1, true),
            ServerDecision::Overflow
        );
        // ...while re-sending one already buffered stays accepted.
        assert_eq!(server.receive(3, true), ServerDecision::Buffered);
    }

    #[test]
    fn server_rejects_stale_and_ignores_ineligible() {
        let mut server = ServerState::new();
        server.advance();
        assert_eq!(server.receive(0, true), ServerDecision::Desync);
        assert_eq!(server.receive(0, false), ServerDecision::Ignored);
        assert_eq!(server.receive(5, false), ServerDecision::Ignored);
        assert_eq!(server.receive(5, true), ServerDecision::Buffered);
    }

    #[test]
    fn r5_multi_op_reject_is_atomic() {
        let content = "hello";
        let ops = vec![
            ChangeOp::Insert {
                at: 5,
                text: " world".to_owned(),
            },
            ChangeOp::Insert {
                at: 99,
                text: "!".to_owned(),
            },
        ];
        assert!(validate_change_set(content, &ops).is_err());
        assert_eq!(
            ops::apply(content, &ops),
            Err(RejectReason::OutOfBounds { at: 99, len: 11 })
        );
        assert_eq!(content, "hello");
    }

    #[test]
    fn applied_sets_carry_origin_and_message_id() {
        let mut client = ClientState::new(1);
        match client.receive(9, 4, 0, insert("a.typ", 0, "a")) {
            ClientReceive::Applied(sets) => {
                assert_eq!(sets.len(), 1);
                assert_eq!(sets[0].origin, 9);
                assert_eq!(sets[0].msg, 4);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(client.generation(), 1);
    }
}
