//! UI-free Git integration for the editor, built on `git2`.
//!
//! Local-repository operations only: working-tree status, commit history,
//! per-commit unified diffs and line blame. Network transports (fetch/push/clone)
//! are deliberately not used. Every operation is blocking, so the app runs them
//! on a dedicated worker thread and never calls into this crate from the UI.

mod blame;
mod diff;
mod error;
mod log;
mod path;
mod repo;
mod status;
mod types;

#[cfg(test)]
mod tests;

pub use error::GitError;
pub use repo::{GitRepo, HeadInfo};
pub use types::{BlameLine, CommitInfo, FileStatus, StatusKind};
