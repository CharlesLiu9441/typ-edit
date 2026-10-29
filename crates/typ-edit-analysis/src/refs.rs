//! Find references: scan project files and keep the occurrences that resolve to
//! the same target.
//!
//! tinymist resolves scope identity from a precomputed expression index; this
//! crate instead compares the target produced by [`typst_ide::definition`] for
//! every same-named occurrence. Shadowing falls out of the comparison: a
//! shadowed occurrence resolves to a different `Definition` and is dropped.
//! Labels have no scope and are matched by name across `Label`/`Ref` nodes.
//!
//! v1 does not walk the import graph  -  the caller passes every project `.typ`
//! file  -  and skips non-`.typ` inputs defensively.

use std::path::{Path, PathBuf};

use typ_edit_model::{BytePos, ByteRange, ReferenceLocation};
use typst::syntax::ast;
use typst::syntax::{LinkedNode, Source, SyntaxKind};
use typst_ide::Definition;

use crate::index::name_index;

/// Resolves the definition of the identifier beginning at the given byte in a
/// file. Callers (the engine worker) implement this on top of
/// [`typst_ide::definition`].
pub type Resolver<'a> = &'a mut dyn FnMut(&Path, usize) -> Option<Definition>;

/// Every reference to `target_name` across `files`, sorted by path then start.
///
/// `target_is_label` selects name-based label matching; otherwise occurrences
/// are kept only when `resolver` maps them to `target`. The definition site is
/// included (it resolves to itself).
pub fn find_references(
    resolver: &mut dyn FnMut(&Path, usize) -> Option<Definition>,
    files: &[(PathBuf, Source)],
    target: &Definition,
    target_is_label: bool,
    target_name: &str,
) -> Vec<ReferenceLocation> {
    let mut references = Vec::new();
    for file in files {
        let (path, source) = file;
        if path.extension().and_then(|ext| ext.to_str()) != Some("typ") {
            continue;
        }
        if target_is_label {
            collect_labels(file, target_name, &mut references);
            continue;
        }
        for (name, offset) in name_index(source).idents {
            if name != target_name {
                continue;
            }
            let Some(resolved) = resolver(path, offset) else {
                continue;
            };
            if same_definition(&resolved, target) {
                references.push(ReferenceLocation {
                    path: path.clone(),
                    range: BytePos(offset)..BytePos(offset + name.len()),
                });
            }
        }
    }
    references.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.range.start.cmp(&b.range.start))
    });
    references
}

/// Structural equality for [`Definition`], which does not derive `PartialEq`.
pub fn same_definition(a: &Definition, b: &Definition) -> bool {
    match (a, b) {
        (Definition::Span(a), Definition::Span(b)) => a == b,
        (Definition::File(a), Definition::File(b)) => a == b,
        (Definition::Std(a), Definition::Std(b)) => a == b,
        _ => false,
    }
}

/// Pushes every `Label`/`Ref` named `target_name`, name-span only.
fn collect_labels(
    file: &(PathBuf, Source),
    target_name: &str,
    references: &mut Vec<ReferenceLocation>,
) {
    let (path, source) = file;
    let mut stack = vec![LinkedNode::new(source.root())];
    while let Some(node) = stack.pop() {
        match node.kind() {
            SyntaxKind::Label => {
                if node
                    .cast::<ast::Label>()
                    .is_some_and(|label| label.get() == target_name)
                    && let Some(range) = label_name_range(&node)
                {
                    references.push(ReferenceLocation {
                        path: path.to_path_buf(),
                        range,
                    });
                }
            }
            SyntaxKind::Ref => {
                if node
                    .cast::<ast::Ref>()
                    .is_some_and(|reference| reference.target() == target_name)
                    && let Some(range) = ref_name_range(&node)
                {
                    references.push(ReferenceLocation {
                        path: path.to_path_buf(),
                        range,
                    });
                }
            }
            _ => {}
        }
        stack.extend(node.children());
    }
}

