//! World-free value-slot candidates for named arguments of standard-library
//! functions.
//!
//! The candidates come from a parameter's declared [`CastInfo`], mirroring the
//! world-free arms of `typst-ide`'s cast completion. Anything that needs fonts,
//! files, labels or the document (world) is intentionally skipped: only the
//! default library is consulted.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::cast::{Accepted, for_each_accepted, type_spec};
use typ_edit_model::{CharCount, CompletionItem, CompletionKind};
use typst::foundations::{CastInfo, Repr, Type, Value};

/// Candidates that satisfy a parameter's declared cast.
///
/// Mirrors the world-free arms of `typst-ide`'s cast completion: a [`Value`]
/// cast offers the value itself, a [`Type`] cast offers its constructors plus
/// every library value of that type, and a [`Union`](CastInfo::Union) offers
/// the concatenation of its members deduplicated by name (first wins). `Any`
/// offers nothing. World-dependent types (labels) are skipped.
pub(crate) fn cast_items(cast: &CastInfo) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = Vec::new();
    for_each_accepted(cast, |accepted| {
        for item in accepted_items(accepted) {
            if !out.iter().any(|existing| existing.name == item.name) {
                out.push(item);
            }
        }
    });
    out
}

/// Completion candidates for one accepted alternative.
fn accepted_items(accepted: Accepted<'_>) -> Vec<CompletionItem> {
    match accepted {
        Accepted::Any => Vec::new(),
        Accepted::Value(value, docs) => value_items(value, docs),
        Accepted::Type(ty) => type_items(ty),
    }
}

/// A value cast: the single value the parameter accepts, inserted verbatim.
fn value_items(value: &Value, docs: &'static str) -> Vec<CompletionItem> {
    let repr = value.repr();
    let name = match value {
        Value::Str(text) => text.as_str().to_owned(),
        _ => repr.as_str().to_owned(),
    };
    vec![CompletionItem {
        name,
        kind: CompletionKind::Value,
        insert: repr.as_str().to_owned(),
        cursor_back: CharCount(0),
        docs: Some(docs.to_owned()),
    }]
}

/// Candidates for a type cast, keyed by the type's long name.
fn type_items(ty: &Type) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = Vec::new();
    if let Some(spec) = type_spec(ty) {
        out.extend(spec.plain.iter().map(|(name, docs)| plain(name, docs)));
        out.extend(
            spec.constructors
                .iter()
                .map(|(insert, docs)| constructor(insert, docs)),
        );
        out.extend(units(spec.units));
    }
    for (name, binding) in crate::stdlib::library().global.scope().iter() {
        if binding.read().ty() == *ty {
            out.push(CompletionItem::plain(name.as_str(), CompletionKind::Value));
        }
    }
    out
}

/// A plain value candidate documented by `docs`.
fn plain(name: &str, docs: &str) -> CompletionItem {
    CompletionItem {
        name: name.to_owned(),
        kind: CompletionKind::Value,
        insert: name.to_owned(),
        cursor_back: CharCount(0),
        docs: Some(docs.to_owned()),
    }
}

/// A constructor candidate; the caret lands inside the parentheses.
fn constructor(insert: &str, docs: &str) -> CompletionItem {
    CompletionItem {
        name: insert.to_owned(),
        kind: CompletionKind::Value,
        insert: insert.to_owned(),
        cursor_back: CharCount(1),
        docs: Some(docs.to_owned()),
    }
}

/// Bare unit-bearing candidates, inserted as typed.
fn units(names: &[&str]) -> Vec<CompletionItem> {
    names
        .iter()
        .map(|name| CompletionItem::plain(*name, CompletionKind::Value))
        .collect()
}

/// Value candidates for the named parameter `param` of `callee`.
///
/// Built once from the default library's global and math scopes, keyed exactly
/// like [`crate::completions::function_params`]: a top-level function by its
/// bare name and a scoped member by its dotted path. Closures carry no cast and
/// contribute nothing; unknown keys yield an empty slice.
pub(crate) fn param_value_items(callee: &str, param: &str) -> &'static [CompletionItem] {
    static CACHE: OnceLock<HashMap<String, HashMap<String, Vec<CompletionItem>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        let mut cache: HashMap<String, HashMap<String, Vec<CompletionItem>>> = HashMap::new();
        for entry in crate::stdlib::entries() {
            collect_param_values(&entry.value, &entry.key, &mut cache);
        }
        cache
    });
    cache
        .get(callee)
        .and_then(|params| params.get(param))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The unit suffixes Typst's lexer accepts, and the only ones offered for a
/// numeric prefix. `grad`/`turn` are absent: they are not valid Typst units
/// even though some type tables mention them.
const VALID_UNITS: &[&str] = &["pt", "mm", "cm", "in", "em", "deg", "rad", "fr", "%"];

