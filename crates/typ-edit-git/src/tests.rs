//! Crate tests against real, self-cleaning temporary repositories.

use super::*;
use git2::{IndexAddOption, Repository, Signature};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique scratch root under the OS temp dir (never the workspace).
fn temp_root(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "typ-edit-git-test-{}-{seq}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp root");
    dir
}

fn init_repo(dir: &Path) -> Repository {
    let repo = Repository::init(dir).expect("init repo");
    let mut config = repo.config().expect("repo config");
    config.set_str("user.name", "Test User").expect("user.name");
    config
        .set_str("user.email", "test@example.com")
        .expect("user.email");
    repo
}

/// Stages every change and commits; returns the new commit id.
fn commit_all(repo: &Repository, message: &str) -> git2::Oid {
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .expect("stage all");
    index.write().expect("write index");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let signature = Signature::now("Test User", "test@example.com").expect("signature");
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .expect("commit")
}

fn discover(dir: &Path) -> GitRepo {
    GitRepo::discover(dir).expect("repo should be discovered")
}

#[test]
fn discover_finds_repo_from_subdir_and_none_elsewhere() {
    let root = temp_root("discover");
    let repo = init_repo(&root);
    std::fs::write(root.join("a.typ"), "= A").expect("write");
    commit_all(&repo, "init");

    let sub = root.join("nested").join("deep");
    std::fs::create_dir_all(&sub).expect("make subdir");
    let found = discover(&sub);
    let expected = std::fs::canonicalize(&root).expect("canonical root");
    let workdir = found.workdir().expect("workdir");
    assert_eq!(std::fs::canonicalize(workdir).expect("canonical"), expected);
    assert!(found.status().is_ok());

    let plain = temp_root("not-a-repo");
    assert!(GitRepo::discover(&plain).is_none());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
fn status_reports_added_untracked_modified_deleted_with_normalized_paths() {
    let root = temp_root("status");
    let repo = init_repo(&root);
    std::fs::write(root.join("tracked.typ"), "one").expect("write");
    std::fs::write(root.join("deleted.typ"), "gone").expect("write");
    commit_all(&repo, "init");

    std::fs::write(root.join("tracked.typ"), "two").expect("modify");
    std::fs::remove_file(root.join("deleted.typ")).expect("delete");
    std::fs::write(root.join("untracked.typ"), "new").expect("write untracked");
    std::fs::write(root.join("added.typ"), "staged").expect("write added");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("added.typ")).expect("stage added");
    index.write().expect("write index");

    let statuses = discover(&root).status().expect("status");
    let find = |name: &str| {
        statuses
            .iter()
            .find(|entry| entry.path == *name)
            .map(|entry| entry.kind)
    };
    assert_eq!(find("added.typ"), Some(StatusKind::Added));
    assert_eq!(find("untracked.typ"), Some(StatusKind::Untracked));
    assert_eq!(find("tracked.typ"), Some(StatusKind::Modified));
    assert_eq!(find("deleted.typ"), Some(StatusKind::Deleted));
    let paths: Vec<&PathBuf> = statuses.iter().map(|entry| &entry.path).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "status must be sorted by path");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn log_is_newest_first_caps_and_filters_including_the_root_commit() {
    let root = temp_root("log");
    let repo = init_repo(&root);
    std::fs::write(root.join("a.typ"), "first").expect("write");
    commit_all(&repo, "first");
    std::fs::write(root.join("a.typ"), "second").expect("modify");
    commit_all(&repo, "second");
    std::fs::write(root.join("b.typ"), "third").expect("write");
    commit_all(&repo, "third");
    let git = discover(&root);

    let all = git.log(None, 10).expect("log");
    let summaries: Vec<&str> = all.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["third", "second", "first"]);
    assert_eq!(all[0].oid.len(), 40);
    assert_eq!(all[0].short.len(), 7);
    assert_eq!(all[0].author, "Test User");
    assert!(all[0].time > 0);

    assert_eq!(git.log(None, 2).expect("log").len(), 2);
    assert!(git.log(None, 0).expect("log").is_empty());

    let only_b: Vec<String> = git
        .log(Some(Path::new("b.typ")), 10)
        .expect("log b")
        .into_iter()
        .map(|c| c.summary)
        .collect();
    assert_eq!(only_b, ["third"]);

    // a.typ was added by the root commit, so the root must be included.
    let only_a: Vec<String> = git
        .log(Some(Path::new("a.typ")), 10)
        .expect("log a")
        .into_iter()
        .map(|c| c.summary)
        .collect();
    assert_eq!(only_a, ["second", "first"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn diff_commit_prints_a_unified_patch_and_honours_the_path_filter() {
    let root = temp_root("diff");
    let repo = init_repo(&root);
    std::fs::write(root.join("a.typ"), "old line").expect("write");
    std::fs::write(root.join("b.typ"), "keep").expect("write");
    commit_all(&repo, "first");
    std::fs::write(root.join("a.typ"), "new line").expect("modify");
    let oid = commit_all(&repo, "second");
    let git = discover(&root);

    let patch = git.diff_commit(&oid.to_string(), None).expect("diff");
    assert!(patch.contains("diff --git"), "patch: {patch}");
    assert!(patch.contains("--- a/a.typ"), "patch: {patch}");
    assert!(patch.contains("+++ b/a.typ"), "patch: {patch}");
    assert!(patch.contains("-old line"), "patch: {patch}");
    assert!(patch.contains("+new line"), "patch: {patch}");
    assert!(!patch.contains("b.typ"), "unrelated path leaked: {patch}");

    let filtered = git
        .diff_commit(&oid.to_string(), Some(Path::new("a.typ")))
        .expect("diff");
    assert!(filtered.contains("a.typ"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn blame_reports_per_line_commits_and_maps_unsaved_content() {
    let root = temp_root("blame");
    let repo = init_repo(&root);
    std::fs::write(root.join("a.typ"), "alpha\nbeta\n").expect("write");
    commit_all(&repo, "one");
    std::fs::write(root.join("a.typ"), "alpha\nbeta\ngamma\n").expect("modify");
    commit_all(&repo, "two");
    let git = discover(&root);

    let lines = git.blame(Path::new("a.typ"), None).expect("blame");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].line, 1);
    assert_eq!(lines[2].line, 3);
    assert!(lines.iter().all(|line| line.author == "Test User"));
    assert!(lines.iter().all(|line| line.oid.len() == 40));
    assert!(lines.iter().any(|line| line.summary == "two"));

    // Unsaved content shorter than the blamed file truncates to its line count.
    let short = git
        .blame(Path::new("a.typ"), Some("x\ny"))
        .expect("blame short");
    assert_eq!(short.len(), 2);
    // Unsaved content longer than the blamed file maps only the blamed lines.
    let long = git
        .blame(Path::new("a.typ"), Some("x\ny\nz\nw\nv"))
        .expect("blame long");
    assert_eq!(long.len(), 3);

    // An untracked file has no committed version: empty, not an error.
    std::fs::write(root.join("untracked.typ"), "hi").expect("write");
    assert!(
        git.blame(Path::new("untracked.typ"), None)
            .expect("blame untracked")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn head_reports_branch_and_short_id_then_unborn() {
    let root = temp_root("head");
    let repo = init_repo(&root);
    let git = discover(&root);
    let unborn = git.head().expect("unborn head");
    assert!(!unborn.branch.is_empty());
    assert!(unborn.short.is_empty());

    std::fs::write(root.join("a.typ"), "= A").expect("write");
    let oid = commit_all(&repo, "init");
    let head = git.head().expect("head");
    assert!(!head.branch.is_empty());
    assert_eq!(head.short, &oid.to_string()[..7]);

    let _ = std::fs::remove_dir_all(&root);
}
