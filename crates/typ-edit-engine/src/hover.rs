//! Semantic hover: function signatures and named-parameter accepted types.
//!
//! Built on top of [`typst_ide::tooltip`] (docs, labels, imports, value reprs),
//! which already powers the editor's tooltip. The analysis runs only when the
//! app asks for a byte it has not cached for the current compile generation.

use typst::foundations::{CastInfo, Func, ParamInfo, Repr, Value};
use typst::syntax::ast::{self, AstNode};
use typst::syntax::{LinkedNode, Side, Source, SyntaxKind};
use typst_ide::Tooltip;
use typst_layout::PagedDocument;

use super::world::WorldState;

/// Parameters rendered before the signature is elided with `…`.
const MAX_SIGNATURE_PARAMS: usize = 10;

/// Hover content for one byte offset. Every field is optional; [`hover_info`]
/// only returns a value with at least one of them set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HoverInfo {
    /// Function signature (enhanced) or a partially-applied call repr.
    pub signature: Option<String>,
    /// Types a hovered named parameter accepts.
    pub accepts: Option<String>,
    /// `typst_ide` summary: docs, label text, import list or value repr.
    pub docs: Option<String>,
}

impl HoverInfo {
    /// Whether every field is empty.
    pub fn is_empty(&self) -> bool {
        self.signature.is_none() && self.accepts.is_none() && self.docs.is_none()
    }
}

/// Describes the item under `byte`, or `None` when there is nothing to show.
///
/// `document` is the last successful compilation (label tooltips need it) and
/// `source` must be the world's source for the file `byte` belongs to.
pub fn hover_info(
    world: &WorldState,
    document: &PagedDocument,
    source: &Source,
    byte: usize,
) -> Option<HoverInfo> {
    let mut info = HoverInfo::default();

    // Existing typst-ide content: function/param docs, label references, import
    // lists, value reprs and the partially-applied call repr.
    if let Some(tooltip) = typst_ide::tooltip(world, Some(document), source, byte, Side::Before) {
        match tooltip {
            Tooltip::Text(text) => info.docs = Some(text.into()),
            Tooltip::Code(code) => info.signature = Some(code.into()),
        }
    }

    if let Some(leaf) = LinkedNode::new(source.root()).leaf_at(byte, Side::Before) {
        // A real signature beats the partial-call repr from `tooltip`.
        if info.signature.is_none()
            && let Some(signature) = function_signature(world, &leaf)
        {
            info.signature = Some(signature);
        }
        if info.accepts.is_none()
            && let Some(accepts) = named_param_accepts(world, &leaf)
        {
            info.accepts = Some(accepts);
        }
    }

    (!info.is_empty()).then_some(info)
}

/// Signature of the function the leaf resolves to, when it is one.
///
/// Mirrors `typst_ide`'s expression tooltip walk: only embeddable code and math
/// identifiers are analyzed, and only an unambiguous function value qualifies.
fn function_signature(world: &WorldState, leaf: &LinkedNode) -> Option<String> {
    let mut ancestor = leaf;
    while !ancestor.is::<ast::Expr>() {
        ancestor = ancestor.parent()?;
    }
    let expr = ancestor.cast::<ast::Expr>()?;
    let embeddable = expr.hash()
        || matches!(
            expr,
            ast::Expr::MathIdent(_) | ast::Expr::MathFieldAccess(_) | ast::Expr::MathCall(_)
        );
    if !embeddable {
        return None;
    }

    let values = typst_ide::analyze_expr(world, ancestor);
    let [(value, _), rest @ ..] = values.as_slice() else {
        return None;
    };
    // A value passed through a branch has no single signature.
    if !rest.iter().all(|(other, _)| other == value) {
        return None;
    }
    let Value::Func(func) = value else {
        return None;
    };
    Some(func_signature(func))
}

/// Accepted types of the named parameter under the leaf, when the leaf is the
/// parameter's name inside a resolved call or set rule.
fn named_param_accepts(world: &WorldState, leaf: &LinkedNode) -> Option<String> {
    if leaf.index() != 0 || !leaf.is::<ast::Ident>() {
        return None;
    }
    let parent = leaf.parent()?;
    parent.cast::<ast::Named>()?;
    let grand = parent.parent()?;
    if !matches!(grand.kind(), SyntaxKind::Args | SyntaxKind::MathArgs) {
        return None;
    }
    let grand_grand = grand.parent()?;
    let expr = grand_grand.cast::<ast::Expr>()?;
    let callee_span = match expr {
        ast::Expr::FuncCall(call) => call.callee().span(),
        ast::Expr::MathCall(call) => call.callee().span(),
        ast::Expr::SetRule(set) => set.target().span(),
        _ => return None,
    };
    let callee = grand_grand.find(callee_span)?;
    let values = typst_ide::analyze_expr(world, &callee);
    let (value, _) = values.as_slice().first()?;
    let func = value.clone().cast::<Func>().ok()?;

    // Closure parameters carry no type information.
    let param = func.param(&leaf.cast::<ast::Ident>()?)?;
    let native = param.to_native()?;
    let names = cast_type_names(&native.input);
    (!names.is_empty()).then_some(names)
}

