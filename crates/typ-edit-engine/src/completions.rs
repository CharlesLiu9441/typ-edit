//! Completion candidates that do not depend on the document: Typst syntax
//! keywords plus every name in the standard library (global and math scopes),
//! including one level of module members such as `grid.cell`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use typ_edit_model::{CharCount, CompletionItem, CompletionKind};
use typst::foundations::Value;

/// Syntax keywords, which are not scope values.
const KEYWORDS: &[&str] = &[
    "let", "set", "show", "if", "else", "for", "in", "while", "break", "continue", "return",
    "import", "include", "as", "context", "none", "auto", "true", "false",
];

/// The keyword candidates (cheap to rebuild, no caching needed).
pub fn keyword_completions() -> Vec<CompletionItem> {
    KEYWORDS
        .iter()
        .map(|name| CompletionItem::plain(*name, CompletionKind::Keyword))
        .collect()
}

/// Longest doc summary kept on a candidate, in chars.
const DOCS_MAX_CHARS: usize = 120;

/// Every name in the Typst standard library. Built once and shared.
///
/// Beyond the top-level bindings of the global and math scopes, this also
/// descends one level into scope-carrying values (`Module` and scoped `Func` /
/// `Type` values such as `grid`), emitting each member as `"{owner}.{member}"`.
/// A member that is itself a module is emitted as a plain name and not
/// descended into further. Library values expose their documentation, so every
/// item carries a one-line summary in [`CompletionItem::docs`].
pub fn library_completions() -> Arc<Vec<CompletionItem>> {
    static CACHE: OnceLock<Arc<Vec<CompletionItem>>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut items: Vec<CompletionItem> = crate::stdlib::entries()
                .iter()
                .map(|entry| {
                    let mut item = CompletionItem::plain(entry.key.as_str(), entry.kind);
                    // A function/element is callable: accepting it opens the
                    // call so the parameter popup can reopen at the caret.
                    if entry.kind.is_callable() {
                        item.insert.push('(');
                    }
                    item.docs = doc_summary(entry.value.docs());
                    item
                })
                .collect();
            items.sort_by(|a, b| a.name.cmp(&b.name));
            items.dedup_by(|a, b| a.name == b.name);
            Arc::new(items)
        })
        .clone()
}

/// First non-empty line of a value's docs, trimmed and capped for the popup.
fn doc_summary(docs: Option<&str>) -> Option<String> {
    let line = docs?.lines().find(|line| !line.trim().is_empty())?.trim();
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() <= DOCS_MAX_CHARS {
        line.to_owned()
    } else {
        let mut out: String = line.chars().take(DOCS_MAX_CHARS - 1).collect();
        out.push('…');
        out
    })
}

/// The named parameters of a standard-library function value.
///
/// Empty for values that are not functions and for functions without usable
/// named parameters. Only parameters that can be passed by name are listed;
/// `self` receivers are skipped.
fn named_function_params(value: &Value) -> Vec<CompletionItem> {
    let Value::Func(func) = value else {
        return Vec::new();
    };
    func.params()
        .filter(|param| param.named())
        .filter_map(|param| param.name().map(str::to_owned))
        .filter(|name| name != "self")
        .map(|name| CompletionItem {
            insert: format!("{name}: "),
            name,
            kind: CompletionKind::Param,
            cursor_back: CharCount(0),
            docs: None,
        })
        .collect()
}

