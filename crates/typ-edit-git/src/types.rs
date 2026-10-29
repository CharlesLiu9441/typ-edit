use std::path::PathBuf;

/// Working-tree status of one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    /// Staged new file (`A`).
    Added,
    /// Modified content, including renames (`M`).
    Modified,
    /// Removed from the index or the working tree (`D`).
    Deleted,
    /// Present in the working tree but not tracked (`??`).
    Untracked,
}

/// One file with its repository-relative path and status.
///
/// `path` always uses forward slashes, on every platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub path: PathBuf,
    pub kind: StatusKind,
}

/// Summary of one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    /// Full 40-character object id (hex).
    pub oid: String,
    /// Abbreviated object id (7 hex characters).
    pub short: String,
    /// Author name.
    pub author: String,
    /// Author time as Unix seconds.
    pub time: i64,
    /// First line of the commit message.
    pub summary: String,
}

/// One blamed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// 1-based line number.
    pub line: u32,
    /// Full commit object id (hex).
    pub oid: String,
    /// Abbreviated commit object id (7 hex characters).
    pub short: String,
    /// Author name of the blamed commit.
    pub author: String,
    /// Author time of the blamed commit as Unix seconds.
    pub time: i64,
    /// First line of the blamed commit message.
    pub summary: String,
}
