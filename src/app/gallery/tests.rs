//! Unit tests for the gallery's pure helpers.

use super::model::{available_tags, filter_entries};
use crate::shell::settings_window::package_view::{filter_groups, format_updated, package_groups};
use std::collections::BTreeMap;
use typ_edit_engine::registry::{RegistryEntry, TemplateEntry};

fn sample(name: &str, version: &str) -> RegistryEntry {
    RegistryEntry {
        name: name.to_owned(),
        version: version.to_owned(),
        entrypoint: "lib.typ".to_owned(),
        authors: Vec::new(),
        license: None,
        description: None,
        homepage: None,
        repository: None,
        keywords: Vec::new(),
        categories: Vec::new(),
        disciplines: Vec::new(),
        compiler: None,
        template: None,
        updated_at: None,
    }
}

fn with_template(entry: &mut RegistryEntry) {
    entry.template = Some(TemplateEntry {
        path: "template".to_owned(),
        entrypoint: "main.typ".to_owned(),
        thumbnail: None,
    });
}

/// Installed versions map with a single name present.
fn installed_with(name: &str, versions: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    map.insert(
        name.to_owned(),
        versions
            .iter()
            .map(|version| (*version).to_owned())
            .collect(),
    );
    map
}

#[test]
fn filter_entries_matches_name_description_authors_and_categories() {
    let mut ieee = sample("charged-ieee", "0.1.0");
    ieee.description = Some("IEEE style paper".to_owned());
    let mut formal = sample("formal-math", "2.0.0");
    formal.authors = vec!["Alice".to_owned()];
    formal.categories = vec!["report".to_owned()];
    let entries = vec![ieee, formal];

    assert_eq!(filter_entries(&entries, "", None).len(), 2);
    assert_eq!(filter_entries(&entries, "  ", None).len(), 2);
    assert_eq!(
        filter_entries(&entries, "IEEE", None)[0].name,
        "charged-ieee"
    );
    assert_eq!(
        filter_entries(&entries, "alice", None)[0].name,
        "formal-math"
    );
    assert_eq!(
        filter_entries(&entries, "report", None)[0].name,
        "formal-math"
    );
    assert!(filter_entries(&entries, "missing", None).is_empty());
}

#[test]
fn filter_entries_combines_the_query_and_tag() {
    let mut report = sample("charged-ieee", "0.1.0");
    report.categories = vec!["report".to_owned()];
    let mut paper = sample("formal-math", "2.0.0");
    paper.categories = vec!["paper".to_owned()];
    let entries = vec![report, paper];

    assert_eq!(filter_entries(&entries, "", Some("report")).len(), 1);
    assert_eq!(
        filter_entries(&entries, "", Some("report"))[0].name,
        "charged-ieee"
    );
    assert_eq!(filter_entries(&entries, "charged", Some("report")).len(), 1);
    assert!(filter_entries(&entries, "charged", Some("paper")).is_empty());
    // An empty tag is treated as no tag filter.
    assert_eq!(filter_entries(&entries, "", Some("  ")).len(), 2);
}

#[test]
fn available_tags_dedupes_count_sorts_and_caps() {
    let mut entries = Vec::new();
    for (name, tags) in [
        ("a", vec!["report", "paper"]),
        ("b", vec!["report"]),
        ("c", vec!["report"]),
        ("d", vec!["paper"]),
        ("e", vec!["thesis"]),
    ] {
        let mut entry = sample(name, "1.0.0");
        entry.categories = tags.into_iter().map(str::to_owned).collect();
        with_template(&mut entry);
        entries.push(entry);
    }
    // An entry without a template must not contribute tags.
    let mut no_template = sample("f", "1.0.0");
    no_template.categories = vec!["ignored".to_owned()];
    entries.push(no_template);

    assert_eq!(available_tags(&entries), vec!["report", "paper", "thesis"]);
}

#[test]
fn available_tags_is_capped_at_twelve() {
    let mut entry = sample("many", "1.0.0");
    entry.categories = (0..20).map(|index| format!("tag{index:02}")).collect();
    with_template(&mut entry);
    assert_eq!(available_tags(&[entry]).len(), 12);
}

#[test]
fn package_groups_sorts_versions_newest_first_and_groups_by_name() {
    let entries = vec![
        sample("demo", "0.1.0"),
        sample("other", "1.0.0"),
        sample("demo", "0.10.0"),
        sample("demo", "0.2.0"),
    ];
    let groups = package_groups(&entries);
    let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
    assert_eq!(names, vec!["demo", "other"]);

    let demo = &groups[0];
    let versions: Vec<&str> = demo
        .entries
        .iter()
        .map(|entry| entry.version.as_str())
        .collect();
    assert_eq!(versions, vec!["0.10.0", "0.2.0", "0.1.0"]);
    assert_eq!(demo.latest().version, "0.10.0");
}

#[test]
fn filter_groups_matches_name_and_latest_description() {
    let mut acme = sample("acme", "1.0.0");
    acme.description = Some("A chart library".to_owned());
    let groups = package_groups(&[acme, sample("zeta", "0.1.0")]);
    let installed = BTreeMap::new();

    assert_eq!(filter_groups(&groups, "", false, &installed).len(), 2);
    assert_eq!(
        filter_groups(&groups, "acme", false, &installed)[0].name,
        "acme"
    );
    assert_eq!(
        filter_groups(&groups, "chart", false, &installed)[0].name,
        "acme"
    );
    assert!(filter_groups(&groups, "nope", false, &installed).is_empty());
}

#[test]
fn filter_groups_hides_non_installed_groups_when_installed_only() {
    let groups = package_groups(&[sample("acme", "1.0.0"), sample("zeta", "0.1.0")]);
    let installed = installed_with("acme", &["1.0.0"]);

    let filtered = filter_groups(&groups, "", true, &installed);
    let names: Vec<&str> = filtered.iter().map(|group| group.name.as_str()).collect();
    assert_eq!(names, vec!["acme"]);

    // Nothing installed and the filter on: an empty result (rendered as the empty state).
    let none = BTreeMap::new();
    assert!(filter_groups(&groups, "", true, &none).is_empty());

    // A package with an empty installed list is treated as not installed.
    let mut empty = BTreeMap::new();
    empty.insert("acme".to_owned(), Vec::new());
    assert!(filter_groups(&groups, "", true, &empty).is_empty());
}

#[test]
fn format_updated_formats_known_timestamps() {
    assert_eq!(format_updated(None), "");
    assert_eq!(format_updated(Some(0)), "1970-01-01");
    assert_eq!(format_updated(Some(1_700_000_000)), "2023-11-14");
    assert_eq!(format_updated(Some(1_000_000_000)), "2001-09-09");
}

#[test]
fn templates_are_the_entries_with_a_template_section() {
    let mut alpha = sample("alpha", "1.0.0");
    with_template(&mut alpha);
    let entries = [alpha, sample("beta", "1.0.0")];
    let templates: Vec<&RegistryEntry> = entries
        .iter()
        .filter(|entry| entry.template.is_some())
        .collect();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "alpha");
}
