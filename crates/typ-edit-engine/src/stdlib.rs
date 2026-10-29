//! Shared, once-built index over the default Typst standard library.
//!
//! Every binding of the global and math scopes — each top-level name plus one
//! level of its members (`grid.cell`) — is collected once and reused by the
//! completion, parameter and value-slot helpers, so the scope walk and the
//! default-library cache have a single maintainer.

use std::collections::HashSet;
use std::sync::OnceLock;

use typ_edit_model::CompletionKind;
use typst::LibraryExt;
use typst::foundations::{Symbol, Value};
use typst::utils::LazyHash;

/// One standard-library binding: its completion key (bare or dotted) and value.
pub(crate) struct StdlibEntry {
    /// The completion key: a top-level name (`grid`) or a member path (`grid.cell`).
    pub(crate) key: String,
    /// The Typst-aligned completion kind derived from the binding's value.
    pub(crate) kind: CompletionKind,
    /// The binding's value.
    pub(crate) value: Value,
}

/// The default library wrapped for hashing, built once process-wide.
///
/// Every world borrows this `&'static` value, so the library is built once for
/// the whole process instead of once per compilation.
pub(crate) fn lazy_library() -> &'static LazyHash<typst::Library> {
    static CACHE: OnceLock<LazyHash<typst::Library>> = OnceLock::new();
    CACHE.get_or_init(|| LazyHash::new(typst::Library::default()))
}

/// The default library, built once.
pub(crate) fn library() -> &'static typst::Library {
    lazy_library()
}

/// Every standard-library binding plus one level of its members, built once.
///
/// Order is the global scope then the math scope, each top-level binding
/// followed by its members in scope order. Math symbols additionally contribute
/// their dotted variants (`plus.minus`, `eq.ast`) so they can complete.
pub(crate) fn entries() -> &'static [StdlibEntry] {
    static CACHE: OnceLock<Vec<StdlibEntry>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut entries: Vec<StdlibEntry> = Vec::new();
        for (scope, math) in [
            (library().global.scope(), false),
            (library().math.scope(), true),
        ] {
            for (name, binding) in scope.iter() {
                let binding_value = binding.read();
                let value: &Value = binding_value;
                entries.push(StdlibEntry {
                    key: name.as_str().to_owned(),
                    kind: classify(math, value),
                    value: value.clone(),
                });
                if math && let Value::Symbol(symbol) = value {
                    push_symbol_variants(&mut entries, name.as_str(), symbol);
                }
                let Some(members) = value.scope() else {
                    continue;
                };
                for (member, member_binding) in members.iter() {
                    let member_value = member_binding.read();
                    let member_ref: &Value = member_value;
                    entries.push(StdlibEntry {
                        key: format!("{}.{}", name.as_str(), member.as_str()),
                        kind: classify(math, member_ref),
                        value: member_ref.clone(),
                    });
                }
            }
        }
        entries
    })
}

/// The Typst-aligned kind of a library binding.
///
/// Math scope: functions are math functions, everything else (symbols,
/// namespaces) is a math symbol. Global scope: native elements are `Element`,
/// other functions `Function`, types `Type`, and the remaining constants and
/// modules `Value`.
fn classify(math: bool, value: &Value) -> CompletionKind {
    if math {
        return match value {
            Value::Func(_) => CompletionKind::MathFunction,
            _ => CompletionKind::MathSymbol,
        };
    }
    match value {
        Value::Func(func) if func.to_element().is_some() => CompletionKind::Element,
        Value::Func(_) => CompletionKind::Function,
        Value::Type(_) => CompletionKind::Type,
        _ => CompletionKind::Value,
    }
}

/// Records every named variant of a math symbol as a dotted candidate.
fn push_symbol_variants(entries: &mut Vec<StdlibEntry>, name: &str, symbol: &Symbol) {
    for (modifiers, _, _) in symbol.variants() {
        let path = modifiers.iter().collect::<Vec<_>>().join(".");
        if path.is_empty() {
            continue;
        }
        entries.push(StdlibEntry {
            key: format!("{name}.{path}"),
            kind: CompletionKind::MathSymbol,
            value: Value::Symbol(symbol.clone()),
        });
    }
}

/// Whether `name` names a Typst math symbol or one of its dotted variants.
///
/// Backs the highlight rule that promotes a math identifier from the generic
/// interpolated variable to the dedicated symbol category.
pub(crate) fn is_math_symbol(name: &str) -> bool {
    math_symbols().contains(name)
}

/// Every math symbol name and dotted variant, built once.
fn math_symbols() -> &'static HashSet<String> {
    static CACHE: OnceLock<HashSet<String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut names = HashSet::new();
        for (name, binding) in library().math.scope().iter() {
            let value = binding.read();
            let Value::Symbol(symbol) = value else {
                continue;
            };
            names.insert(name.as_str().to_owned());
            for (modifiers, _, _) in symbol.variants() {
                let path = modifiers.iter().collect::<Vec<_>>().join(".");
                if !path.is_empty() {
                    names.insert(format!("{}.{}", name.as_str(), path));
                }
            }
        }
        names
    })
}
