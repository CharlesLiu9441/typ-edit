//! Workspace-wide assertion that every `t`-family call-site key is translated.

use super::i18n_scanner::literal_keys_in_source;
use crate::{en_pack, zh_pack};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Keys deliberately used outside the packs: the negative test that proves an
/// unknown key falls back to itself.
const ALLOWLISTED_KEYS: &[&str] = &["this.key.does.not.exist"];

/// Recursively collects `.rs` files under `dir`, skipping hidden directories.
fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every workspace Rust source under `src/` and `crates/`, sorted for
/// determinism.
fn workspace_rust_sources() -> Vec<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at <workspace>/crates/<name>");
    let mut files = Vec::new();
    for top in ["src", "crates"] {
        collect_rust_sources(&workspace.join(top), &mut files);
    }
    files.sort();
    files
}

/// Every call-site key in the workspace exists in both packs.
#[test]
fn workspace_call_site_keys_exist_in_both_packs() {
    let zh = zh_pack();
    let en = en_pack();

    let mut keys: BTreeSet<String> = BTreeSet::new();
    for path in workspace_rust_sources() {
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for key in literal_keys_in_source(&text) {
            if !ALLOWLISTED_KEYS.contains(&key.as_str()) {
                keys.insert(key);
            }
        }
    }

    let missing_zh: Vec<&String> = keys
        .iter()
        .filter(|k| !zh.contains_key(k.as_str()))
        .collect();
    let missing_en: Vec<&String> = keys
        .iter()
        .filter(|k| !en.contains_key(k.as_str()))
        .collect();
    assert!(
        missing_zh.is_empty(),
        "{} call-site keys missing from zh-CN: {missing_zh:?}",
        missing_zh.len()
    );
    assert!(
        missing_en.is_empty(),
        "{} call-site keys missing from en-US: {missing_en:?}",
        missing_en.len()
    );
    assert!(
        keys.len() > 300,
        "expected hundreds of call-site keys, found {} — scanner likely broken",
        keys.len()
    );
}
