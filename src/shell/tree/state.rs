//! [`ProjectTree`] state and its cached directory-listing IO.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::RowRef;

/// Cached, sorted children of a directory. `None` marks a directory that could not be read.
pub(super) type DirCache = HashMap<PathBuf, Option<Arc<Vec<(PathBuf, bool)>>>>;

/// Tree state (root directory, expanded dirs, current main file, open file).
pub struct ProjectTree {
    pub root: Option<PathBuf>,
    pub main_file: Option<PathBuf>,
    /// File currently open in the editor; its row is outlined in the tree.
    pub open_file: Option<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    /// Keyboard cursor (the row path), independent of the open file.
    pub cursor: Option<PathBuf>,
    /// Directory listings, read once and reused until explicitly invalidated.
    pub(super) cache: DirCache,
}

impl ProjectTree {
    pub fn new(root: Option<PathBuf>, main_file: Option<PathBuf>) -> Self {
        Self {
            root,
            main_file,
            open_file: None,
            expanded: HashSet::new(),
            cursor: None,
            cache: DirCache::new(),
        }
    }

    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
        self.expanded.clear();
        self.cursor = None;
        self.cache.clear();
    }

    /// Drops the cached listing of `dir` so it is re-read on the next frame.
    pub fn refresh_dir(&mut self, dir: &Path) {
        self.cache.remove(dir);
    }

    /// Drops cached listings for `path`, its descendants and its parent.
    ///
    /// Called after create/delete/rename: the parent listing must forget or gain an entry, and a
    /// renamed/removed subtree's own listings are stale.
    pub fn invalidate(&mut self, path: &Path) {
        self.cache.retain(|dir, _| !dir.starts_with(path));
        if let Some(parent) = path.parent() {
            self.cache.remove(parent);
        }
    }

    /// Reads (and caches) the visible rows under `root`, in draw order.
    pub(super) fn visible_rows(&mut self, root: &Path) -> Vec<RowRef> {
        let mut rows = Vec::new();
        collect_rows(root, 0, &self.expanded, &mut self.cache, &mut rows);
        rows
    }
}

/// Human-readable file name (lossy for non-UTF-8 paths — never panics).
pub(super) fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Name used for the case-insensitive alphabetical ordering.
fn sort_key(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Sort entries directories-first, then case-insensitively alphabetical by file name.
fn sort_entries(entries: &mut [(PathBuf, bool)]) {
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| sort_key(&a.0).cmp(&sort_key(&b.0)))
    });
}

/// Whether a directory entry is app metadata the tree hides (the workspace
/// marker file and the crash-recovery snapshot directory).
fn is_hidden_entry(name: &str) -> bool {
    name == crate::workspace::MAIN_MARKER || name == crate::workspace::SNAPSHOT_DIR
}

/// Reads and sorts a directory's children; `None` when it cannot be read.
fn read_dir_entries(dir: &Path) -> Option<Vec<(PathBuf, bool)>> {
    let read = match crate::platform::fs_ops::read_dir(dir) {
        Ok(read) => read,
        Err(err) => {
            log::warn!("项目树: 无法读取目录 {}: {err}", dir.display());
            return None;
        }
    };
    let mut entries = Vec::new();
    for entry in read {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_hidden_entry)
                {
                    continue;
                }
                // `file_type` does not follow symlinks, so recursion cannot loop.
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push((path, is_dir));
            }
            Err(err) => log::warn!("项目树: 跳过 {} 中的条目: {err}", dir.display()),
        }
    }
    sort_entries(&mut entries);
    Some(entries)
}

/// Returns the cached listing of `dir`, loading and caching it once on first use.
pub(super) fn cached_entries(
    cache: &mut DirCache,
    dir: &Path,
) -> Option<Arc<Vec<(PathBuf, bool)>>> {
    cache
        .entry(dir.to_path_buf())
        .or_insert_with(|| read_dir_entries(dir).map(Arc::new))
        .clone()
}

