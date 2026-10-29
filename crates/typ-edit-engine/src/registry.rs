//! Access to the `@preview` package registry and locally installed versions.
//!
//! Typst Universe publishes a single JSON index at [`REGISTRY_INDEX_URL`]. Its
//! schema is explicitly not stabilized, so every field beyond the required ones
//! is optional and a malformed entry is skipped rather than failing the whole
//! index. Installing and removing packages goes through the standard typst-kit
//! layout: `{data|cache}/preview/{name}/{version}`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_kit::downloader::Downloader;
use typst_kit::packages::FsPackages;

use crate::packages::{curl_downloader, system_packages};

/// The URL of the official Typst Universe package index.
pub const REGISTRY_INDEX_URL: &str = "https://packages.typst.org/preview/index.json";

/// The namespace served by the official Typst Universe registry.
const PREVIEW_NAMESPACE: &str = "preview";

/// A single entry from the `@preview` package index.
#[derive(Clone, Debug, Deserialize)]
pub struct RegistryEntry {
    /// The package name within the `preview` namespace.
    pub name: String,
    /// The published version, as a `major.minor.patch` string.
    pub version: String,
    /// The package entrypoint, relative to the package root.
    pub entrypoint: String,
    /// The package authors.
    #[serde(default)]
    pub authors: Vec<String>,
    /// The package license identifier.
    #[serde(default)]
    pub license: Option<String>,
    /// A short package description.
    #[serde(default)]
    pub description: Option<String>,
    /// A link to the package's web presence.
    #[serde(default)]
    pub homepage: Option<String>,
    /// A link to the repository where the package is developed.
    #[serde(default)]
    pub repository: Option<String>,
    /// Search keywords for the package.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Up to three predefined categories for discovery.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Disciplines the package targets.
    #[serde(default)]
    pub disciplines: Vec<String>,
    /// The minimum required compiler version.
    #[serde(default)]
    pub compiler: Option<String>,
    /// Template metadata, if the package is a template.
    #[serde(default)]
    pub template: Option<TemplateEntry>,
    /// The time of the package's last update, as a Unix timestamp.
    #[serde(default, rename = "updatedAt")]
    pub updated_at: Option<u64>,
}

/// The `template` object of a [`RegistryEntry`].
#[derive(Clone, Debug, Deserialize)]
pub struct TemplateEntry {
    /// The directory within the package that holds the template files.
    pub path: String,
    /// The compilation target, relative to [`path`](Self::path).
    pub entrypoint: String,
    /// A path to a PNG or lossless WebP thumbnail.
    #[serde(default)]
    pub thumbnail: Option<String>,
}

/// Parses the registry index, skipping entries that do not match the schema.
///
/// A malformed top-level document yields an empty list. Use [`load_index`] when
/// a fetch or parse failure must be reported.
pub fn parse_index(json: &str) -> Vec<RegistryEntry> {
    decode_index(json).unwrap_or_default()
}

/// Loads the registry index, preferring a fresh on-disk cache.
///
/// The cache at `cache_path` is returned when its modification time is within
/// `max_age` and its contents parse. Otherwise the index is downloaded from
/// [`REGISTRY_INDEX_URL`] and written back to `cache_path` best-effort: the
/// parent directory is created if needed and a write failure is ignored.
pub fn load_index(
    cache_path: &Path,
    max_age: Duration,
    force_refresh: bool,
) -> Result<Vec<RegistryEntry>, String> {
    if !force_refresh && let Some(entries) = read_fresh_cache(cache_path, max_age) {
        return Ok(entries);
    }

    let bytes = curl_downloader()
        .download(&"package index", REGISTRY_INDEX_URL)
        .map_err(|err| format!("failed to download package index: {err}"))?;
    let json = String::from_utf8(bytes)
        .map_err(|err| format!("package index is not valid UTF-8: {err}"))?;

    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(cache_path, &json);

    decode_index(&json)
}

