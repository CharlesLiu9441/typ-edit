use crate::error::GitError;
use crate::path::to_path;
use crate::repo::GitRepo;
use crate::types::{FileStatus, StatusKind};
use git2::{Status, StatusOptions};

impl GitRepo {
    /// Working-tree status of every changed or untracked file.
    ///
    /// Ignored files are skipped, renames surface as [`StatusKind::Modified`],
    /// and the result is sorted by path for deterministic ordering.
    pub fn status(&self) -> Result<Vec<FileStatus>, GitError> {
        let repo = self.repo();
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true);
        let statuses = repo.statuses(Some(&mut options))?;
        let mut files: Vec<FileStatus> = statuses
            .iter()
            .filter_map(|entry| {
                let kind = classify(entry.status())?;
                let path = entry.path().ok()?;
                Some(FileStatus {
                    path: to_path(path),
                    kind,
                })
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }
}

/// Maps one libgit2 status flag set to a [`StatusKind`]; `None` for ignored or
/// unchanged entries (which `statuses` never yields, but the encoding is total).
fn classify(status: Status) -> Option<StatusKind> {
    if status.is_index_new() {
        Some(StatusKind::Added)
    } else if status.is_wt_new() {
        Some(StatusKind::Untracked)
    } else if status.is_index_deleted() || status.is_wt_deleted() {
        Some(StatusKind::Deleted)
    } else if status.is_index_renamed()
        || status.is_wt_renamed()
        || status.is_index_modified()
        || status.is_wt_modified()
        || status.is_index_typechange()
        || status.is_wt_typechange()
    {
        Some(StatusKind::Modified)
    } else {
        None
    }
}