/// The named parameters of a standard-library function, built once and shared.
///
/// Top-level functions are keyed by their bare name (`"figure"`); functions
/// reachable through a scope are additionally keyed by their full path
/// (`"grid.cell"`). `None` for unknown names and for values without usable
/// parameters.
pub fn function_params(name: &str) -> Option<&'static [CompletionItem]> {
    static CACHE: OnceLock<HashMap<String, Vec<CompletionItem>>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut cache: HashMap<String, Vec<CompletionItem>> = HashMap::new();
            for entry in crate::stdlib::entries() {
                let items = named_function_params(&entry.value);
                if !items.is_empty() {
                    cache.entry(entry.key.clone()).or_insert(items);
                }
            }
            cache
        })
        .get(name)
        .map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_params_map_library_functions_to_their_named_parameters() {
        let params = function_params("pagebreak").expect("pagebreak is a library function");
        let weak = params
            .iter()
            .find(|item| item.name == "weak")
            .expect("pagebreak has a `weak` parameter");
        assert_eq!(weak.kind, CompletionKind::Param);
        assert_eq!(weak.insert, "weak: ");
        assert_eq!(weak.cursor_back, CharCount(0));
        assert!(params.iter().any(|item| item.name == "to"));

        assert!(function_params("definitely-not-a-typst-function").is_none());
        assert!(function_params("none").is_none());
    }

    #[test]
    fn library_completions_include_module_members() {
        let items = library_completions();
        let has = |name: &str| items.iter().any(|item| item.name == name);

        assert!(has("grid"), "the `grid` binding itself must remain");
        assert!(
            has("grid.cell"),
            "`grid.cell` must be offered from the library scope"
        );

        let grid_members: Vec<&str> = items
            .iter()
            .map(|item| item.name.as_str())
            .filter(|name| name.starts_with("grid."))
            .collect();
        assert!(
            grid_members.contains(&"grid.cell"),
            "discovered grid members: {grid_members:?}"
        );
        assert!(
            grid_members.contains(&"grid.header"),
            "discovered grid members: {grid_members:?}"
        );

        let math_members: Vec<&str> = items
            .iter()
            .map(|item| item.name.as_str())
            .filter(|name| name.starts_with("math."))
            .collect();
        assert!(
            !math_members.is_empty(),
            "the `math` module must contribute members"
        );
    }

    #[test]
    fn library_completions_attach_docs_summaries() {
        let items = library_completions();
        let text = items
            .iter()
            .find(|item| item.name == "text")
            .expect("`text` binding");
        let docs = text.docs.as_deref().expect("`text` has docs");
        assert!(!docs.is_empty());
        assert!(!docs.contains('\n'), "summary must be one line: {docs:?}");

        assert!(
            items.iter().any(|item| item.docs.is_some()),
            "the library must expose at least one documented value"
        );
        assert!(keyword_completions().iter().all(|item| item.docs.is_none()));
    }

    #[test]
    fn param_value_items_offer_colors_for_fill_and_units_for_size() {
        let fill = crate::values::param_value_items("rect", "fill");
        assert!(!fill.is_empty(), "rect.fill must offer candidates");
        assert!(fill.iter().any(|item| item.name == "rgb()"));
        assert!(fill.iter().any(|item| item.name == "red"));

        let size = crate::values::param_value_items("text", "size");
        assert!(!size.is_empty(), "text.size must offer candidates");
        assert!(size.iter().any(|item| item.name == "0pt"));

        assert!(crate::values::param_value_items("rect", "unknown-param").is_empty());
        assert!(crate::values::param_value_items("not-a-function", "fill").is_empty());
        assert!(crate::values::param_value_items("figure", "fill").is_empty());
    }

    #[test]
    fn cast_items_deduplicate_union_candidates_by_name() {
        use typst::foundations::{CastInfo, NoneValue, Type};

        let union = CastInfo::Union(vec![
            CastInfo::Type(Type::of::<NoneValue>()),
            CastInfo::Type(Type::of::<NoneValue>()),
        ]);
        let items = crate::values::cast_items(&union);
        assert_eq!(items.iter().filter(|item| item.name == "none").count(), 1);
    }

    #[test]
    fn library_completions_classify_by_typst_kind() {
        let items = library_completions();
        let kind_of = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .map(|item| item.kind)
        };
        assert_eq!(kind_of("heading"), Some(CompletionKind::Element), "heading");
        assert_eq!(kind_of("table"), Some(CompletionKind::Element), "table");
        assert_eq!(kind_of("black"), Some(CompletionKind::Value), "black");
        assert_eq!(kind_of("eq"), Some(CompletionKind::MathSymbol), "eq");
        assert_eq!(kind_of("delta"), Some(CompletionKind::MathSymbol), "delta");
        assert_eq!(kind_of("sqrt"), Some(CompletionKind::MathFunction), "sqrt");
    }

    #[test]
    fn library_completions_open_a_call_for_callables_only() {
        let items = library_completions();
        let insert_of = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .map(|item| item.insert.clone())
        };
        assert_eq!(insert_of("heading").as_deref(), Some("heading("));
        assert_eq!(insert_of("pagebreak").as_deref(), Some("pagebreak("));
        assert_eq!(insert_of("sqrt").as_deref(), Some("sqrt("));
        assert_eq!(insert_of("delta").as_deref(), Some("delta"));
        assert_eq!(insert_of("black").as_deref(), Some("black"));
    }

    #[test]
    fn library_completions_keep_both_symbol_cases() {
        let items = library_completions();
        assert!(items.iter().any(|item| item.name == "delta"));
        assert!(items.iter().any(|item| item.name == "Delta"));
    }

    #[test]
    fn library_completions_offer_dotted_math_symbol_variants() {
        let items = library_completions();
        let has = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .map(|item| item.kind)
        };
        assert_eq!(has("plus.minus"), Some(CompletionKind::MathSymbol));
        assert_eq!(has("eq.ast"), Some(CompletionKind::MathSymbol));
        assert_eq!(has("arrow.r"), Some(CompletionKind::MathSymbol));
    }

    #[test]
    fn function_params_resolve_scoped_member_functions() {
        let params =
            function_params("grid.cell").expect("`grid.cell` is a scoped library function");
        assert!(!params.is_empty());
        assert!(
            params.iter().all(|item| item.kind == CompletionKind::Param),
            "all `grid.cell` candidates must be parameters"
        );
        let names: Vec<&str> = params.iter().map(|item| item.name.as_str()).collect();
        assert!(
            names.contains(&"align"),
            "discovered grid.cell params: {names:?}"
        );
        assert!(
            names.contains(&"rowspan"),
            "discovered grid.cell params: {names:?}"
        );
    }
}
