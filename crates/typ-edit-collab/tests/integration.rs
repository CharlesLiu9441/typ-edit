//! Loopback integration suite for the collaboration session layer.
//!
//! Every test runs a real `CollabServer` on `127.0.0.1` over an ephemeral port
//! and a throwaway temp project directory. Waits are deadline-driven (no fixed
//! sleeps): the harness pumps client events and polls a predicate until it holds
//! or the deadline passes.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use typ_edit_collab::{
    Advertiser, Browser, ChangeOp, ChangeSet, ClientError, ClientEvent, CollabClient, CollabServer,
    DiscoveryEvent, HistoryOp, RejectKind, ServerConfig, ServerEvent, Sid, UserInfo, apply,
};
use typ_edit_model::TextSnapshot;

/// Generous per-step deadline for event-driven waits.
const DEADLINE: Duration = Duration::from_secs(10);

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = env::temp_dir().join(format!(
        "typ-edit-collab-it-{}-{seq}-{nanos}-{tag}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp root");
    dir
}

fn write_rel(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, text).expect("write file");
}

fn start_server(root: &Path, project: &str, password: Option<&str>) -> CollabServer {
    start_server_with(root, project, password, 16, Duration::from_secs(30))
}

fn start_server_with(
    root: &Path,
    project: &str,
    password: Option<&str>,
    max_connections: usize,
    ready_timeout: Duration,
) -> CollabServer {
    let config = ServerConfig {
        root: root.to_path_buf(),
        project: project.to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: root.to_path_buf(),
        max_connections,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout,
        initial_docs: Vec::new(),
    };
    CollabServer::start(config, password.map(str::to_owned)).expect("start server")
}

fn addr(server: &CollabServer) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], server.port()))
}

type CursorView = (Sid, String, Vec<u64>, Vec<(u64, u64)>);

/// A headless editor peer: the client plus the in-memory mirror it fills from the
/// client's content events.
struct Peer {
    client: CollabClient,
    docs: BTreeMap<String, String>,
    bins: BTreeMap<String, Vec<u8>>,
    users: Vec<UserInfo>,
    cursor: Option<CursorView>,
    events: Vec<ClientEvent>,
}

impl Peer {
    fn new(client: CollabClient) -> Self {
        Self {
            client,
            docs: BTreeMap::new(),
            bins: BTreeMap::new(),
            users: Vec::new(),
            cursor: None,
            events: Vec::new(),
        }
    }

    fn handle(&mut self, event: ClientEvent) {
        match &event {
            ClientEvent::Changes { set, .. } => self.apply_set(set),
            ClientEvent::SyncState { files, applied, .. } => {
                for file in files {
                    self.docs.insert(file.path.clone(), file.text.clone());
                }
                for set in applied {
                    self.apply_set(set);
                }
            }
            ClientEvent::FileText { path, text } => {
                self.docs.insert(path.clone(), text.clone());
            }
            ClientEvent::FileBin { path, bytes } => {
                self.bins.insert(path.clone(), bytes.clone());
            }
            ClientEvent::Users { list } => self.users = list.clone(),
            ClientEvent::Cursors {
                sid,
                file,
                carets,
                selections,
                ..
            } => self.cursor = Some((*sid, file.clone(), carets.clone(), selections.clone())),
            _ => {}
        }
        self.events.push(event);
    }

    fn apply_set(&mut self, set: &ChangeSet) {
        let content = self.docs.get(&set.file).cloned().unwrap_or_default();
        let next = apply(&content, &set.ops).expect("peer applies a valid change set");
        self.docs.insert(set.file.clone(), next);
    }

    fn pump(&mut self) {
        while let Some(event) = self.client.try_recv_event() {
            self.handle(event);
        }
    }

    fn wait_for(&mut self, deadline: Duration, predicate: impl Fn(&Self) -> bool) -> bool {
        let end = Instant::now() + deadline;
        loop {
            self.pump();
            if predicate(self) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn saw(&self, predicate: impl Fn(&ClientEvent) -> bool) -> bool {
        self.events.iter().any(predicate)
    }

    fn saw_since(&self, from: usize, predicate: impl Fn(&ClientEvent) -> bool) -> bool {
        self.events[from..].iter().any(predicate)
    }

    fn edit(&mut self, file: &str, at: u64, text: &str) {
        let set = ChangeSet::new(
            file,
            vec![ChangeOp::Insert {
                at,
                text: text.to_owned(),
            }],
        );
        let content = self.docs.get(file).cloned().unwrap_or_default();
        self.docs.insert(
            file.to_owned(),
            apply(&content, &set.ops).expect("local edit is valid"),
        );
        self.client.send_changes(set);
    }

    fn send_without_applying(&self, set: ChangeSet) {
        self.client.send_changes(set);
    }
}

fn connect(client_addr: SocketAddr, user: &str, password: Option<&str>) -> CollabClient {
    CollabClient::connect(&[client_addr], user, password).expect("connect")
}

fn join(server: &CollabServer, user: &str, password: Option<&str>, open: &[&str]) -> Peer {
    join_with_ready(server, user, password, open, true)
}

fn join_with_ready(
    server: &CollabServer,
    user: &str,
    password: Option<&str>,
    open: &[&str],
    ready: bool,
) -> Peer {
    let client = connect(addr(server), user, password);
    client.set_open_files(open.iter().map(|file| (*file).to_owned()).collect());
    let mut peer = Peer::new(client);
    assert!(
        peer.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::SnapshotComplete))),
        "snapshot should complete for {user}"
    );
    if ready {
        peer.client.ready();
    }
    peer
}