/// Lists installed versions of `name` under the given package roots.
///
/// Each root uses the standard `{root}/preview/{name}/{version}` layout.
/// Versions are deduplicated; when every entry parses as a version they are
/// sorted newest-first, otherwise they are sorted lexically (descending).
pub fn installed_versions_in(roots: &[&Path], name: &str) -> Vec<String> {
    let mut versions: BTreeSet<String> = BTreeSet::new();
    for root in roots {
        let dir = root.join(PREVIEW_NAMESPACE).join(name);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if is_dir && let Ok(file_name) = entry.file_name().into_string() {
                versions.insert(file_name);
            }
        }
    }

    sort_versions(versions.into_iter().collect())
}

/// Lists installed versions of `name` across the system data and cache roots.
pub fn installed_versions(name: &str) -> Vec<String> {
    let data = FsPackages::system_data();
    let cache = FsPackages::system_cache();
    let roots: Vec<&Path> = [data.as_ref(), cache.as_ref()]
        .into_iter()
        .flatten()
        .map(FsPackages::path)
        .collect();
    installed_versions_in(&roots, name)
}

/// Downloads and installs a versioned package spec such as `@preview/foo:1.2.3`.
///
/// The package is stored in the system cache through the standard
/// `typst-kit` loader.
pub fn install_package(spec: &str) -> Result<(), String> {
    let spec: PackageSpec = spec
        .trim()
        .parse()
        .map_err(|err| format!("invalid package specification `{spec}`: {err}"))?;
    system_packages()
        .obtain(&spec)
        .map(|_| ())
        .map_err(|err| format!("failed to install {spec}: {err}"))
}

/// Removes an installed package version from the system **cache** root.
///
/// Only `{cache_root}/preview/{name}/{version}` is touched; the system data
/// directory is never modified. A missing directory is not an error. Both
/// `name` and `version` are validated by parsing a package spec, which rejects
/// path separators and prevents escaping the cache root.
pub fn uninstall_package(name: &str, version: &str) -> Result<(), String> {
    let spec: PackageSpec = format!("@{PREVIEW_NAMESPACE}/{name}:{version}")
        .parse()
        .map_err(|err| format!("invalid package name or version: {err}"))?;

    let cache = FsPackages::system_cache()
        .ok_or_else(|| "package cache directory is unavailable".to_owned())?;
    let cache_root = cache.path();

    let target = cache_root
        .join(&*spec.namespace)
        .join(&*spec.name)
        .join(spec.version.to_string());
    if !target.starts_with(cache_root) {
        return Err(format!(
            "refusing to remove `{}` outside the package cache",
            target.display()
        ));
    }

    match fs::remove_dir_all(&target) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("failed to remove {}: {err}", target.display())),
    }
}

fn decode_index(json: &str) -> Result<Vec<RegistryEntry>, String> {
    let values: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|err| format!("failed to parse package index: {err}"))?;
    Ok(values
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect())
}

