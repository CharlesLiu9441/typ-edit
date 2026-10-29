use crate::error::GitError;
use git2::{Oid, Repository};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Branch name and short HEAD id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadInfo {
    /// Current branch name (`HEAD` when detached or unborn).
    pub branch: String,
    /// Abbreviated HEAD commit id; empty on an unborn HEAD.
    pub short: String,
}

/// Cheap, cloneable handle to an open repository.
///
/// Cloning shares the same repository behind an `Arc<Mutex<..>>`; every method
/// takes `&self`. `git2::Repository` is `Send` but not `Sync`, so the mutex is
/// what makes the handle shareable with the worker thread.
#[derive(Clone)]
pub struct GitRepo {
    inner: Arc<Mutex<Repository>>,
}

impl GitRepo {
    /// Discovers the repository containing `start`; `None` when there is none.
    pub fn discover(start: &Path) -> Option<GitRepo> {
        let repository = Repository::discover(start).ok()?;
        Some(GitRepo {
            inner: Arc::new(Mutex::new(repository)),
        })
    }

    /// The repository working directory, when it has one (never a bare repo here).
    pub fn workdir(&self) -> Option<PathBuf> {
        self.repo().workdir().map(Path::to_path_buf)
    }

    /// Branch name and short HEAD id; empty branch/short on an unborn HEAD.
    pub fn head(&self) -> Result<HeadInfo, GitError> {
        let repo = self.repo();
        match repo.head() {
            Ok(reference) => {
                let branch = reference.shorthand().unwrap_or("HEAD").to_owned();
                let short = reference
                    .peel_to_commit()
                    .map(|commit| short_id(commit.id()))
                    .unwrap_or_default();
                Ok(HeadInfo { branch, short })
            }
            // Unborn HEAD (a fresh repository with no commits): report the
            // branch the first commit will land on, with no commit id.
            Err(_) => Ok(HeadInfo {
                branch: repo
                    .find_reference("HEAD")
                    .ok()
                    .and_then(|reference| {
                        reference
                            .symbolic_target()
                            .ok()
                            .flatten()
                            .map(branch_from_ref)
                    })
                    .unwrap_or_else(|| "HEAD".to_owned()),
                short: String::new(),
            }),
        }
    }

    /// Locks the shared repository, recovering from a poisoned mutex.
    pub(crate) fn repo(&self) -> MutexGuard<'_, Repository> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Abbreviated object id: the first 7 hex characters.
pub(crate) fn short_id(oid: Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

/// Strips the `refs/heads/` prefix from a symbolic HEAD target.
fn branch_from_ref(target: &str) -> String {
    target
        .strip_prefix("refs/heads/")
        .unwrap_or(target)
        .to_owned()
}
