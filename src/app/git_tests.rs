//! App-level git worker tests: a real temp repo round-trip and response routing.

use super::git::{GitRequest, GitResponse, GitState, GitStatusResult, GitWorker};
use git2::Repository;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use typ_edit_git::{BlameLine, CommitInfo, FileStatus, StatusKind};

/// Unique scratch root under the OS temp dir (never the workspace).
fn temp_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "typ-edit-app-git-test-{}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp root");
    dir
}

fn init_repo(dir: &Path) -> git2::Oid {
    let repo = Repository::init(dir).expect("init repo");
    let mut config = repo.config().expect("config");
    config.set_str("user.name", "Test User").expect("user.name");
    config
        .set_str("user.email", "test@example.com")
        .expect("user.email");
    std::fs::write(dir.join("a.typ"), "= A").expect("write");
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("stage");
    index.write().expect("write index");
    let tree = repo
        .find_tree(index.write_tree().expect("tree"))
        .expect("find tree");
    let signature = git2::Signature::now("Test User", "test@example.com").expect("signature");
    repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
        .expect("commit")
}

fn collect(worker: &GitWorker, count: usize) -> Vec<GitResponse> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut out = Vec::new();
    while out.len() < count && Instant::now() < deadline {
        out.extend(worker.drain());
        if out.len() < count {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    out
}

#[test]
fn worker_round_trips_every_request_kind() {
    let root = temp_root("worker");
    let oid = init_repo(&root);
    std::fs::write(root.join("untracked.typ"), "new").expect("write untracked");
    let worker = GitWorker::spawn(root.clone(), Arc::new(|| {}));

    worker.request(GitRequest::RefreshStatus);
    worker.request(GitRequest::Log {
        path: None,
        limit: 5,
    });
    worker.request(GitRequest::Diff {
        oid: oid.to_string(),
        path: None,
    });
    worker.request(GitRequest::Blame {
        path: PathBuf::from("a.typ"),
        line_count: Some(2),
    });

    let responses = collect(&worker, 4);
    assert_eq!(responses.len(), 4, "expected one response per request");
    match &responses[0] {
        GitResponse::Status(status) => {
            assert!(status.repo_root.is_some());
            assert!(!status.branch.is_empty());
            assert!(
                status
                    .statuses
                    .iter()
                    .any(|file| file.path == *"untracked.typ"),
                "untracked file must appear in the snapshot"
            );
        }
        _ => panic!("first response must be Status"),
    }
    assert!(matches!(&responses[1], GitResponse::Log { items, .. } if !items.is_empty()));
    assert!(matches!(&responses[2], GitResponse::Diff { text, .. } if text.contains("+")));
    assert!(matches!(&responses[3], GitResponse::Blame { lines, .. } if lines.len() == 1));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_refresh_surfaces_in_the_snapshot() {
    let mut state = GitState::new(Arc::new(|| {}));
    state.apply(GitResponse::Status(GitStatusResult {
        repo_root: Some(PathBuf::from("repo")),
        branch: String::new(),
        head_short: String::new(),
        statuses: Vec::new(),
        error: Some("index.lock: permission denied".to_owned()),
    }));
    assert_eq!(
        state.snapshot.error.as_deref(),
        Some("index.lock: permission denied"),
        "an unreadable repository must not look like a clean one"
    );
}

#[test]
fn applying_responses_fills_every_state_slot() {
    let mut state = GitState::new(Arc::new(|| {}));
    state.apply(GitResponse::Status(GitStatusResult {
        repo_root: Some(PathBuf::from("repo")),
        branch: "main".to_owned(),
        head_short: "abc1234".to_owned(),
        statuses: vec![FileStatus {
            path: PathBuf::from("a.typ"),
            kind: StatusKind::Modified,
        }],
        error: None,
    }));
    assert_eq!(state.repo_root.as_deref(), Some(Path::new("repo")));
    assert_eq!(state.snapshot.branch, "main");
    assert_eq!(state.snapshot.head_short, "abc1234");
    assert_eq!(state.snapshot.revision, 1);
    assert_eq!(
        state.snapshot.statuses.get(Path::new("a.typ")),
        Some(&StatusKind::Modified)
    );

    state.apply(GitResponse::Log {
        path: Some(PathBuf::from("a.typ")),
        items: vec![CommitInfo {
            oid: "a".repeat(40),
            short: "aaaaaaa".to_owned(),
            author: "Test User".to_owned(),
            time: 1,
            summary: "init".to_owned(),
        }],
    });
    assert_eq!(state.log.len(), 1);
    assert_eq!(state.log_path.as_deref(), Some(Path::new("a.typ")));

    state.apply(GitResponse::Diff {
        oid: "a".repeat(40),
        text: "+line".to_owned(),
    });
    assert_eq!(state.diff, Some(("a".repeat(40), "+line".to_owned())));

    state.apply(GitResponse::Blame {
        path: PathBuf::from("a.typ"),
        lines: vec![BlameLine {
            line: 1,
            oid: "a".repeat(40),
            short: "aaaaaaa".to_owned(),
            author: "Test User".to_owned(),
            time: 1,
            summary: "init".to_owned(),
        }],
    });
    assert_eq!(state.blame_path.as_deref(), Some(Path::new("a.typ")));
    assert_eq!(state.blame.len(), 1);

    state.shutdown();
    assert!(state.repo_root.is_none());
    assert!(state.log.is_empty());
}