fn read_fresh_cache(cache_path: &Path, max_age: Duration) -> Option<Vec<RegistryEntry>> {
    let modified = fs::metadata(cache_path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > max_age {
        return None;
    }
    let json = fs::read_to_string(cache_path).ok()?;
    decode_index(&json).ok()
}

fn sort_versions(versions: Vec<String>) -> Vec<String> {
    let parsed: Option<Vec<PackageVersion>> = versions
        .iter()
        .map(|version| version.parse().ok())
        .collect();
    match parsed {
        Some(parsed) => {
            let mut pairs: Vec<(PackageVersion, String)> =
                parsed.into_iter().zip(versions).collect();
            pairs.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
            pairs.into_iter().map(|(_, version)| version).collect()
        }
        None => {
            let mut versions = versions;
            versions.sort_by_key(|version| std::cmp::Reverse(version.clone()));
            versions
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const INDEX_FIXTURE: &str = r#"[
        {
            "name": "charged-ieee",
            "version": "0.1.0",
            "entrypoint": "lib.typ",
            "authors": ["Alice"],
            "license": "MIT",
            "description": "IEEE style",
            "homepage": null,
            "keywords": ["ieee"],
            "categories": ["paper"],
            "disciplines": ["computer-science"],
            "compiler": "0.11.0",
            "template": {
                "path": "template",
                "entrypoint": "main.typ",
                "thumbnail": "thumbnail.png"
            },
            "updatedAt": 1730000000
        },
        { "name": "wrong-version-type", "version": 123, "entrypoint": "lib.typ" },
        { "name": "missing-required-fields" },
        { "name": "minimal", "version": "2.3.4", "entrypoint": "lib.typ" }
    ]"#;

    #[test]
    fn parse_index_skips_malformed_entries() {
        let entries = parse_index(INDEX_FIXTURE);
        assert_eq!(entries.len(), 2);

        let full = &entries[0];
        assert_eq!(full.name, "charged-ieee");
        assert_eq!(full.version, "0.1.0");
        assert_eq!(full.authors, vec!["Alice"]);
        assert_eq!(full.license.as_deref(), Some("MIT"));
        assert_eq!(
            full.template
                .as_ref()
                .map(|entry| entry.entrypoint.as_str()),
            Some("main.typ")
        );
        assert_eq!(full.updated_at, Some(1730000000));

        let minimal = &entries[1];
        assert_eq!(minimal.name, "minimal");
        assert!(minimal.authors.is_empty());
        assert!(minimal.license.is_none());
        assert!(minimal.template.is_none());
        assert!(minimal.updated_at.is_none());
    }

    #[test]
    fn parse_index_tolerates_invalid_documents() {
        assert!(parse_index("not json").is_empty());
        assert!(parse_index("{}").is_empty());
        assert!(parse_index("[]").is_empty());
    }

    #[test]
    fn load_index_prefers_fresh_cache() {
        let dir = temp_dir("cache");
        let path = dir.join("index.json");
        fs::write(&path, INDEX_FIXTURE).expect("write cache");

        let entries =
            load_index(&path, Duration::from_secs(3600), false).expect("fresh cache is used");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "charged-ieee");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installed_versions_scans_and_sorts_by_version() {
        let root = temp_dir("installed");
        let base = root.join("preview").join("demo");
        for version in ["0.1.0", "0.10.0", "0.2.0"] {
            fs::create_dir_all(base.join(version)).expect("create version dir");
        }
        fs::write(base.join("README"), b"stray file").expect("write stray file");

        let versions = installed_versions_in(&[root.as_path()], "demo");
        assert_eq!(versions, vec!["0.10.0", "0.2.0", "0.1.0"]);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn installed_versions_dedupes_across_roots() {
        let first = temp_dir("dedupe-a");
        let second = temp_dir("dedupe-b");
        for root in [&first, &second] {
            fs::create_dir_all(root.join("preview").join("demo").join("1.0.0"))
                .expect("create version dir");
        }

        let versions = installed_versions_in(&[first.as_path(), second.as_path()], "demo");
        assert_eq!(versions, vec!["1.0.0"]);

        fs::remove_dir_all(&first).ok();
        fs::remove_dir_all(&second).ok();
    }

    #[test]
    fn installed_versions_fall_back_to_lexical_order() {
        let root = temp_dir("lexical");
        let base = root.join("preview").join("demo");
        for name in ["nightly", "main"] {
            fs::create_dir_all(base.join(name)).expect("create version dir");
        }

        let versions = installed_versions_in(&[root.as_path()], "demo");
        assert_eq!(versions, vec!["nightly", "main"]);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn uninstall_rejects_unsafe_arguments() {
        assert!(uninstall_package("../evil", "1.0.0").is_err());
        assert!(uninstall_package("demo", "../../evil").is_err());
        assert!(uninstall_package("demo/x", "1.0.0").is_err());
        assert!(uninstall_package("demo", "1.0").is_err());
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-registry-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
