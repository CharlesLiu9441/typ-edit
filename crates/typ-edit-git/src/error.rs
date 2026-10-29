use std::fmt;

/// Error returned by a [`crate::GitRepo`] operation.
///
/// Wraps the underlying [`git2::Error`]; the crate has no other failure modes
/// (paths that cannot be represented are lossily normalized rather than rejected).
#[derive(Debug)]
pub struct GitError(git2::Error);

impl GitError {
    /// The underlying `git2` error.
    pub fn source_error(&self) -> &git2::Error {
        &self.0
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git: {}", self.0.message())
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl From<git2::Error> for GitError {
    fn from(error: git2::Error) -> Self {
        Self(error)
    }
}