/// Byte range of a label's name, excluding the `<` and `>` delimiters.
fn label_name_range(node: &LinkedNode) -> Option<ByteRange> {
    let range = node.range();
    (range.end >= range.start + 2).then(|| BytePos(range.start + 1)..BytePos(range.end - 1))
}

/// Byte range of a reference's target, excluding the leading `@` and any
/// trailing supplement.
fn ref_name_range(node: &LinkedNode) -> Option<ByteRange> {
    let marker = node
        .children()
        .find(|child| child.kind() == SyntaxKind::RefMarker)?;
    let range = marker.range();
    (range.end > range.start).then(|| BytePos(range.start + 1)..BytePos(range.end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst::syntax::Side;

    /// Span of the leaf starting at `offset`, used to build distinct targets.
    fn leaf_span(source: &Source, offset: usize) -> typst::syntax::Span {
        LinkedNode::new(source.root())
            .leaf_at(offset, Side::After)
            .map(|node| node.span())
            .unwrap_or_else(|| source.root().span())
    }

    #[test]
    fn ident_mode_keeps_matching_and_drops_shadowed_occurrences() {
        let text = "#let x = 1\n#x\n#x";
        let source = Source::detached(text);
        let target = Definition::Span(leaf_span(&source, 5));
        let shadow = Definition::Span(leaf_span(&source, 15));
        let files = vec![(PathBuf::from("a.typ"), source)];

        let mut resolver = |_: &Path, byte: usize| -> Option<Definition> {
            match byte {
                5 | 12 => Some(target.clone()),
                15 => Some(shadow.clone()),
                _ => None,
            }
        };
        let references = find_references(&mut resolver, &files, &target, false, "x");
        assert_eq!(references.len(), 2, "{references:?}");
        assert_eq!(references[0].path, PathBuf::from("a.typ"));
        assert_eq!(references[0].range, BytePos(5)..BytePos(6));
        assert_eq!(references[1].range, BytePos(12)..BytePos(13));
    }

    #[test]
    fn ident_mode_scans_every_file_and_includes_the_definition() {
        let a = Source::detached("#let x = 1");
        let b = Source::detached("#import \"a.typ\": x\n#x");
        let target = Definition::Span(leaf_span(&a, 5));
        let files = vec![(PathBuf::from("a.typ"), a), (PathBuf::from("b.typ"), b)];

        let mut resolver = |path: &Path, byte: usize| -> Option<Definition> {
            (path == Path::new("a.typ") && byte == 5).then(|| target.clone())
        };
        let references = find_references(&mut resolver, &files, &target, false, "x");
        assert_eq!(references.len(), 1, "{references:?}");
        assert_eq!(references[0].path, PathBuf::from("a.typ"));
        assert_eq!(references[0].range, BytePos(5)..BytePos(6));
    }

    #[test]
    fn label_mode_matches_labels_and_refs_by_name_without_delimiters() {
        let source = Source::detached("<a> and @a");
        let files = vec![(PathBuf::from("a.typ"), source)];
        let mut resolver = |_: &Path, _: usize| -> Option<Definition> { None };

        let references = find_references(
            &mut resolver,
            &files,
            &Definition::Span(typst::syntax::Span::detached()),
            true,
            "a",
        );
        assert_eq!(references.len(), 2, "{references:?}");
        assert_eq!(references[0].range, BytePos(1)..BytePos(2));
        assert_eq!(references[1].range, BytePos(9)..BytePos(10));
    }

    #[test]
    fn non_typ_files_are_skipped() {
        let source = Source::detached("#let x = 1");
        let files = vec![(PathBuf::from("a.yml"), source)];
        let target = Definition::Span(typst::syntax::Span::detached());
        let mut resolver = |_: &Path, _: usize| -> Option<Definition> { Some(target.clone()) };

        assert!(find_references(&mut resolver, &files, &target, false, "x").is_empty());
    }
}
