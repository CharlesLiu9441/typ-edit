//! Thin filesystem façade for the composed read/write paths.
//!
//! Every function returns [`io::Result`] and none of them touch egui or i18n,
//! so workers can call the module without pulling in the UI. It is deliberately
//! not a trait-based backend: there is one platform, and callers keep their own
//! error-handling policy (ignore, log, or surface) unchanged.

use std::fs::{File, ReadDir};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Number of rename attempts in [`write_atomic`] before giving up.
const RENAME_ATTEMPTS: u32 = 5;

/// Reads a UTF-8 text file, stripping a single leading UTF-8 BOM. Whitespace is
/// left untouched, so the caller can parse it (JSON, dictionary, snapshot).
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text))
}

/// Writes `bytes` to `path` atomically.
///
/// The parent directory is created as needed, the bytes go to a sibling
/// `<name>.tmp` file, and that temp file is renamed over the target. The rename
/// retries a few times on a permission or Windows sharing error because
/// antivirus, indexers and other editors transiently hold the target open. On
/// any failure the temp file is removed, best effort, and the error is returned;
/// the target is never truncated, so a crash or a full disk cannot destroy an
/// existing file (this is why document saves, exports and refactors all go
/// through here).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_streaming(path, |file| file.write_all(bytes))
}

/// Atomically writes whatever `write` produces, without requiring the caller
/// to hold the whole payload as a contiguous slice.
///
/// The staging, fsync and rename policy is identical to [`write_atomic`].
pub(crate) fn write_atomic_streaming<F>(path: &Path, write: F) -> io::Result<()>
where
    F: FnOnce(&mut File) -> io::Result<()>,
{
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        create_dir_all(dir)?;
    }
    let tmp = temp_path(path);
    let staged = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        write(&mut file)?;
        // A durability hint must never change the outcome, but it is the only
        // signal that the bytes may not survive a crash, so it is logged rather
        // than dropped.
        if let Err(error) = file.sync_all() {
            log::warn!("cannot sync {}: {error}", tmp.display());
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    match rename_with_retry(&tmp, path) {
        Ok(()) => {
            // Persist the new directory entry as well, so a crash right after the
            // rename cannot resurrect the old file. Directories cannot be opened
            // for syncing on Windows, so this is unix-only.
            #[cfg(unix)]
            if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                if let Err(error) = File::open(dir).and_then(|dir| dir.sync_all()) {
                    log::warn!("cannot sync {}: {error}", dir.display());
                }
            }
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Writes a disposable cache file (the collaboration mirror) without the
/// atomic replace and per-file fsync used for user documents.
///
/// The mirror is rebuilt from the server on the next join, so a crash can only
/// leave a stale cache entry, never corrupt a user document. Avoiding the
/// per-file `sync_all` is what keeps a large snapshot download responsive on
/// the UI thread.
pub(crate) fn write_mirror(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// Creates a directory and all of its ancestors.
pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Removes a regular file.
pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

/// Removes a directory and all of its contents.
pub(crate) fn remove_dir_all(path: &Path) -> io::Result<()> {
    std::fs::remove_dir_all(path)
}

/// Renames or moves `from` to `to` (no retry; use [`write_atomic`] for the
/// replace-a-live-target case).
pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Copies `from` to `to`, returning the number of bytes copied.
pub(crate) fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    std::fs::copy(from, to)
}

/// Reads a path's metadata (existence, size, timestamps, file type).
pub(crate) fn metadata(path: &Path) -> io::Result<std::fs::Metadata> {
    std::fs::metadata(path)
}

/// Opens a directory and returns its entry iterator. The caller keeps its own
/// per-entry error handling, so the entries are intentionally not collected.
pub(crate) fn read_dir(path: &Path) -> io::Result<ReadDir> {
    std::fs::read_dir(path)
}

/// A unique sibling temp path used by [`write_atomic`].
///
/// The process id and a monotonic counter keep concurrent writers to the same
/// target from truncating each other's staging file.
fn temp_path(path: &Path) -> PathBuf {
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// Renames `from` onto `to`, retrying while the error is transient.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut delay = Duration::from_millis(10);
    let mut remaining = RENAME_ATTEMPTS;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) => {
                remaining -= 1;
                if remaining == 0 || !is_transient_rename_error(&err) {
                    return Err(err);
                }
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
        }
    }
}

