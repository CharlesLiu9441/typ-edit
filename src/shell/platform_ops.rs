//! Filesystem and shell actions for the project tree: create, delete, reveal and terminal.
//!
//! Every entry point returns [`std::io::Result`]; UI callers turn failures into dialog-local
//! messages. OS-specific process handling lives behind `#[cfg(windows)]` / `#[cfg(not(windows))]`.
//! The OS-shell layer (create/delete/reveal/terminal) deliberately uses `std::fs` directly,
//! separate from [`crate::platform::fs_ops`].

use std::io;
use std::path::{Path, PathBuf};

use typ_edit_i18n::{t, t_with};

/// Characters that are invalid in a Windows file name. Rejected on every platform so a project
/// stays portable.
const INVALID_NAME_CHARS: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Checks a user-provided file/folder name.
///
/// The name is trimmed first; empty names, `.`/`..` and Windows-reserved characters are rejected.
/// The error message is shown verbatim in the dialog, so it is Chinese.
pub fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(t("error.name_empty").into_owned());
    }
    if name == "." || name == ".." {
        return Err(t("error.name_dot").into_owned());
    }
    if let Some(ch) = name.chars().find(|ch| INVALID_NAME_CHARS.contains(ch)) {
        return Err(t_with("error.name_invalid", &[("ch", &ch.to_string())]));
    }
    Ok(())
}

/// Creates the empty file `dir/name` and returns its full path.
///
/// `create_new` never truncates an existing file: the call fails with `AlreadyExists` instead.
pub fn create_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let path = dir.join(name.trim());
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok(path)
}

/// Creates the directory `dir/name` and returns its full path.
pub fn create_dir(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let path = dir.join(name.trim());
    std::fs::create_dir(&path)?;
    Ok(path)
}

/// Deletes a file, or a directory with all of its contents.
pub fn delete_path(path: &Path, is_dir: bool) -> io::Result<()> {
    if is_dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Renames/moves `from` to `to`.
pub fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Shows `path` in the system file manager.
#[cfg(windows)]
pub fn reveal_in_explorer(path: &Path, is_dir: bool) -> io::Result<()> {
    let mut command = std::process::Command::new("explorer.exe");
    if is_dir {
        command.arg(path);
    } else {
        // `/select,` must be glued to the path in a single argument.
        command.arg(format!("/select,{}", path.display()));
    }
    // `explorer.exe` exits immediately after handing the request to the running shell; never wait.
    command.spawn().map(|_| ())
}

/// Flag opening the fallback console in its own window.
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// Opens a terminal at `dir`, without blocking the UI.
///
/// Tries Windows Terminal first; when `wt.exe` is not installed it falls back to `cmd.exe`
/// started with its own console.
#[cfg(windows)]
pub fn open_terminal(dir: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;

    match std::process::Command::new("wt.exe")
        .arg("-d")
        .arg(dir)
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            let mut command = std::process::Command::new("cmd.exe");
            command.args(["/k", "cd", "/d"]).arg(dir);
            command.creation_flags(CREATE_NEW_CONSOLE);
            command.spawn().map(|_| ())
        }
    }
}

/// Shows `path` in the system file manager.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn reveal_in_explorer(_path: &Path, _is_dir: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        t("error.reveal_unsupported").into_owned(),
    ))
}

/// Opens a terminal at `dir`.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn open_terminal(_dir: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        t("error.terminal_unsupported").into_owned(),
    ))
}

/// macOS argument vector for revealing a path in Finder.
///
/// A file is revealed with `open -R`; a directory is opened directly, matching
/// the Windows `explorer <dir>` behavior.
#[cfg(any(test, target_os = "macos"))]
fn macos_reveal_argv(path: &Path, is_dir: bool) -> (&'static str, Vec<std::ffi::OsString>) {
    let mut args = Vec::with_capacity(2);
    if !is_dir {
        args.push(std::ffi::OsString::from("-R"));
    }
    args.push(path.as_os_str().to_owned());
    ("open", args)
}

/// macOS argument vector for opening Terminal at `dir`.
#[cfg(any(test, target_os = "macos"))]
fn macos_terminal_argv(dir: &Path) -> (&'static str, Vec<std::ffi::OsString>) {
    (
        "open",
        vec![
            std::ffi::OsString::from("-a"),
            std::ffi::OsString::from("Terminal"),
            dir.as_os_str().to_owned(),
        ],
    )
}

