//! App-owned cache inventory and clearing.
//!
//! The app keeps a small registry index under the config directory, collab
//! mirrors under `collab/`, and short-lived `typ-edit-*` entries under the OS
//! temp directory. This module lists them with their on-disk size and removes
//! only paths that fall under those roots.

use std::path::{Path, PathBuf};

use typ_edit_i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
    Registry,
    Collab,
    Temp,
}

#[derive(Clone, Debug)]
pub struct CacheEntry {
    pub kind: CacheKind,
    pub label: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Lists the app-owned cache entries that currently exist.
pub fn entries() -> Vec<CacheEntry> {
    let mut out = Vec::new();
    if let Some(config) = crate::settings::config_dir() {
        let cache = config.join("cache");
        if cache.exists() {
            out.push(CacheEntry {
                kind: CacheKind::Registry,
                label: t("settings.cache.entry.registry").into_owned(),
                bytes: path_size(&cache),
                path: cache,
            });
        }
        let collab = config.join("collab");
        if collab.exists() {
            out.push(CacheEntry {
                kind: CacheKind::Collab,
                label: t("settings.cache.entry.collab").into_owned(),
                bytes: path_size(&collab),
                path: collab,
            });
        }
    }

    if let Ok(read) = crate::platform::fs_ops::read_dir(&std::env::temp_dir()) {
        let mut temp = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("typ-edit-") {
                temp.push(entry.path());
            }
        }
        temp.sort();
        for path in temp {
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            out.push(CacheEntry {
                kind: CacheKind::Temp,
                label,
                bytes: path_size(&path),
                path,
            });
        }
    }
    out
}

/// Removes one cache entry. Paths outside the app-owned roots are refused.
pub fn clear(entry: &CacheEntry) -> std::io::Result<()> {
    if !is_app_owned(&entry.path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing to remove a path outside the app cache roots",
        ));
    }
    if entry.path.is_dir() {
        crate::platform::fs_ops::remove_dir_all(&entry.path)
    } else {
        crate::platform::fs_ops::remove_file(&entry.path)
    }
}

fn is_app_owned(path: &Path) -> bool {
    if path.starts_with(std::env::temp_dir()) {
        return path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("typ-edit-"));
    }
    crate::settings::config_dir().is_some_and(|dir| {
        path.starts_with(dir.join("cache")) || path.starts_with(dir.join("collab"))
    })
}

fn path_size(path: &Path) -> u64 {
    let Ok(meta) = crate::platform::fs_ops::metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = crate::platform::fs_ops::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let child = entry.path();
            match crate::platform::fs_ops::metadata(&child) {
                Ok(meta) if meta.is_dir() => stack.push(child),
                Ok(meta) => total = total.saturating_add(meta.len()),
                Err(_) => {}
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_clear_an_unrelated_temp_path() {
        let entry = CacheEntry {
            kind: CacheKind::Temp,
            label: "unrelated".to_owned(),
            path: std::env::temp_dir().join("not-a-typ-edit-cache"),
            bytes: 0,
        };
        let err = clear(&entry).expect_err("unrelated path must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn measures_and_clears_an_app_owned_temp_directory() {
        let path = std::env::temp_dir().join(format!(
            "typ-edit-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("nested")).expect("create cache dir");
        std::fs::write(path.join("nested").join("entry.bin"), b"12345").expect("write cache file");

        let entry = CacheEntry {
            kind: CacheKind::Temp,
            label: "test".to_owned(),
            bytes: path_size(&path),
            path: path.clone(),
        };
        assert_eq!(entry.bytes, 5);
        clear(&entry).expect("clear app-owned temp dir");
        assert!(!path.exists());
    }
}