/// Whether a failed rename may succeed if retried: a permission denial or a
/// Windows sharing violation (os error 5 / 32) is usually a transient lock.
fn is_transient_rename_error(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
        || matches!(err.raw_os_error(), Some(5) | Some(32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic suffix so parallel tests never share a scratch directory.
    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// Unique, fresh scratch directory under the system temp dir.
    fn scratch_dir() -> PathBuf {
        let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("typ-edit-fs-ops-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn read_to_string_strips_a_single_leading_bom_only() {
        let dir = scratch_dir();

        let with_bom = dir.join("bom.json");
        std::fs::write(&with_bom, "\u{feff}{\"a\":1}").expect("write bom file");
        assert_eq!(read_to_string(&with_bom).expect("read"), "{\"a\":1}");

        // Whitespace after the BOM (and plain files) must be preserved verbatim.
        let spaced = dir.join("spaced.json");
        std::fs::write(&spaced, "  {\"a\":1}\n").expect("write spaced file");
        assert_eq!(read_to_string(&spaced).expect("read"), "  {\"a\":1}\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names of every staging file left under `dir`.
    fn tmp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("read scratch")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                name.ends_with(".tmp").then_some(name)
            })
            .collect()
    }

    #[test]
    fn write_atomic_creates_parents_overwrites_and_leaves_no_tmp() {
        let dir = scratch_dir();
        let parent = dir.join("nested");
        let path = parent.join("config.json");

        write_atomic(&path, b"first").expect("write into missing parent");
        assert_eq!(std::fs::read(&path).expect("read"), b"first");
        assert!(
            tmp_files(&parent).is_empty(),
            "temp file must be renamed away"
        );

        write_atomic(&path, b"second").expect("overwrite existing");
        assert_eq!(std::fs::read(&path).expect("read"), b"second");
        assert!(
            tmp_files(&parent).is_empty(),
            "temp file must be renamed away"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_streaming_writes_in_chunks_and_leaves_no_tmp() {
        use std::io::Write as _;
        let dir = scratch_dir();
        let path = dir.join("streamed.txt");
        write_atomic_streaming(&path, |file| {
            file.write_all(b"one")?;
            file.write_all(b" two")?;
            file.write_all(b" three")
        })
        .expect("streamed write");
        assert_eq!(std::fs::read(&path).expect("read"), b"one two three");
        assert!(
            tmp_files(&dir).is_empty(),
            "streamed temp file must be renamed away"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_atomic_writes_keep_one_whole_file() {
        let dir = scratch_dir();
        let path = dir.join("shared.txt");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for writer in 0..2 {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for round in 0..25 {
                    let text = format!("writer-{writer}-{round}-{}", "x".repeat(64 * 1024));
                    write_atomic(&path, text.as_bytes()).expect("concurrent write");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer joins");
        }

        let content = std::fs::read_to_string(&path).expect("read shared file");
        assert!(
            content.starts_with("writer-0-") || content.starts_with("writer-1-"),
            "the target must hold one writer's whole payload"
        );
        assert!(
            tmp_files(&dir).is_empty(),
            "no staging files may remain: {:?}",
            tmp_files(&dir)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_mirror_creates_parents_and_overwrites() {
        let dir = scratch_dir();
        let path = dir.join("nested").join("page.typ");
        write_mirror(&path, b"first").expect("write mirror");
        assert_eq!(std::fs::read(&path).expect("read"), b"first");
        write_mirror(&path, b"second").expect("overwrite mirror");
        assert_eq!(std::fs::read(&path).expect("read"), b"second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_dir_lists_entries() {
        let dir = scratch_dir();
        std::fs::write(dir.join("a.typ"), "").expect("write a");
        std::fs::write(dir.join("b.typ"), "").expect("write b");

        let mut names: Vec<String> = read_dir(&dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(names, ["a.typ", "b.typ"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
