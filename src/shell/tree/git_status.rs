//! Git status lookup for the project tree.
//!
//! Statuses arrive from the git worker keyed by repository-relative,
//! forward-slashed paths; the tree works with absolute workspace paths, so every
//! lookup normalizes through [`repo_relative`] first. A directory shows the most
//! severe status among its tracked descendants.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use typ_edit_git::StatusKind;

/// Borrowed status data threaded into the tree renderer while git is enabled.
#[derive(Clone, Copy)]
pub struct GitTreeStatus<'a> {
    pub repo_root: &'a Path,
    pub statuses: &'a BTreeMap<PathBuf, StatusKind>,
}

/// Whether git badges should be threaded into the tree at all.
pub fn badges_available(enabled: bool, has_repo: bool) -> bool {
    enabled && has_repo
}

/// Forward-slashed path of `path` relative to `repo_root`; `None` when `path`
/// is not inside the repository.
pub fn repo_relative(repo_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(repo_root).ok()?;
    let mut out = String::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&part.to_string_lossy());
    }
    Some(out)
}

/// Status of `path`, resolved through [`repo_relative`].
pub fn status_for(
    repo_root: &Path,
    statuses: &BTreeMap<PathBuf, StatusKind>,
    path: &Path,
) -> Option<StatusKind> {
    let relative = repo_relative(repo_root, path)?;
    statuses.get(Path::new(&relative)).copied()
}

/// The most severe status among the tracked descendants of `dir`.
pub fn dir_status(
    repo_root: &Path,
    statuses: &BTreeMap<PathBuf, StatusKind>,
    dir: &Path,
) -> Option<StatusKind> {
    let relative = repo_relative(repo_root, dir)?;
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{relative}/")
    };
    statuses
        .iter()
        .filter(|(path, _)| {
            path.to_string_lossy()
                .replace('\\', "/")
                .starts_with(&prefix)
        })
        .map(|(_, kind)| *kind)
        .max_by_key(|kind| severity(*kind))
}

/// Relative severity used to pick a directory's aggregate marker.
fn severity(kind: StatusKind) -> u8 {
    match kind {
        StatusKind::Deleted => 3,
        StatusKind::Modified => 2,
        StatusKind::Added => 1,
        StatusKind::Untracked => 0,
    }
}

/// The single-letter badge for `kind`.
pub fn badge(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Added => "A",
        StatusKind::Modified => "M",
        StatusKind::Deleted => "D",
        StatusKind::Untracked => "U",
    }
}

/// The i18n key of `kind`'s tooltip.
pub fn tooltip_key(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Added => "git.status.added",
        StatusKind::Modified => "git.status.modified",
        StatusKind::Deleted => "git.status.deleted",
        StatusKind::Untracked => "git.status.untracked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typ_edit_i18n::t;

    fn map(entries: &[(&str, StatusKind)]) -> BTreeMap<PathBuf, StatusKind> {
        entries
            .iter()
            .map(|(path, kind)| (PathBuf::from(path), *kind))
            .collect()
    }

    #[test]
    fn repo_relative_normalizes_separators_and_rejects_outside_paths() {
        let root = Path::new("C:/proj");
        assert_eq!(
            repo_relative(root, Path::new("C:/proj/chapters/a.typ")),
            Some("chapters/a.typ".to_owned())
        );
        assert_eq!(
            repo_relative(root, Path::new("C:/proj")),
            Some(String::new())
        );
        assert_eq!(repo_relative(root, Path::new("C:/other/a.typ")), None);
    }

    #[test]
    fn status_for_maps_a_workspace_path_to_its_repo_relative_entry() {
        let root = Path::new("C:/proj");
        let statuses = map(&[
            ("chapters/a.typ", StatusKind::Modified),
            ("b.typ", StatusKind::Untracked),
        ]);
        assert_eq!(
            status_for(root, &statuses, Path::new("C:/proj/chapters/a.typ")),
            Some(StatusKind::Modified)
        );
        assert_eq!(
            status_for(root, &statuses, Path::new("C:/proj/b.typ")),
            Some(StatusKind::Untracked)
        );
        assert_eq!(
            status_for(root, &statuses, Path::new("C:/proj/missing.typ")),
            None
        );
        assert_eq!(
            status_for(root, &statuses, Path::new("C:/other/a.typ")),
            None
        );
    }

    #[test]
    fn dir_status_picks_the_most_severe_descendant() {
        let root = Path::new("C:/proj");
        let statuses = map(&[
            ("chapters/a.typ", StatusKind::Untracked),
            ("chapters/b.typ", StatusKind::Modified),
            ("chapters/sub/c.typ", StatusKind::Added),
            ("other.typ", StatusKind::Deleted),
        ]);
        assert_eq!(
            dir_status(root, &statuses, Path::new("C:/proj/chapters")),
            Some(StatusKind::Modified)
        );
        assert_eq!(
            dir_status(root, &statuses, Path::new("C:/proj/chapters/sub")),
            Some(StatusKind::Added)
        );
        assert_eq!(
            dir_status(root, &statuses, Path::new("C:/proj")),
            Some(StatusKind::Deleted),
            "the repository root aggregates every descendant"
        );
        assert_eq!(dir_status(root, &statuses, Path::new("C:/proj/none")), None);
    }

    #[test]
    fn dir_aggregation_is_not_confused_by_a_sibling_prefix() {
        let root = Path::new("C:/proj");
        let statuses = map(&[
            ("chapters/a.typ", StatusKind::Modified),
            ("chapters-old/b.typ", StatusKind::Deleted),
        ]);
        assert_eq!(
            dir_status(root, &statuses, Path::new("C:/proj/chapters")),
            Some(StatusKind::Modified)
        );
    }

    #[test]
    fn badge_and_tooltip_cover_every_kind() {
        for (kind, badge, key) in [
            (StatusKind::Added, "A", "git.status.added"),
            (StatusKind::Modified, "M", "git.status.modified"),
            (StatusKind::Deleted, "D", "git.status.deleted"),
            (StatusKind::Untracked, "U", "git.status.untracked"),
        ] {
            assert_eq!(super::badge(kind), badge);
            assert_eq!(tooltip_key(kind), key);
        }
    }

    #[test]
    fn every_tooltip_key_resolves() {
        for kind in [
            StatusKind::Added,
            StatusKind::Modified,
            StatusKind::Deleted,
            StatusKind::Untracked,
        ] {
            let key = tooltip_key(kind);
            assert_ne!(&*t(key), key, "unresolved i18n key: {key}");
        }
    }
}