/// Compact `name(param: type = …, ...)` rendering of a function.
fn func_signature(func: &Func) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut count = 0usize;
    for param in func.params() {
        count += 1;
        if entries.len() < MAX_SIGNATURE_PARAMS {
            entries.push(param_entry(&param));
        }
    }
    if count > MAX_SIGNATURE_PARAMS {
        entries.push("…".to_owned());
    }

    let body = entries.join(", ");
    match func.name() {
        Some(name) => format!("{name}({body})"),
        None => format!("({body})"),
    }
}

/// One parameter of [`func_signature`].
fn param_entry(param: &ParamInfo) -> String {
    match param {
        ParamInfo::Native(info) => {
            let types = cast_type_names(&info.input);
            let types = if types.is_empty() {
                "…".to_owned()
            } else {
                types
            };
            let variadic = if info.variadic { ".." } else { "" };
            let default = if info.default.is_some() { " = …" } else { "" };
            format!("{variadic}{}: {types}{default}", info.name)
        }
        ParamInfo::Closure(_) => {
            let variadic = if param.variadic() { ".." } else { "" };
            let name = param.name().unwrap_or("…");
            match param.default() {
                Some(default) => format!("{variadic}{name} = {}", default.repr()),
                None => format!("{variadic}{name}"),
            }
        }
        ParamInfo::Plugin => "..bytes".to_owned(),
    }
}

/// Human-readable, de-duplicated list of the values a cast accepts, e.g.
/// `color | gradient`. Union members are walked in declaration order.
fn cast_type_names(info: &CastInfo) -> String {
    let mut names: Vec<String> = Vec::new();
    crate::cast::for_each_accepted(info, |accepted| {
        let name = accepted.display();
        if !names.contains(&name) {
            names.push(name);
        }
    });
    names.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontCatalog;
    use std::sync::Arc;
    use typst::Library;
    use typst::LibraryExt;

    /// A stdlib function by name.
    fn stdlib_func(name: &str) -> Func {
        let library = Library::default();
        library
            .global
            .scope()
            .get(name)
            .unwrap_or_else(|| panic!("stdlib `{name}`"))
            .read()
            .clone()
            .cast::<Func>()
            .expect("function value")
    }

    #[test]
    fn box_signature_names_fill_as_color() {
        let signature = func_signature(&stdlib_func("box"));
        assert!(signature.starts_with("box("), "{signature}");
        assert!(signature.contains("fill: color"), "{signature}");
    }

    #[test]
    fn text_signature_names_size_as_length() {
        let signature = func_signature(&stdlib_func("text"));
        assert!(signature.starts_with("text("), "{signature}");
        assert!(signature.contains("size: length"), "{signature}");
    }

    #[test]
    fn cast_info_collects_types_and_values() {
        let func = stdlib_func("box");
        let param = func.param("fill").expect("fill param");
        let native = param.to_native().expect("native param");
        let names = cast_type_names(&native.input);
        assert!(names.contains("color"), "{names}");
    }

    #[test]
    fn signature_elides_long_parameter_lists() {
        let func = stdlib_func("box");
        let signature = func_signature(&func);
        let entries = signature.trim_end_matches(')').split(", ").count();
        assert!(entries <= MAX_SIGNATURE_PARAMS + 1, "{signature}");
    }

    /// Compiles `text` as the main file of a fresh world. The returned world
    /// owns the source the document was compiled from.
    fn compiled(text: &str) -> (WorldState, PagedDocument) {
        let root = std::env::temp_dir();
        let main = root.join("typ-edit-hover-test.typ");
        let world = WorldState::new(
            root,
            main.clone(),
            vec![(main, text.to_owned())],
            &FontCatalog::new(),
            Arc::new(crate::packages::system_packages()),
        );
        let document = typst::compile::<PagedDocument>(&world)
            .output
            .expect("hover test document compiles");
        (world, document)
    }

    #[test]
    fn hovers_function_parameter_and_value_offline() {
        let text = "#box(fill: red)";
        let (world, document) = compiled(text);
        let source = world
            .source_of(world.main_path())
            .expect("main source is available");

        // The editor reports the caret-side byte of the glyph under the pointer
        // (the token's end), which `Side::Before` resolves back to the token.
        let end_of = |token: &str| text.find(token).expect("token") + token.len();

        // The function: signature + docs.
        let info = hover_info(&world, &document, &source, end_of("box")).expect("hover `box`");
        assert!(
            info.signature
                .as_deref()
                .is_some_and(|signature| signature.contains("fill: color")),
            "{info:?}"
        );
        assert!(info.docs.is_some(), "{info:?}");

        // The named parameter: accepted types.
        let info = hover_info(&world, &document, &source, end_of("fill")).expect("hover `fill`");
        assert!(
            info.accepts.as_deref().is_some_and(|a| a.contains("color")),
            "{info:?}"
        );

        // A plain value: the repr as the `signature` fallback.
        let info = hover_info(&world, &document, &source, end_of("red")).expect("hover `red`");
        assert!(
            info.signature
                .as_deref()
                .is_some_and(|signature| signature.contains("rgb")),
            "{info:?}"
        );
    }
}
