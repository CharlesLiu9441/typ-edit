//! Pure gallery helpers: template query filtering and category tags. No UI and
//! no I/O, so they are unit-tested directly.

use crate::shell::settings_window::package_view::entry_matches;
use typ_edit_engine::registry::RegistryEntry;

/// Registry entries whose name/description/authors/categories contain `query`,
/// case-insensitively, and whose categories contain `tag` (when one is given).
/// An empty/blank query and `None` tag return every entry.
pub(super) fn filter_entries<'a>(
    entries: &'a [RegistryEntry],
    query: &str,
    tag: Option<&str>,
) -> Vec<&'a RegistryEntry> {
    let needle = query.trim().to_lowercase();
    let tag = tag.map(str::trim).filter(|tag| !tag.is_empty());
    entries
        .iter()
        .filter(|entry| {
            (needle.is_empty() || entry_matches(entry, &needle)) && tag_matches(entry, tag)
        })
        .collect()
}

/// The distinct, count-sorted template categories (at most 12), for the chip
/// row above the template list. Only entries that ship a template count.
pub(super) fn available_tags(entries: &[RegistryEntry]) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.template.is_some()) {
        for category in &entry.categories {
            let category = category.trim();
            if !category.is_empty() {
                *counts.entry(category.to_owned()).or_default() += 1;
            }
        }
    }
    let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tags.into_iter().take(12).map(|(tag, _)| tag).collect()
}

/// Whether an entry carries `tag` in its categories (case-insensitive).
fn tag_matches(entry: &RegistryEntry, tag: Option<&str>) -> bool {
    match tag {
        None => true,
        Some(tag) => entry
            .categories
            .iter()
            .any(|category| category.eq_ignore_ascii_case(tag)),
    }
}