/// Flattens the visible tree under `dir` in draw order.
fn collect_rows(
    dir: &Path,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    cache: &mut DirCache,
    rows: &mut Vec<RowRef>,
) {
    let Some(entries) = cached_entries(cache, dir) else {
        return;
    };
    for (path, is_dir) in entries.iter() {
        let is_expanded = *is_dir && expanded.contains(path);
        rows.push(RowRef {
            path: path.clone(),
            is_dir: *is_dir,
            depth,
            expanded: is_expanded,
        });
        if is_expanded {
            collect_rows(path, depth + 1, expanded, cache, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn entry(name: &str, is_dir: bool) -> (PathBuf, bool) {
        (PathBuf::from(name), is_dir)
    }

    fn names(entries: &[(PathBuf, bool)]) -> Vec<String> {
        entries.iter().map(|(path, _)| display_name(path)).collect()
    }

    /// Monotonic suffix so parallel tests never share a scratch directory.
    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// Unique scratch directory under the system temp dir.
    fn scratch_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("typ-edit-tree-{}-{id}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn sort_entries_puts_dirs_first_then_case_insensitive_alphabetical() {
        let mut entries = vec![
            entry("zeta.typ", false),
            entry("Alpha", true),
            entry("beta.txt", false),
            entry("gamma", true),
            entry("Delta.typ", false),
        ];
        sort_entries(&mut entries);
        assert_eq!(
            names(&entries),
            ["Alpha", "gamma", "beta.txt", "Delta.typ", "zeta.typ"]
        );
    }

    #[test]
    fn sort_entries_handles_empty_lists() {
        let mut entries: Vec<(PathBuf, bool)> = Vec::new();
        sort_entries(&mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn cached_entries_reads_once_and_reuses_the_cache_until_invalidated() {
        let dir = scratch_dir();
        std::fs::create_dir(dir.join("a")).expect("create subdir");
        std::fs::write(dir.join("b.typ"), "").expect("write file");

        let mut cache = DirCache::new();
        let first = cached_entries(&mut cache, &dir).expect("first listing");
        assert_eq!(names(&first), ["a", "b.typ"]);

        // A new file on disk must not appear until the cache entry is dropped.
        std::fs::write(dir.join("c.typ"), "").expect("write second file");
        let second = cached_entries(&mut cache, &dir).expect("cached listing");
        assert!(Arc::ptr_eq(&first, &second), "the cached listing is reused");
        assert_eq!(names(&second), ["a", "b.typ"]);

        cache.remove(&dir);
        let third = cached_entries(&mut cache, &dir).expect("reloaded listing");
        assert_eq!(names(&third), ["a", "b.typ", "c.typ"]);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn cached_entries_skips_workspace_and_recovery_metadata() {
        let dir = scratch_dir();
        std::fs::create_dir(dir.join(crate::workspace::SNAPSHOT_DIR)).expect("create snapshot dir");
        std::fs::write(dir.join(crate::workspace::MAIN_MARKER), "{}").expect("write marker");
        std::fs::write(dir.join("main.typ"), "").expect("write source");

        let mut cache = DirCache::new();
        let entries = cached_entries(&mut cache, &dir).expect("listing");
        assert_eq!(names(&entries), ["main.typ"]);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn invalidate_drops_the_path_parent_and_descendants() {
        let mut tree = ProjectTree::new(Some(PathBuf::from("root")), None);
        tree.cache
            .insert(PathBuf::from("root"), Some(Arc::new(Vec::new())));
        tree.cache
            .insert(PathBuf::from("root/sub"), Some(Arc::new(Vec::new())));
        tree.cache
            .insert(PathBuf::from("root/sub/deep"), Some(Arc::new(Vec::new())));
        tree.cache
            .insert(PathBuf::from("other"), Some(Arc::new(Vec::new())));

        tree.invalidate(Path::new("root/sub"));
        assert!(!tree.cache.contains_key(Path::new("root/sub")));
        assert!(!tree.cache.contains_key(Path::new("root/sub/deep")));
        assert!(
            !tree.cache.contains_key(Path::new("root")),
            "parent dropped"
        );
        assert!(
            tree.cache.contains_key(Path::new("other")),
            "unrelated kept"
        );
    }
}
