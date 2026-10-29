//! Per-file name index: every identifier and label occurrence with its byte
//! offset, in source order.

use typst::syntax::ast;
use typst::syntax::{LinkedNode, Source, SyntaxKind};

/// All name occurrences in one parsed file, in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameIndex {
    /// `(name, byte offset of the identifier)` for every `Ident`/`MathIdent`.
    pub idents: Vec<(String, usize)>,
    /// `(name, byte offset of the name inside `<...>`)` for every label.
    pub labels: Vec<(String, usize)>,
}

/// Collects the name index of `source`.
pub fn name_index(source: &Source) -> NameIndex {
    let mut index = NameIndex::default();
    walk(&LinkedNode::new(source.root()), &mut index);
    index
}

/// Visits `node`, recording identifier and label leaves.
fn walk(node: &LinkedNode, index: &mut NameIndex) {
    match node.kind() {
        SyntaxKind::Ident | SyntaxKind::MathIdent => {
            index
                .idents
                .push((node.leaf_text().to_string(), node.offset()));
            return;
        }
        SyntaxKind::Label => {
            let name = node
                .cast::<ast::Label>()
                .map(|label| label.get().to_string())
                .unwrap_or_default();
            index.labels.push((name, node.offset() + 1));
            return;
        }
        _ => {}
    }
    for child in node.children() {
        walk(&child, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idents_and_labels_are_indexed_in_source_order() {
        let source = Source::detached("#let foo = foo\n<bar>");
        let index = name_index(&source);
        assert_eq!(
            index.idents,
            vec![("foo".to_owned(), 5), ("foo".to_owned(), 11)]
        );
        assert_eq!(index.labels, vec![("bar".to_owned(), 16)]);
    }

    #[test]
    fn empty_source_has_an_empty_index() {
        assert_eq!(name_index(&Source::detached("")), NameIndex::default());
    }

    #[test]
    fn import_binding_and_label_are_both_indexed() {
        let source = Source::detached("#import \"x\": y\n<a>");
        let index = name_index(&source);
        assert_eq!(index.idents, vec![("y".to_owned(), 13)]);
        assert_eq!(index.labels, vec![("a".to_owned(), 16)]);
    }
}