fn server_generation(server: &CollabServer, path: &str) -> u64 {
    server.query_doc(path).expect("server responds").0
}

fn server_text(server: &CollabServer, path: &str) -> String {
    server
        .query_doc(path)
        .expect("server responds")
        .1
        .unwrap_or_default()
}

/// Poll until every peer holds `expected` for `path` and matches the server.
fn wait_all_equal(peers: &mut [&mut Peer], server: &CollabServer, path: &str) -> bool {
    let end = Instant::now() + DEADLINE;
    loop {
        for peer in peers.iter_mut() {
            peer.pump();
        }
        let server_now = server_text(server, path);
        let all = peers
            .iter()
            .all(|peer| peer.docs.get(path).cloned().unwrap_or_default() == server_now);
        if all {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn join_without_password_receives_snapshot() {
    let root = temp_root("join-plain");
    write_rel(&root, "main.typ", "= Title");
    let server = start_server(&root, "proj", None);

    let peer = join(&server, "ann", None, &["main.typ"]);
    assert_eq!(
        peer.docs.get("main.typ").map(String::as_str),
        Some("= Title")
    );
    assert_eq!(peer.client.generation(), 0);
    assert!(peer.saw(|event| matches!(event, ClientEvent::Joined { .. })));

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn join_with_password_and_wrong_password_rejected() {
    let root = temp_root("join-pw");
    write_rel(&root, "main.typ", "secret");
    let server = start_server(&root, "proj", Some("hunter2"));

    let good = join(&server, "ann", Some("hunter2"), &["main.typ"]);
    assert_eq!(
        good.docs.get("main.typ").map(String::as_str),
        Some("secret")
    );

    let bad = CollabClient::connect(&[addr(&server)], "bob", Some("wrong"));
    match bad {
        Err(ClientError::Rejected(RejectKind::Auth)) => {}
        Err(other) => panic!("expected auth rejection, got {other:?}"),
        Ok(_) => panic!("wrong password should be rejected"),
    }

    drop(good);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn two_clients_edit_and_converge() {
    let root = temp_root("converge");
    write_rel(&root, "main.typ", "x");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let mut b = join(&server, "bob", None, &["main.typ"]);

    a.edit("main.typ", 0, "A");
    assert!(wait_all_equal(&mut [&mut a, &mut b], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "Ax");

    b.edit("main.typ", 0, "B");
    assert!(wait_all_equal(&mut [&mut a, &mut b], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "BAx");
    assert_eq!(a.client.generation(), b.client.generation());

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn concurrent_conflict_resyncs_and_converges() {
    let root = temp_root("conflict");
    write_rel(&root, "main.typ", "x");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let mut b = join(&server, "bob", None, &["main.typ"]);

    a.edit("main.typ", 0, "A");
    b.edit("main.typ", 0, "B");

    let end = Instant::now() + DEADLINE;
    loop {
        a.pump();
        b.pump();
        let server_now = server_text(&server, "main.typ");
        let converged = a.docs.get("main.typ").cloned().unwrap_or_default() == server_now
            && b.docs.get("main.typ").cloned().unwrap_or_default() == server_now;
        if converged {
            break;
        }
        assert!(Instant::now() < end, "conflict did not converge");
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(server_now_is_one_of(&server, &["Ax", "Bx"]));
    assert_eq!(a.client.generation(), b.client.generation());
    let resynced = a.saw(|event| matches!(event, ClientEvent::Resyncing { .. }))
        || b.saw(|event| matches!(event, ClientEvent::Resyncing { .. }));
    assert!(resynced, "the losing peer should resynchronise");

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

fn server_now_is_one_of(server: &CollabServer, expected: &[&str]) -> bool {
    let now = server_text(server, "main.typ");
    expected.contains(&now.as_str())
}

#[test]
fn cursors_reach_the_other_peer_but_not_the_origin() {
    let root = temp_root("cursors");
    write_rel(&root, "main.typ", "hello");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let mut b = join(&server, "bob", None, &["main.typ"]);
    let a_sid = a.client.sid();

    a.client.send_cursors("main.typ", vec![2], vec![(0, 2)]);
    assert!(
        b.wait_for(DEADLINE, |peer| peer
            .cursor
            .as_ref()
            .is_some_and(|(sid, ..)| *sid == a_sid)),
        "B should see A's cursor"
    );
    a.pump();
    assert!(
        !a.saw(|event| matches!(event, ClientEvent::Cursors { sid, .. } if *sid == a_sid)),
        "A must not receive its own cursor"
    );

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn image_upload_is_stored_broadcast_and_fetchable() {
    let root = temp_root("image");
    write_rel(&root, "main.typ", "body");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let mut b = join(&server, "bob", None, &["main.typ"]);

    let bytes = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    let upload_id = a.client.upload_image("pic.png", bytes.clone());

    let end = Instant::now() + DEADLINE;
    let name = loop {
        b.pump();
        if let Some(ClientEvent::ImageAdded {
            upload_id: id,
            name: got,
            ..
        }) = b
            .events
            .iter()
            .find(|event| matches!(event, ClientEvent::ImageAdded { .. }))
        {
            assert_eq!(*id, upload_id);
            break got.clone();
        }
        assert!(Instant::now() < end, "ImageAdded not broadcast");
        std::thread::sleep(Duration::from_millis(5));
    };

    b.client.get_file(&name);
    assert!(
        b.wait_for(DEADLINE, |peer| peer.bins.get(&name) == Some(&bytes)),
        "B should fetch the uploaded bytes"
    );

    let snippet = ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Insert {
            at: 4,
            text: format!("\n#image(\"{name}\")"),
        }],
    );
    let content = b.docs.get("main.typ").cloned().unwrap_or_default();
    b.docs.insert(
        "main.typ".to_owned(),
        apply(&content, &snippet.ops).expect("snippet"),
    );
    b.client.send_changes(snippet);
    assert!(wait_all_equal(&mut [&mut a, &mut b], &server, "main.typ"));

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn save_writes_confirmed_content_to_disk() {
    let root = temp_root("save");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    a.edit("main.typ", 4, "-tail");
    // The save is requested while the edit is still unacked, so the client must
    // defer it until the echo clears unacked (F5).
    a.client.save_file("main.typ");
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::Saved { ok: true, .. }))),
        "save should complete"
    );
    assert!(wait_all_equal(&mut [&mut a], &server, "main.typ"));
    let on_disk = fs::read_to_string(root.join("main.typ")).expect("read saved file");
    assert_eq!(on_disk, "base-tail");

    drop(a);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn undo_redo_round_trip_and_redo_cleared_by_new_change() {
    let root = temp_root("history");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    a.edit("main.typ", 4, "-A");
    assert!(wait_all_equal(&mut [&mut a], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "base-A");

    a.client.request_history(HistoryOp::Undo);
    assert!(wait_for_text(&server, "main.typ", "base", &mut a));

    a.client.request_history(HistoryOp::Redo);
    assert!(wait_for_text(&server, "main.typ", "base-A", &mut a));

    a.client.request_history(HistoryOp::Undo);
    assert!(wait_for_text(&server, "main.typ", "base", &mut a));

    let mark = a.events.len();
    a.edit("main.typ", 4, "-B");
    assert!(wait_for_text(&server, "main.typ", "base-B", &mut a));

    let cleared = a.wait_for(Duration::from_millis(300), |peer| {
        peer.events[mark..]
            .iter()
            .rev()
            .find_map(|event| match event {
                ClientEvent::HistoryState { can_redo, .. } => Some(!can_redo),
                _ => None,
            })
            .unwrap_or(false)
    });
    assert!(cleared, "a new regular change must clear redo");

    a.client.request_history(HistoryOp::Redo);
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        a.pump();
        assert!(
            !a.saw_since(mark, |event| matches!(event, ClientEvent::Changes { .. })),
            "redo must be a no-op after it was cleared"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server_text(&server, "main.typ"), "base-B");

    drop(a);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

fn wait_for_text(server: &CollabServer, path: &str, expected: &str, peer: &mut Peer) -> bool {
    let end = Instant::now() + DEADLINE;
    loop {
        peer.pump();
        if server_text(server, path) == expected
            && peer.docs.get(path).map(String::as_str) == Some(expected)
        {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn disconnect_broadcasts_users_and_server_stop_closes() {
    let root = temp_root("disconnect");
    write_rel(&root, "main.typ", "x");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let b = join(&server, "bob", None, &["main.typ"]);
    let b_sid = b.client.sid();

    assert!(
        a.wait_for(DEADLINE, |peer| peer.users.len() == 2),
        "A should see both users"
    );

    drop(b); // sends Bye and joins threads
    assert!(
        a.wait_for(DEADLINE, |peer| {
            peer.users.len() == 1 && !peer.users.iter().any(|user| user.sid == b_sid)
        }),
        "A should see B removed from the user list"
    );

    let mut saw_left = false;
    let end = Instant::now() + DEADLINE;
    while Instant::now() < end && !saw_left {
        if let Some(ServerEvent::ClientLeft { sid, .. }) = server.try_recv_event() {
            saw_left = sid == b_sid;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(saw_left, "server should report the client leaving");

    server.stop();
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::ServerClosed))),
        "A should receive ServerClosed"
    );

    drop(a);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn direct_address_connect_uses_no_mdns() {
    let root = temp_root("direct");
    write_rel(&root, "main.typ", "direct");
    let server = start_server(&root, "proj", None);

    let direct = addr(&server);
    let client = CollabClient::connect(&[direct], "ann", None).expect("direct connect");
    let mut peer = Peer::new(client);
    assert!(peer.wait_for(DEADLINE, |peer| {
        peer.saw(|event| matches!(event, ClientEvent::SnapshotComplete))
    }));
    peer.client.ready();
    assert_eq!(
        peer.docs.get("main.typ").map(String::as_str),
        Some("direct")
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn multi_address_fallback_skips_unreachable_address() {
    let root = temp_root("fallback");
    write_rel(&root, "main.typ", "fallback");
    let server = start_server(&root, "proj", None);

    // A refused loopback port is reliably unreachable (TEST-NET would be routed
    // by a local VPN); the client must fall back to the second address.
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 1));
    let reachable = addr(&server);
    let client =
        CollabClient::connect(&[unreachable, reachable], "ann", None).expect("fallback connect");
    let mut peer = Peer::new(client);
    assert!(peer.wait_for(DEADLINE, |peer| {
        peer.saw(|event| matches!(event, ClientEvent::SnapshotComplete))
    }));

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn exhausted_candidates_report_unreachable_not_a_socket_error() {
    // A refused loopback port is the ordinary "nothing is listening" shape, and a
    // firewalled one times out the same way (Windows `os error 10060`). Either
    // must surface as `Unreachable` — the UI localises that — instead of the raw
    // `session error: socket error: …` text a user cannot act on.
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 1));
    let Err(error) = CollabClient::connect(&[unreachable], "ann", None) else {
        panic!("connect must fail when nothing is listening");
    };
    assert!(
        matches!(error, ClientError::Unreachable),
        "expected Unreachable, got: {error}"
    );
}

#[test]
fn join_outbox_delivers_change_made_before_ready() {
    let root = temp_root("outbox");
    write_rel(&root, "main.typ", "x");
    let server = start_server(&root, "proj", None);

    // A joins but does not send Ready: it stays pending with an open outbox.
    let mut a = join_with_ready(&server, "ann", None, &["main.typ"], false);

    let mut b = join(&server, "bob", None, &["main.typ"]);
    b.edit("main.typ", 0, "B");
    assert!(wait_all_equal(&mut [&mut b], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "Bx");
    assert_eq!(server_generation(&server, "main.typ"), 1);

    // Now A goes live; the queued change must arrive exactly once.
    a.client.ready();
    assert!(wait_for_text(&server, "main.typ", "Bx", &mut a));
    assert_eq!(a.client.generation(), 1);
    assert_eq!(a.docs.get("main.typ").map(String::as_str), Some("Bx"));

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resync_covers_a_file_the_peer_ignored_while_optimistic() {
    let root = temp_root("f1");
    write_rel(&root, "f1.typ", "one");
    write_rel(&root, "f2.typ", "two");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["f1.typ"]);
    let mut b = join(&server, "bob", None, &["f2.typ"]);

    // B commits on f2 first, then A commits on f1 while still optimistic; A must
    // ignore B's now-stale f2 change (recording it dirty) and later be forced to
    // resync, whose request must therefore cover f2. The winner of each round is
    // scheduling-dependent, so retry with short per-round bounds until A loses.
    for _round in 0..50 {
        let mark = a.events.len();
        b.edit("f2.typ", 3, "!");
        a.edit("f1.typ", 3, "?");

        let end = Instant::now() + Duration::from_millis(300);
        let mut saw_resync = false;
        loop {
            a.pump();
            b.pump();
            if let Some(ClientEvent::Resyncing { files }) = a
                .events
                .iter()
                .skip(mark)
                .find(|event| matches!(event, ClientEvent::Resyncing { .. }))
            {
                assert!(
                    files.iter().any(|file| file == "f2.typ"),
                    "resync must cover the ignored f2 file, got {files:?}"
                );
                saw_resync = true;
                break;
            }
            if Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if saw_resync {
            return;
        }
        // Let the previous round's conflict settle before the next one.
        let settle = Instant::now() + Duration::from_millis(300);
        loop {
            a.pump();
            b.pump();
            let server_f1 = server_text(&server, "f1.typ");
            let server_f2 = server_text(&server, "f2.typ");
            if a.docs.get("f1.typ") == Some(&server_f1)
                && a.docs.get("f2.typ") == Some(&server_f2)
                && b.docs.get("f1.typ") == Some(&server_f1)
                && b.docs.get("f2.typ") == Some(&server_f2)
            {
                break;
            }
            if Instant::now() >= settle {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    panic!("no conflict was observed in 40 rounds");
}

#[test]
fn desync_with_same_window_edit_does_not_stall() {
    let root = temp_root("f14");
    write_rel(&root, "main.typ", "abc");
    let server = start_server(&root, "proj", None);

    let mut a = join(&server, "ann", None, &["main.typ"]);
    let mut b = join(&server, "bob", None, &["main.typ"]);

    // A deliberately invalid change (delete-text mismatch) is rejected atomically
    // by the server, forcing a Desync and a resynchronisation.
    let invalid = ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Delete {
            at: 0,
            text: "ZZZ".to_owned(),
        }],
    );
    a.send_without_applying(invalid);
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::Desync))),
        "A should be told it desynced"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::SyncState { .. }))),
        "the resync should complete"
    );

    // The client is not stalled: a fresh edit must still converge.
    a.edit("main.typ", 3, ".");
    assert!(wait_all_equal(&mut [&mut a, &mut b], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "abc.");

    // An edit issued while a re-sync is in flight is dropped (F14), and the
    // client still recovers on the next edit.
    let before = a.events.len();
    a.client.request_sync(Vec::new());
    a.edit("main.typ", 4, "!");
    assert!(
        a.wait_for(DEADLINE, |peer| peer.saw_since(before, |event| matches!(
            event,
            ClientEvent::LocalEditDropped
        ))),
        "the in-window edit should be dropped"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer.saw_since(before, |event| matches!(
            event,
            ClientEvent::SyncState { .. }
        ))),
        "the forced resync should complete"
    );

    a.edit("main.typ", 4, "?");
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .docs
            .get("main.typ")
            .map(String::as_str)
            == Some("abc.?")),
        "the dropped edit must not appear"
    );
    assert!(wait_all_equal(&mut [&mut a, &mut b], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "abc.?");

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn mdns_advertise_and_browse_smoke() {
    let project = format!("proj-{}", std::process::id());
    let advertiser = Advertiser::start(&project, "ann", true, 39_950).expect("advertise");
    let browser = Browser::start().expect("browse");

    let end = Instant::now() + Duration::from_secs(15);
    let mut found = false;
    while Instant::now() < end {
        match browser.recv_timeout(Duration::from_millis(200)) {
            Some(DiscoveryEvent::Discovered(service))
                if service.project == project && service.user == "ann" =>
            {
                assert!(service.needs_password);
                assert_eq!(service.port, 39_950);
                assert!(!service.addresses.is_empty());
                found = true;
                break;
            }
            Some(_) => {}
            None => {}
        }
    }

    drop(advertiser);
    drop(browser);
    assert!(found, "mDNS browse should resolve the advertised service");
}

#[test]
fn server_host_apis_reach_clients() {
    let root = temp_root("hostapi");
    write_rel(&root, "main.typ", "x");
    let server = start_server(&root, "proj", None);
    let mut a = join(&server, "ann", None, &["main.typ"]);

    server.submit_local_change(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Insert {
            at: 0,
            text: "H".to_owned(),
        }],
    ));
    server.submit_local_cursors("main.typ", vec![1], vec![(0, 1)]);
    let bytes = vec![1u8, 2, 3, 4];
    server.publish_local_image("host.png", bytes.clone());

    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .docs
            .get("main.typ")
            .map(String::as_str)
            == Some("Hx")),
        "the client applies the server host's change"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .cursor
            .as_ref()
            .is_some_and(|(sid, ..)| *sid == 0)),
        "the client sees the server host's cursor"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::ImageAdded { name, .. } if name == "host.png"
        ))),
        "the client sees the server host's image"
    );

    server.request_local_history(HistoryOp::Undo);
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .docs
            .get("main.typ")
            .map(String::as_str)
            == Some("x")),
        "the server host's undo reverts the change on the global stack"
    );

    drop(a);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

