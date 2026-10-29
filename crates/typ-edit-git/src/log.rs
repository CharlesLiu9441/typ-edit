use crate::error::GitError;
use crate::path::key;
use crate::repo::{GitRepo, short_id};
use crate::types::CommitInfo;
use git2::{Commit, DiffDelta, Repository, Sort};
use std::path::Path;

impl GitRepo {
    /// Newest commits first, capped at `limit`.
    ///
    /// When `path` is given, only commits whose diff against the first parent
    /// touches that path are returned; the root commit (no parent) is compared
    /// against an empty tree so its files count too.
    pub fn log(&self, path: Option<&Path>, limit: usize) -> Result<Vec<CommitInfo>, GitError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let repo = self.repo();
        let filter = path.map(key);
        let mut walk = repo.revwalk()?;
        // Topological order puts every parent after its children (newest first
        // for linear history) even when commits share a timestamp; time breaks
        // ties across branches.
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        walk.push_head()?;
        let mut commits = Vec::new();
        for oid in walk {
            let commit = repo.find_commit(oid?)?;
            if let Some(target) = filter.as_deref()
                && !touches(&repo, &commit, target)?
            {
                continue;
            }
            commits.push(info(&commit));
            if commits.len() >= limit {
                break;
            }
        }
        Ok(commits)
    }
}

/// Whether `commit` (vs its first parent) changes `target`.
fn touches(repo: &Repository, commit: &Commit<'_>, target: &str) -> Result<bool, git2::Error> {
    let tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
    Ok(diff.deltas().any(|delta| path_matches(&delta, target)))
}

/// Whether a diff delta's old or new path equals `target`.
fn path_matches(delta: &DiffDelta<'_>, target: &str) -> bool {
    [delta.new_file().path(), delta.old_file().path()]
        .into_iter()
        .flatten()
        .any(|path| key(path) == target)
}

/// Projects a commit onto the crate's [`CommitInfo`].
pub(crate) fn info(commit: &Commit<'_>) -> CommitInfo {
    let oid = commit.id().to_string();
    CommitInfo {
        short: short_id(commit.id()),
        author: commit.author().name().ok().unwrap_or_default().to_owned(),
        time: commit.time().seconds(),
        summary: commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_owned(),
        oid,
    }
}
