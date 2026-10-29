//! Per-workspace metadata stored in a hidden dot-file inside the project root.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sysinfo::{Pid, ProcessesToUpdate, System};

/// Hidden file at a project root that records the workspace's main file.
pub const MAIN_MARKER: &str = ".typ-edit.json";

/// Hidden directory at a project root holding per-project metadata (the
/// crash-recovery snapshot). Hidden from the project tree.
pub const SNAPSHOT_DIR: &str = ".typ-edit";

pub const LOCAL_DIR: &str = "local";

pub const LOCK_FILE: &str = "project.lock";

#[derive(serde::Serialize, serde::Deserialize)]
struct LockRecord {
    pid: u32,
    #[serde(default)]
    start_time: u64,
}

#[derive(Debug)]
pub enum ProjectLockError {
    InUse(u32),
    Corrupt(String),
    Io(String),
}

impl std::fmt::Display for ProjectLockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InUse(pid) => write!(formatter, "project is already in use by PID {pid}"),
            Self::Corrupt(path) => write!(formatter, "project lock is unreadable: {path}"),
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ProjectLockError {}

pub struct ProjectLock {
    root: PathBuf,
    path: PathBuf,
    pid: u32,
    start_time: u64,
}

impl ProjectLock {
    pub fn acquire(root: &Path) -> Result<Self, ProjectLockError> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if !root.is_dir() {
            return Err(ProjectLockError::Io(format!(
                "project root is not a directory: {}",
                root.display()
            )));
        }
        let path = lock_path(&root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ProjectLockError::Io(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        let pid = std::process::id();
        let start_time = process_start_time(pid).ok_or_else(|| {
            ProjectLockError::Io("cannot read the current process start time".into())
        })?;
        for attempt in 0..5 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let record = LockRecord { pid, start_time };
                    let json = serde_json::to_vec(&record).map_err(|error| {
                        ProjectLockError::Io(format!("cannot encode project lock: {error}"))
                    })?;
                    if let Err(error) = file.write_all(&json) {
                        let _ = fs::remove_file(&path);
                        return Err(ProjectLockError::Io(format!(
                            "cannot write {}: {error}",
                            path.display()
                        )));
                    }
                    let _ = file.sync_all();
                    return Ok(Self {
                        root,
                        path,
                        pid,
                        start_time,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = match fs::read_to_string(&path) {
                        Ok(text) => text,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(ProjectLockError::Io(format!(
                                "cannot read {}: {error}",
                                path.display()
                            )));
                        }
                    };
                    match serde_json::from_str::<LockRecord>(&text) {
                        Ok(record) if record.pid == pid && record.start_time == start_time => {
                            return Ok(Self {
                                root,
                                path,
                                pid,
                                start_time,
                            });
                        }
                        Ok(record) if process_has_start_time(record.pid, record.start_time) => {
                            return Err(ProjectLockError::InUse(record.pid));
                        }
                        Ok(_) => {
                            let _ = fs::remove_file(&path);
                        }
                        Err(_) if attempt < 4 => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => {
                            return Err(ProjectLockError::Corrupt(path.display().to_string()));
                        }
                    }
                }
                Err(error) => {
                    return Err(ProjectLockError::Io(format!(
                        "cannot create {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(ProjectLockError::Io(format!(
            "cannot acquire project lock: {}",
            path.display()
        )))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        release_lock(&self.path, self.pid, self.start_time);
    }
}

pub fn lock_path(root: &Path) -> PathBuf {
    root.join(SNAPSHOT_DIR).join(LOCAL_DIR).join(LOCK_FILE)
}

fn release_lock(path: &Path, pid: u32, start_time: u64) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(record) = serde_json::from_str::<LockRecord>(&text) else {
        return;
    };
    if record.pid == pid && record.start_time == start_time {
        let _ = fs::remove_file(path);
    }
}

fn process_start_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map(|process| process.start_time())
}

