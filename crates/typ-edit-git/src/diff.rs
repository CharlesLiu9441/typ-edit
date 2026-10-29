use crate::error::GitError;
use crate::path::key;
use crate::repo::GitRepo;
use git2::{DiffFormat, DiffOptions, Oid};
use std::path::Path;

impl GitRepo {
    /// Unified patch of `oid` against its first parent, optionally limited to `path`.
    ///
    /// The text is what `git show` prints: `diff --git` / `---` / `+++` headers,
    /// `@@` hunk headers, then `+`/`-`/space-prefixed body lines. The root commit
    /// is diffed against an empty tree.
    pub fn diff_commit(&self, oid: &str, path: Option<&Path>) -> Result<String, GitError> {
        let repo = self.repo();
        let commit = repo.find_commit(Oid::from_str(oid)?)?;
        let tree = commit.tree()?;
        let parent_tree = match commit.parent(0) {
            Ok(parent) => Some(parent.tree()?),
            Err(_) => None,
        };
        let mut options = DiffOptions::new();
        if let Some(path) = path {
            options.pathspec(key(path));
        }
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))?;
        let mut patch = String::new();
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            // Header lines (`F`/`H`) carry their prefix in `content`; body lines
            // carry only their text, so the `+`/`-`/space origin is re-added.
            match line.origin() {
                '+' | '-' | ' ' => patch.push(line.origin()),
                _ => {}
            }
            patch.push_str(&String::from_utf8_lossy(line.content()));
            true
        })?;
        Ok(patch)
    }
}