// Regression for the startup freeze: file contents are resolved lazily and
// capped, so oversized files are never loaded or served as text.
#[test]
fn startup_scan_skips_oversized_files() {
    let root = temp_root("scan-cap");
    write_rel(&root, "main.typ", "= hi");
    fs::write(root.join("big.typ"), vec![b'a'; 11 * 1024 * 1024]).expect("write oversized");
    let server = start_server(&root, "proj", None);
    let loaded = |path: &str| {
        server
            .query_doc(path)
            .map(|(generation, text)| (generation, text.map(|text| text.len())))
    };
    assert_eq!(loaded("main.typ"), Some((0, Some(4))));
    assert_eq!(
        loaded("big.typ"),
        Some((0, None)),
        "oversized files must not be loaded into the doc store"
    );
    drop(server);
    let _ = fs::remove_dir_all(&root);
}

// The startup scan is metadata-only: a rewrite on disk after start is what a
// later request sees for a file no change set has touched.
#[test]
fn untouched_files_are_read_lazily() {
    let root = temp_root("lazy-read");
    write_rel(&root, "main.typ", "v1");
    let server = start_server(&root, "proj", None);
    // A core reply proves the startup scan has already finished.
    assert_eq!(server.query_doc("missing"), Some((0, None)));
    fs::write(root.join("main.typ"), "v2").expect("rewrite");
    assert_eq!(
        server.query_doc("main.typ"),
        Some((0, Some("v2".to_owned()))),
        "untouched files must be read from disk on demand, not cached at startup"
    );
    drop(server);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn connection_limit_refuses_extra_clients_and_keeps_the_live_peer() {
    let root = temp_root("conn-limit");
    write_rel(&root, "main.typ", "base");
    let server = start_server_with(&root, "proj", None, 1, Duration::from_secs(30));
    let mut a = join(&server, "ann", None, &["main.typ"]);

    // Typed rejections: the client must surface `Rejected(Full)`, not a generic
    // handshake error, even though the server refuses before sending `Hello`.
    for i in 0..8 {
        match CollabClient::connect(&[addr(&server)], &format!("extra-{i}"), None) {
            Err(ClientError::Rejected(RejectKind::Full)) => {}
            Ok(_) => panic!("expected a full rejection for extra-{i}, got a live client"),
            Err(other) => panic!("expected a full rejection for extra-{i}, got {other:?}"),
        }
    }

    // Raw socket flood: refused connections must not spawn sessions or wedge the
    // listener; the admitted peer must keep working afterwards.
    for _ in 0..64 {
        let _ = std::net::TcpStream::connect(addr(&server));
    }

    a.edit("main.typ", 0, "A");
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .docs
            .get("main.typ")
            .is_some_and(|text| text == "Abase")),
        "the admitted peer must still receive edits after the connection flood"
    );

    drop(a);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn pending_join_without_ready_is_dropped() {
    let root = temp_root("ready-timeout");
    write_rel(&root, "main.typ", "base");
    let server = start_server_with(&root, "proj", None, 16, Duration::from_millis(300));
    let mut pending = join_with_ready(&server, "ann", None, &["main.typ"], false);

    assert!(
        pending.wait_for(Duration::from_secs(5), |peer| peer
            .saw(|event| matches!(event, ClientEvent::Disconnected))),
        "a connection that never sends Ready must be dropped at the deadline"
    );

    drop(pending);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn snapshot_streams_a_large_project() {
    let root = temp_root("large-snapshot");
    write_rel(&root, "main.typ", "main");
    for index in 0..1100 {
        write_rel(
            &root,
            &format!("part-{index:04}.typ"),
            &format!("part {index}"),
        );
    }
    let server = start_server(&root, "proj", None);
    let peer = join(&server, "ann", None, &["main.typ"]);

    assert_eq!(
        peer.docs.len(),
        1101,
        "every text file must be streamed in the join snapshot"
    );
    assert_eq!(
        peer.docs.get("part-1099.typ").map(String::as_str),
        Some("part 1099")
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn initial_docs_are_served_to_the_first_joiner() {
    let root = temp_root("initial-docs");
    write_rel(&root, "main.typ", "disk");
    let config = ServerConfig {
        root: root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: root.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: vec![(
            "main.typ".to_owned(),
            TextSnapshot::from_owned("unsaved", 1),
        )],
    };
    let server = CollabServer::start(config, None).expect("server");
    let peer = join(&server, "ann", None, &["main.typ"]);
    assert_eq!(
        peer.docs.get("main.typ").map(String::as_str),
        Some("unsaved"),
        "the joiner must see the host's seeded buffer, not the disk copy"
    );
    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn save_of_unknown_path_fails_without_creating_it() {
    let root = temp_root("save-missing");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);
    let mut peer = join(&server, "ann", None, &["main.typ"]);

    peer.client.save_file("missing.typ");
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::Saved { path, ok: false } if path == "missing.typ"
        ))),
        "the server must refuse to save a path it has no text for"
    );
    assert!(
        !root.join("missing.typ").exists(),
        "a refused save must not create an empty file"
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn save_of_oversized_file_does_not_truncate_it() {
    let root = temp_root("save-oversized");
    write_rel(&root, "main.typ", "base");
    let oversized = vec![b'a'; 11 * 1024 * 1024];
    fs::write(root.join("big.typ"), &oversized).expect("write oversized");
    let server = start_server(&root, "proj", None);
    let mut peer = join(&server, "ann", None, &["main.typ"]);

    peer.client.save_file("big.typ");
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::Saved { path, ok: false } if path == "big.typ"
        ))),
        "an oversized doc has no authoritative text and must fail closed"
    );
    assert_eq!(
        fs::metadata(root.join("big.typ")).expect("metadata").len(),
        oversized.len() as u64,
        "the original file must not be truncated"
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn image_dir_is_shared_and_tree_entry_is_root_relative() {
    let root = temp_root("image-dir");
    write_rel(&root, "main.typ", "body");
    let image_dir = root.join("assets");
    let config = ServerConfig {
        root: root.clone(),
        project: "proj".to_owned(),
        main_file: Some("main.typ".to_owned()),
        server_user: "srv".to_owned(),
        port: 0,
        image_dir: image_dir.clone(),
        max_connections: 16,
        idle_timeout: Some(Duration::from_secs(30)),
        ready_timeout: Duration::from_secs(30),
        initial_docs: Vec::new(),
    };
    let server = CollabServer::start(config, None).expect("server");
    let mut peer = join(&server, "ann", None, &["main.typ"]);

    assert!(
        peer.saw(|event| matches!(
            event,
            ClientEvent::Config { image_dir: Some(dir), .. } if dir == "assets"
        )),
        "the join Config must carry the server's root-relative image dir"
    );
    let bytes = vec![0x89u8, b'P', b'N', b'G', 7, 8, 9];
    let upload_id = peer.client.upload_image("pic.png", bytes.clone());
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::ImageAdded { upload_id: id, name, .. }
                if *id == upload_id && name == "pic.png"
        ))),
        "the upload must be stored and broadcast"
    );
    peer.client.get_file("assets/pic.png");
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.bins.get("assets/pic.png")
            == Some(&bytes)),
        "the client must fetch the image from the server's image dir"
    );
    let mark = peer.events.len();
    peer.client.get_tree();
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.events[mark..].iter().any(|event| {
            matches!(event, ClientEvent::Tree { entries } if entries.iter().any(|entry| entry.path == "assets/pic.png"))
        })),
        "the tree entry must use the root-relative path"
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn host_rejected_local_change_requests_an_authoritative_resync() {
    let root = temp_root("host-reject");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);
    let mut peer = join(&server, "ann", None, &["main.typ"]);
    peer.edit("main.typ", 0, "C");
    assert!(wait_all_equal(&mut [&mut peer], &server, "main.typ"));
    assert_eq!(server_text(&server, "main.typ"), "Cbase");

    // The host's edit was computed against "base"; its delete text cannot match
    // the current authoritative content, so the core must reject it and offer
    // the authoritative text instead of dropping it silently.
    server.submit_local_change(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Delete {
            at: 0,
            text: "ZZZ".to_owned(),
        }],
    ));
    let end = Instant::now() + DEADLINE;
    let mut found = None;
    while Instant::now() < end {
        while let Some(event) = server.try_recv_event() {
            if let ServerEvent::ResyncFile { path, text } = event {
                found = Some((path, text));
            }
        }
        if found.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (path, text) = found.expect("the host must be told to resync");
    assert_eq!(path, "main.typ");
    assert_eq!(text.as_deref(), Some("Cbase"));

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn host_history_events_mark_only_normal_edits_as_applied() {
    let root = temp_root("host-history-flags");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);

    server.submit_local_change(ChangeSet::new(
        "main.typ",
        vec![ChangeOp::Insert {
            at: 0,
            text: "H".to_owned(),
        }],
    ));
    let end = Instant::now() + DEADLINE;
    let mut normal_flag = None;
    while Instant::now() < end && normal_flag.is_none() {
        while let Some(event) = server.try_recv_event() {
            if let ServerEvent::ChangeApplied {
                already_applied, ..
            } = event
            {
                normal_flag = Some(already_applied);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        normal_flag,
        Some(true),
        "a normal host edit is already in the host buffer"
    );
    assert_eq!(server_text(&server, "main.typ"), "Hbase");

    server.request_local_history(HistoryOp::Undo);
    let end = Instant::now() + DEADLINE;
    let mut undo_flag = None;
    while Instant::now() < end && undo_flag.is_none() {
        while let Some(event) = server.try_recv_event() {
            if let ServerEvent::ChangeApplied {
                already_applied, ..
            } = event
            {
                undo_flag = Some(already_applied);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        undo_flag,
        Some(false),
        "a host undo is not applied by the editor and must reach the host buffer"
    );
    assert_eq!(server_text(&server, "main.typ"), "base", "undo applied");

    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn history_state_is_included_in_the_join_snapshot() {
    let root = temp_root("history-join");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);
    let mut a = join(&server, "ann", None, &["main.typ"]);
    a.edit("main.typ", 0, "A");
    assert!(wait_all_equal(&mut [&mut a], &server, "main.typ"));

    let mut b = join(&server, "bob", None, &["main.typ"]);
    assert!(
        b.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::HistoryState { can_undo: true, .. }
        ))),
        "a joiner must receive the existing undo availability"
    );

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn live_clients_pause_heartbeats_while_a_snapshot_streams() {
    let root = temp_root("busy-broadcast");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);
    let mut a = join(&server, "ann", None, &["main.typ"]);

    // B's join makes the core stream a snapshot; A must be told to pause its
    // heartbeat while the core cannot answer pings.
    let b = join(&server, "bob", None, &["main.typ"]);
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::Busy { busy: true }))),
        "live clients must see busy before the snapshot"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::Busy { busy: false }))),
        "live clients must be released after the snapshot"
    );

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn live_clients_pause_heartbeats_around_a_resync() {
    let root = temp_root("busy-resync");
    write_rel(&root, "main.typ", "base");
    let server = start_server(&root, "proj", None);
    let mut a = join(&server, "ann", None, &["main.typ"]);
    let b = join(&server, "bob", None, &["main.typ"]);

    // Ignore the Busy pair produced by B's join; only the resync pair matters.
    assert!(
        a.wait_for(DEADLINE, |peer| peer
            .saw(|event| matches!(event, ClientEvent::Busy { busy: false }))),
        "the join must release live peers before the resync"
    );
    let mark = a.events.len();
    b.client.request_sync(vec!["main.typ".into()]);
    assert!(
        a.wait_for(DEADLINE, |peer| peer.events[mark..]
            .iter()
            .any(|event| matches!(event, ClientEvent::Busy { busy: true }))),
        "live clients must see busy while a resync snapshot is built"
    );
    assert!(
        a.wait_for(DEADLINE, |peer| peer.events[mark..]
            .iter()
            .any(|event| matches!(event, ClientEvent::Busy { busy: false }))),
        "live clients must be released after the resync snapshot"
    );

    drop(a);
    drop(b);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn join_snapshot_includes_binary_assets() {
    let root = temp_root("join-bin");
    write_rel(&root, "main.typ", "main");
    let image_dir = root.join("image");
    fs::create_dir_all(&image_dir).expect("image dir");
    let bytes = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3, 4];
    fs::write(image_dir.join("pic.png"), &bytes).expect("write image");
    let server = start_server(&root, "proj", None);
    let peer = join(&server, "ann", None, &["main.typ"]);
    assert_eq!(
        peer.bins.get("image/pic.png"),
        Some(&bytes),
        "binary assets must be streamed in the join snapshot"
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn chunked_download_streams_progress_chunks() {
    let root = temp_root("chunked");
    write_rel(&root, "main.typ", "main");
    let bytes: Vec<u8> = (0..(1024 * 1024 + 12345))
        .map(|i| (i % 251) as u8)
        .collect();
    fs::create_dir_all(root.join("assets")).expect("assets dir");
    fs::write(root.join("assets/big.bin"), &bytes).expect("write asset");
    let server = start_server(&root, "proj", None);
    let mut peer = join(&server, "ann", None, &["main.typ"]);

    peer.client.get_chunked_file("assets/big.bin", false);
    let mut downloaded = Vec::new();
    let mut cursor = 0;
    let mut chunks = 0;
    let deadline = Instant::now() + DEADLINE;
    while downloaded.len() < bytes.len() {
        peer.pump();
        while cursor < peer.events.len() {
            if let ClientEvent::FileChunk {
                path,
                offset,
                total,
                bytes: chunk,
            } = &peer.events[cursor]
                && path == "assets/big.bin"
                && *offset == downloaded.len() as u64
            {
                assert_eq!(*total, bytes.len() as u64);
                downloaded.extend_from_slice(chunk);
                chunks += 1;
            }
            cursor += 1;
        }
        assert!(Instant::now() < deadline, "chunked download timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(downloaded, bytes);
    assert!(chunks > 1, "the file must arrive in multiple chunks");

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn chunked_download_reports_the_over_limit_size() {
    let root = temp_root("chunked-large");
    write_rel(&root, "main.typ", "main");
    fs::write(root.join("huge.bin"), vec![0u8; 33 * 1024 * 1024]).expect("write huge");
    let server = start_server(&root, "proj", None);
    let mut peer = join(&server, "ann", None, &["main.typ"]);

    peer.client.get_chunked_file("huge.bin", false);
    assert!(
        peer.wait_for(DEADLINE, |peer| peer.saw(|event| matches!(
            event,
            ClientEvent::FileTooLarge { path, size }
                if path == "huge.bin" && *size == 33 * 1024 * 1024
        ))),
        "the server must report the over-limit size before prompting"
    );

    drop(peer);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unauthenticated_connections_expire() {
    let root = temp_root("handshake-timeout");
    write_rel(&root, "main.typ", "base");
    let server = start_server_with(&root, "proj", None, 1, Duration::from_millis(300));
    // Hold the only connection slot with a socket that never sends `Join`.
    let raw = std::net::TcpStream::connect(addr(&server)).expect("raw connect");

    let end = Instant::now() + Duration::from_secs(5);
    let mut client = None;
    while Instant::now() < end {
        match CollabClient::connect(&[addr(&server)], "ann", None) {
            Ok(joined) => {
                client = Some(joined);
                break;
            }
            Err(ClientError::Rejected(RejectKind::Full)) => {}
            Err(other) => panic!("unexpected connect error: {other:?}"),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let client = client.expect("the handshake timeout must release the slot");

    drop(client);
    drop(raw);
    server.stop();
    let _ = fs::remove_dir_all(&root);
}