fn process_has_start_time(pid: u32, start_time: u64) -> bool {
    process_start_time(pid).is_some_and(|current| start_time == 0 || current == start_time)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WorkspaceMeta {
    main_file: Option<String>,
}

/// Reads the workspace's stored main file: a path relative to `root` that still
/// points at a file; `None` when unset, unreadable, or invalid.
pub fn main_file(root: &Path) -> Option<PathBuf> {
    let text = crate::platform::fs_ops::read_to_string(&root.join(MAIN_MARKER)).ok()?;
    let meta: WorkspaceMeta = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let rel = meta.main_file?;
    let rel = Path::new(&rel);
    if rel.as_os_str().is_empty() || rel.is_absolute() {
        return None;
    }
    let path = root.join(rel);
    path.is_file().then_some(path)
}

/// Whether `root` is an app-owned transient workspace (collab mirror or a
/// directory under the OS temp dir). Such roots must never enter the recent
/// projects MRU: they are deleted/recreated by the app and are not user work.
pub fn is_transient_root(root: &Path) -> bool {
    if root.starts_with(std::env::temp_dir()) {
        return true;
    }
    crate::settings::config_dir()
        .map(|dir| root.starts_with(dir.join("collab")))
        .unwrap_or(false)
}

/// Writes the workspace's main file into the marker; `None`, a non-file, or a
/// path outside `root` removes the marker instead. A failed marker write is
/// returned so a user-initiated caller can surface it; the best-effort clear
/// logs and always succeeds.
pub fn set_main_file(root: &Path, main: Option<&Path>) -> std::io::Result<()> {
    let marker = root.join(MAIN_MARKER);
    let relative = main
        .filter(|path| path.starts_with(root) && path.is_file())
        .and_then(|path| path.strip_prefix(root).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"));
    let Some(relative) = relative else {
        if let Err(err) = crate::platform::fs_ops::remove_file(&marker)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot clear workspace marker {}: {err}", marker.display());
        }
        return Ok(());
    };
    let meta = WorkspaceMeta {
        main_file: Some(relative),
    };
    let json = serde_json::to_string_pretty(&meta).map_err(std::io::Error::other)?;
    crate::platform::fs_ops::write_atomic(&marker, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("typ-edit-workspace-{name}-{}", std::process::id()))
    }

    #[test]
    fn roundtrip_writes_and_clears_the_marker() {
        let root = temp_root("roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("create temp root");
        let chapter = root.join("sub").join("chapter.typ");
        std::fs::write(&chapter, "= Chapter").expect("write chapter");

        set_main_file(&root, Some(&chapter)).expect("write marker");
        assert_eq!(main_file(&root), Some(chapter.clone()));

        set_main_file(&root, None).expect("clear marker");
        assert_eq!(main_file(&root), None);
        assert!(!root.join(MAIN_MARKER).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_paths_outside_the_root() {
        let root = temp_root("rejection");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");

        // A missing marker reads as `None`.
        assert_eq!(main_file(&root), None);

        // An absolute path outside the root is rejected.
        let absolute = std::env::temp_dir().join("typ-edit-workspace-outside.typ");
        std::fs::write(&absolute, "= Outside").expect("write outside");
        set_main_file(&root, Some(&absolute)).expect("reject outside path");
        assert_eq!(main_file(&root), None);
        assert!(!root.join(MAIN_MARKER).exists());

        // A relative path outside the root is rejected too.
        let outside = root
            .parent()
            .expect("root has a parent")
            .join("elsewhere.typ");
        std::fs::write(&outside, "= Elsewhere").expect("write elsewhere");
        set_main_file(&root, Some(&outside)).expect("reject outside path");
        assert_eq!(main_file(&root), None);
        assert!(!root.join(MAIN_MARKER).exists());

        let _ = std::fs::remove_file(&absolute);
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transient_roots_cover_temp_and_collab_mirrors() {
        let temp = std::env::temp_dir().join("typ-edit-transient-test");
        assert!(is_transient_root(&temp));

        if let Some(config) = crate::settings::config_dir() {
            assert!(is_transient_root(&config.join("collab").join("peer")));
        }

        let project = std::env::current_dir()
            .expect("current dir")
            .join("ordinary-project");
        assert!(!is_transient_root(&project));
    }

    #[test]
    fn project_lock_lives_under_the_hidden_local_directory_and_is_released() {
        let root = temp_root("project-lock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");

        let lock = ProjectLock::acquire(&root).expect("acquire lock");
        assert_eq!(lock.pid, std::process::id());
        assert_eq!(lock.root(), root.canonicalize().expect("canonical root"));
        let expected_path = lock_path(lock.root());
        assert_eq!(lock.path, expected_path);
        assert!(lock.path.is_file());

        let recorded = std::fs::read_to_string(&lock.path).expect("read lock");
        let recorded: LockRecord = serde_json::from_str(&recorded).expect("parse lock");
        assert_eq!(recorded.pid, std::process::id());
        assert_eq!(
            recorded.start_time,
            process_start_time(std::process::id()).expect("current process start time")
        );

        let lock_path = lock.path.clone();
        drop(lock);
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dead_processes_do_not_hold_a_project_lock() {
        let root = temp_root("stale-project-lock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = lock_path(&root);
        std::fs::create_dir_all(path.parent().expect("lock parent")).expect("create local dir");
        std::fs::write(&path, r#"{"pid":4294967295,"start_time":0}"#).expect("write stale lock");

        let lock = ProjectLock::acquire(&root).expect("reclaim stale lock");
        assert_eq!(lock.pid, std::process::id());
        assert!(process_start_time(std::process::id()).is_some());
        assert!(process_start_time(u32::MAX).is_none());

        let lock_path = lock.path.clone();
        drop(lock);
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reused_pid_with_a_different_start_time_does_not_hold_a_project_lock() {
        let root = temp_root("reused-pid-project-lock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = lock_path(&root);
        std::fs::create_dir_all(path.parent().expect("lock parent")).expect("create local dir");
        let current_start = process_start_time(std::process::id()).expect("current start time");
        let stale = serde_json::to_vec(&LockRecord {
            pid: std::process::id(),
            start_time: current_start.saturating_add(1),
        })
        .expect("encode stale lock");
        std::fs::write(&path, stale).expect("write stale lock");

        let lock = ProjectLock::acquire(&root).expect("reclaim reused pid");
        assert_eq!(lock.start_time, current_start);
        let lock_path = lock.path.clone();
        drop(lock);
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