/// The digit-led numeric prefix of a candidate name (`0pt` -> `0`, `1fr` -> `1`).
fn numeric_prefix_of(name: &str) -> Option<&str> {
    let end = name
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(name.len());
    let digits = &name[..end];
    digits
        .starts_with(|c: char| c.is_ascii_digit())
        .then_some(digits)
}

/// The valid unit suffixes implied by an existing value-slot candidate set:
/// every candidate shaped `{number}{unit}` contributes its unit. This reads the
/// already-working cast-derived source instead of re-deriving types, so the unit
/// set follows the parameter's declared type (and excludes invalid suffixes).
pub(crate) fn unit_suffixes_from_items(items: &[CompletionItem]) -> Vec<&'static str> {
    VALID_UNITS
        .iter()
        .copied()
        .filter(|unit| {
            items
                .iter()
                .any(|item| item.name.ends_with(unit) && numeric_prefix_of(&item.name).is_some())
        })
        .collect()
}

/// Curated unit sets for common unit-accepting parameter names whose cast has no
/// world-free candidate table (so the source above yields nothing). A parameter
/// not listed here offers no numeric units.
fn curated_unit_suffixes(param: &str) -> &'static [&'static str] {
    const LENGTH: &[&str] = &["pt", "mm", "cm", "in", "em"];
    const ANGLE: &[&str] = &["deg", "rad"];
    const RATIO: &[&str] = &["%"];
    const FRACTION: &[&str] = &["fr"];
    match param {
        "size" | "width" | "height" | "radius" | "stroke" | "inset" | "margin" | "padding"
        | "spacing" | "length" | "gap" | "outset" | "indent" => LENGTH,
        "angle" => ANGLE,
        "ratio" => RATIO,
        "fraction" => FRACTION,
        _ => &[],
    }
}

/// The unit suffixes to offer when a number is typed in `param` of `callee`:
/// the cast-derived set when it exists, else the curated fallback. Empty when
/// the parameter does not accept units.
pub(crate) fn param_unit_suffixes(callee: &str, param: &str) -> Vec<&'static str> {
    let from_source = unit_suffixes_from_items(param_value_items(callee, param));
    if !from_source.is_empty() {
        return from_source;
    }
    curated_unit_suffixes(param).to_vec()
}

/// One candidate per unit, inserting the full token (`12` + `pt` -> `12pt`). The
/// candidate is named by the full token so the editor's prefix filter matches
/// the typed digits.
pub(crate) fn unit_items(number: &str, units: &[&'static str]) -> Vec<CompletionItem> {
    units
        .iter()
        .map(|unit| {
            let token = format!("{number}{unit}");
            CompletionItem {
                name: token.clone(),
                kind: CompletionKind::Value,
                insert: token,
                cursor_back: CharCount(0),
                docs: None,
            }
        })
        .collect()
}

/// Records the value candidates of every named parameter of a function value.
fn collect_param_values(
    value: &Value,
    callee: &str,
    cache: &mut HashMap<String, HashMap<String, Vec<CompletionItem>>>,
) {
    let Value::Func(func) = value else {
        return;
    };
    for param in func.params() {
        let Some(native) = param.to_native() else {
            continue;
        };
        let Some(name) = param.name() else {
            continue;
        };
        if !param.named() {
            continue;
        }
        let items = cast_items(&native.input);
        if items.is_empty() {
            continue;
        }
        cache
            .entry(callee.to_owned())
            .or_default()
            .entry(name.to_owned())
            .or_insert(items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_suffixes_follow_the_cast_source() {
        assert_eq!(
            param_unit_suffixes("text", "size"),
            ["pt", "mm", "cm", "in", "em"]
        );
        assert_eq!(param_unit_suffixes("rotate", "angle"), ["deg", "rad"]);
        let ratios = param_unit_suffixes("pad", "x");
        assert!(
            ratios.contains(&"%") && ratios.contains(&"em"),
            "{ratios:?}"
        );
        assert!(param_unit_suffixes("rect", "fill").is_empty());
    }

    #[test]
    fn curated_fallback_covers_named_unit_parameters() {
        assert_eq!(param_unit_suffixes("no-such-fn", "fraction"), ["fr"]);
        let size = param_unit_suffixes("no-such-fn", "size");
        assert_eq!(size, ["pt", "mm", "cm", "in", "em"]);
        assert!(param_unit_suffixes("no-such-fn", "fill").is_empty());
    }

    #[test]
    fn unit_items_insert_the_full_token() {
        let items = unit_items("12", &["pt", "em"]);
        let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["12pt", "12em"]);
        assert_eq!(items[0].insert, "12pt");
        assert_eq!(items[0].cursor_back, CharCount(0));
    }
}
