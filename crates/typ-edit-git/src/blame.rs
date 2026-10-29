use crate::error::GitError;
use crate::repo::{GitRepo, short_id};
use crate::types::BlameLine;
use git2::{BlameOptions, Repository};
use std::path::Path;

impl GitRepo {
    /// Blames `path`, one entry per line.
    ///
    /// `content: None` returns every blamed line of the file. `content: Some(text)`
    /// supports unsaved buffer text: the committed file is blamed and its per-line
    /// blame is zipped positionally onto the first `min(lines)` lines of `text`
    /// (a deliberate simplification — no offset or word-level tracking).
    ///
    /// `git2` resolves the blamed blob from the newest commit (HEAD), not the
    /// working-tree copy, so uncommitted edits never shift attribution. A path with
    /// no committed version (for example an untracked file) yields an empty list.
    pub fn blame(&self, path: &Path, content: Option<&str>) -> Result<Vec<BlameLine>, GitError> {
        let repo = self.repo();
        let mut options = BlameOptions::new();
        let blame = match repo.blame_file(path, Some(&mut options)) {
            Ok(blame) => blame,
            Err(_) => return Ok(Vec::new()),
        };
        let mut lines = Vec::new();
        for hunk in blame.iter() {
            let oid = hunk.final_commit_id();
            let (author, time, summary) = metadata(&repo, oid, &hunk);
            let start = hunk.final_start_line() as u32;
            for offset in 0..hunk.lines_in_hunk() {
                lines.push(BlameLine {
                    line: start + offset as u32,
                    oid: oid.to_string(),
                    short: short_id(oid),
                    author: author.clone(),
                    time,
                    summary: summary.clone(),
                });
            }
        }
        if let Some(text) = content {
            lines.truncate(text.lines().count());
        }
        Ok(lines)
    }
}

/// Commit author/time/summary for a hunk, falling back to the hunk's own
/// signature when the commit cannot be looked up.
fn metadata(
    repo: &Repository,
    oid: git2::Oid,
    hunk: &git2::BlameHunk<'_>,
) -> (String, i64, String) {
    if let Ok(commit) = repo.find_commit(oid) {
        return (
            commit.author().name().ok().unwrap_or_default().to_owned(),
            commit.time().seconds(),
            commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_owned(),
        );
    }
    let author = hunk
        .final_signature()
        .and_then(|signature| signature.name().ok().map(str::to_owned))
        .unwrap_or_default();
    (author, 0, String::new())
}