/// Shows `path` in Finder.
#[cfg(target_os = "macos")]
pub fn reveal_in_explorer(path: &Path, is_dir: bool) -> io::Result<()> {
    spawn_macos(macos_reveal_argv(path, is_dir))
}

/// Opens Terminal at `dir`.
#[cfg(target_os = "macos")]
pub fn open_terminal(dir: &Path) -> io::Result<()> {
    spawn_macos(macos_terminal_argv(dir))
}

/// Spawns a detached macOS helper process without waiting for it.
#[cfg(target_os = "macos")]
fn spawn_macos((program, args): (&'static str, Vec<std::ffi::OsString>)) -> io::Result<()> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic suffix so parallel tests never share a scratch directory.
    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// Unique scratch directory under the system temp dir.
    fn scratch_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-platform-ops-{}-{id}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn validate_name_accepts_regular_names_and_trims() {
        assert!(validate_name("chapter 1.typ").is_ok());
        assert!(validate_name("  note.typ  ").is_ok());
        assert!(validate_name("新建文件夹").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty_and_dot_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
    }

    #[test]
    fn validate_name_rejects_invalid_characters() {
        for ch in INVALID_NAME_CHARS {
            assert!(
                validate_name(&format!("a{ch}b")).is_err(),
                "expected {ch} to be rejected"
            );
        }
    }

    #[test]
    fn create_file_writes_an_empty_file_and_refuses_to_overwrite() {
        let dir = scratch_dir();
        let path = create_file(&dir, " chapter.typ ").expect("create file");
        assert_eq!(path, dir.join("chapter.typ"));
        assert!(path.is_file());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        let err = create_file(&dir, "chapter.typ").expect_err("second create must fail");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn create_dir_makes_a_directory_and_refuses_duplicates() {
        let dir = scratch_dir();
        let path = create_dir(&dir, "chapters").expect("create dir");
        assert_eq!(path, dir.join("chapters"));
        assert!(path.is_dir());

        assert!(create_dir(&dir, "chapters").is_err());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn rename_path_moves_files_and_directory_trees() {
        let dir = scratch_dir();
        let file = create_file(&dir, "old.typ").expect("create file");
        let renamed = dir.join("new.typ");
        rename_path(&file, &renamed).expect("rename file");
        assert!(!file.exists());
        assert!(renamed.is_file());

        let nested = dir.join("chapters");
        std::fs::create_dir(&nested).expect("create dir");
        std::fs::write(nested.join("part.typ"), "x").expect("write nested file");
        let moved = dir.join("book");
        rename_path(&nested, &moved).expect("rename dir");
        assert!(!nested.exists());
        assert!(moved.join("part.typ").is_file());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn delete_path_removes_files_and_directory_trees() {
        let dir = scratch_dir();
        let file = create_file(&dir, "note.typ").expect("create file");
        delete_path(&file, false).expect("delete file");
        assert!(!file.exists());

        let nested = dir.join("chapters").join("deep");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::write(nested.join("part.typ"), "x").expect("write nested file");
        delete_path(&dir.join("chapters"), true).expect("delete dir tree");
        assert!(!dir.join("chapters").exists());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn macos_reveal_opens_directories_and_reveals_files() {
        let dir = Path::new("/tmp/book");
        let (program, args) = macos_reveal_argv(dir, true);
        assert_eq!(program, "open");
        assert_eq!(args, vec![OsString::from("/tmp/book")]);

        let file = Path::new("/tmp/book/main.typ");
        let (program, args) = macos_reveal_argv(file, false);
        assert_eq!(program, "open");
        assert_eq!(
            args,
            vec![OsString::from("-R"), OsString::from("/tmp/book/main.typ")]
        );
    }

    #[test]
    fn macos_terminal_opens_terminal_at_the_directory() {
        let dir = Path::new("/tmp/book");
        let (program, args) = macos_terminal_argv(dir);
        assert_eq!(program, "open");
        assert_eq!(
            args,
            vec![
                OsString::from("-a"),
                OsString::from("Terminal"),
                OsString::from("/tmp/book")
            ]
        );
    }
}
