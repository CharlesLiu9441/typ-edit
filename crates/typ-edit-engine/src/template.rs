//! Template instantiation, equivalent to `typst init`.
//!
//! A template package declares a `[template]` section in its `typst.toml`
//! manifest. Instantiation copies the contents of the declared template
//! directory into a destination project directory and reports the entrypoint.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use typst::syntax::package::{PackageManifest, PackageSpec, VersionlessPackageSpec};

use crate::packages::system_packages;

/// The result of instantiating a template.
#[derive(Clone, Debug)]
pub struct TemplateInit {
    /// The project root that was created or populated.
    pub root: PathBuf,
    /// The entrypoint file, relative to [`root`](Self::root).
    pub entrypoint: String,
}

/// Resolves a package spec to a concrete version.
///
/// A fully versioned spec such as `@preview/foo:1.2.3` is returned as-is. A
/// versionless spec such as `@preview/foo` is resolved to the latest published
/// version through the registry.
pub fn resolve_spec(spec: &str) -> Result<PackageSpec, String> {
    let spec = spec.trim();
    if let Ok(concrete) = spec.parse::<PackageSpec>() {
        return Ok(concrete);
    }

    let versionless: VersionlessPackageSpec = spec
        .parse()
        .map_err(|err| format!("invalid package specification `{spec}`: {err}"))?;
    let version = system_packages()
        .latest_version(&versionless)
        .map_err(|err| format!("failed to determine the latest version of {versionless}: {err}"))?;
    Ok(versionless.at(version))
}

/// Instantiates a template package into `dest`.
///
/// Rejects a `dest` that exists and is non-empty. The package is obtained
/// through the standard loader, its manifest is parsed and validated, and the
/// contents of the template directory are copied recursively into `dest`.
pub fn init_project(template_spec: &str, dest: &Path) -> Result<TemplateInit, String> {
    let has_entries = dir_has_entries(dest)
        .map_err(|err| format!("failed to inspect `{}`: {err}", dest.display()))?;
    if has_entries {
        return Err(format!(
            "destination `{}` already exists and is not empty",
            dest.display()
        ));
    }

    let spec = resolve_spec(template_spec)?;
    let package = system_packages()
        .obtain(&spec)
        .map_err(|err| format!("failed to download {spec}: {err}"))?;

    let manifest_path = package.path().join("typst.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read {}: {err}", manifest_path.display()))?;
    let manifest: PackageManifest = toml::from_str(&manifest_text)
        .map_err(|err| format!("failed to parse {}: {err}", manifest_path.display()))?;
    manifest
        .validate(&spec)
        .map_err(|err| format!("package manifest is invalid: {err}"))?;

    let template = manifest
        .template
        .ok_or_else(|| format!("{spec} is not a template package"))?;
    let source = package.path().join(template.path.as_str());
    copy_dir_contents(&source, dest)
        .map_err(|err| format!("failed to copy template into `{}`: {err}", dest.display()))?;

    Ok(TemplateInit {
        root: dest.to_path_buf(),
        entrypoint: template.entrypoint.to_string(),
    })
}

fn dir_has_entries(dir: &Path) -> io::Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    Ok(fs::read_dir(dir)?.next().is_some())
}

fn copy_dir_contents(source: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_contents(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    const SAMPLE_MANIFEST: &str = r#"
[package]
name = "demo"
version = "0.1.0"
entrypoint = "lib.typ"

[template]
path = "template"
entrypoint = "main.typ"
thumbnail = "thumbnail.png"
"#;

    #[test]
    fn parses_manifest_with_template() {
        let manifest: PackageManifest = toml::from_str(SAMPLE_MANIFEST).expect("parse manifest");
        assert_eq!(manifest.package.name, "demo");

        let spec: PackageSpec = "@preview/demo:0.1.0".parse().expect("parse spec");
        assert!(manifest.validate(&spec).is_ok());

        let template = manifest.template.expect("template section");
        assert_eq!(template.path, "template");
        assert_eq!(template.entrypoint, "main.typ");
    }

    #[test]
    fn resolve_spec_rejects_malformed_input() {
        assert!(resolve_spec("not-a-spec").is_err());
        assert!(resolve_spec("").is_err());
        assert!(resolve_spec("preview/demo").is_err());
    }

    #[test]
    fn copies_directory_recursively() {
        let dir = temp_dir("copy");
        let source = dir.join("source");
        fs::create_dir_all(source.join("sub")).expect("create dirs");
        fs::write(source.join("main.typ"), b"= Hello").expect("write main");
        fs::write(source.join("sub").join("lib.typ"), b"library").expect("write lib");

        let dest = dir.join("dest");
        copy_dir_contents(&source, &dest).expect("copy");

        assert_eq!(
            fs::read_to_string(dest.join("main.typ")).expect("read main"),
            "= Hello"
        );
        assert_eq!(
            fs::read_to_string(dest.join("sub").join("lib.typ")).expect("read lib"),
            "library"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_non_empty_destination() {
        let dir = temp_dir("nonempty");
        fs::write(dir.join("existing.typ"), b"x").expect("write");

        let err = init_project("@preview/demo:0.1.0", &dir).expect_err("must reject");
        assert!(err.contains("not empty"));

        fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "typ-edit-template-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
