//! Completion extraction: document definitions and imports.

use std::collections::HashSet;

use super::MAX_DEFINITIONS;
use typ_edit_model::{CompletionItem, CompletionKind};
use typst::syntax::{LinkedNode, SyntaxKind};

/// The set of names bound in the document by `#let` bindings, closure
/// parameters and imports.
///
/// Derived from the fused visitor's definitions by keeping their
/// [`CompletionKind::Local`] entries (labels are excluded: they are not
/// identifiers). The highlight walk
/// tags any `Ident` leaf whose text is in the set, and the incremental parser
/// compares the set between requests to decide whether cached highlight spans
/// can be spliced or must be recomputed wholesale.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DefinedNames(HashSet<String>);

impl DefinedNames {
    /// The document-local names among `items`, ignoring non-local completions.
    pub(crate) fn from_items(items: &[CompletionItem]) -> Self {
        let names = items
            .iter()
            .filter(|item| item.kind == CompletionKind::Local)
            .map(|item| item.name.clone())
            .collect();
        Self(names)
    }

    /// Whether `name` is bound somewhere in the document.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Records the completion definitions `node` contributes, at its pre-order
/// entry in the fused visitor's walk.
///
/// For `let f(a, b) = a` this binds `f` (first binding ident) plus the params
/// `a`, `b`; an import binds the last ident of its item path.
pub(super) fn collect_definition(node: &LinkedNode<'_>, out: &mut Vec<CompletionItem>) {
    match node.kind() {
        SyntaxKind::LetBinding => {
            if let Some(name) = first_ident(node) {
                out.push(CompletionItem::plain(name, CompletionKind::Local));
            }
        }
        SyntaxKind::Params => {
            for child in node.children() {
                if child.kind() == SyntaxKind::Ident {
                    out.push(CompletionItem::plain(
                        child.leaf_text().as_str(),
                        CompletionKind::Local,
                    ));
                }
            }
        }
        SyntaxKind::ModuleImport => collect_imports(node, out),
        SyntaxKind::Label => {
            let text = node.leaf_text();
            let name = text
                .strip_prefix('<')
                .and_then(|rest| rest.strip_suffix('>'))
                .unwrap_or(text);
            if !name.is_empty() {
                out.push(CompletionItem::plain(name, CompletionKind::Label));
            }
        }
        _ => {}
    }
}

/// Deduplicates the collected definitions by name (first in tree order wins),
/// sorts them, and caps the result at [`MAX_DEFINITIONS`].
pub(super) fn finish_definitions(mut out: Vec<CompletionItem>) -> Vec<CompletionItem> {
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out.truncate(MAX_DEFINITIONS);
    out
}

/// Collects the bound names imported by a `ModuleImport` node.
fn collect_imports(node: &LinkedNode<'_>, out: &mut Vec<CompletionItem>) {
    for child in node.children() {
        if child.kind() != SyntaxKind::ImportItems {
            continue;
        }
        for item in child.children() {
            if matches!(
                item.kind(),
                SyntaxKind::ImportItemPath | SyntaxKind::RenamedImportItem
            ) && let Some(name) = last_ident(&item)
            {
                out.push(CompletionItem::plain(name, CompletionKind::Local));
            }
        }
    }
}

/// Text of the first `Ident` in the pre-order subtree of `node`.
pub(super) fn first_ident(node: &LinkedNode<'_>) -> Option<String> {
    if node.kind() == SyntaxKind::Ident {
        return Some(node.leaf_text().to_string());
    }
    node.children().find_map(|child| first_ident(&child))
}

/// Text of the last `Ident` in the pre-order subtree of `node`.
pub(super) fn last_ident(node: &LinkedNode<'_>) -> Option<String> {
    let mut found = None;
    if node.kind() == SyntaxKind::Ident {
        found = Some(node.leaf_text().to_string());
    }
    for child in node.children() {
        if let Some(name) = last_ident(&child) {
            found = Some(name);
        }
    }
    found
}
