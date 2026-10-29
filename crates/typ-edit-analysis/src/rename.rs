//! Prepare-rename and rename-edit construction: a small subset of tinymist's
//! `prepare_rename.rs` / `rename.rs`.
//!
//! v1 refuses field accesses, package files and standard-library targets. It
//! does not rename import paths, bib entries or aliases, and it never validates
//! the new name against the target's type.

use std::path::Path;

use typ_edit_model::{BytePos, ByteRange, FileEdits, PrepareRename, ReferenceLocation, TextEdit};
use typst::syntax::ast;
use typst::syntax::{LinkedNode, Source, SyntaxKind};
use typst_ide::Definition;

use crate::classify::{SyntaxClass, classified_expr, classify};

/// Why a rename is refused at a cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameRefusal {
    /// The cursor is on the field of a field access (`a.b`), not a definition.
    FieldAccess,
    /// Nothing resolves at the cursor.
    NotFound,
    /// The target is a standard-library item.
    Builtin,
    /// The file belongs to a package.
    Package,
    /// The cursor does not sit on valid syntax.
    SyntaxError,
    /// The expression is classifiable but not renameable (import path, ...).
    NotRenameable,
}

/// Describes what a rename at `byte` would change, or why it is refused.
///
/// `is_package` is the caller's package classification of `path`; `resolver`
/// maps an identifier start byte to its definition.
pub fn prepare_rename(
    source: &Source,
    path: &Path,
    byte: usize,
    is_package: bool,
    resolver: &mut dyn FnMut(&Path, usize) -> Option<Definition>,
) -> Result<PrepareRename, RenameRefusal> {
    if is_package {
        return Err(RenameRefusal::Package);
    }

    let expr = classified_expr(source, byte).ok_or(RenameRefusal::SyntaxError)?;
    if matches!(
        expr.kind(),
        SyntaxKind::FieldAccess | SyntaxKind::MathFieldAccess
    ) {
        return Err(RenameRefusal::FieldAccess);
    }

    match classify(source, byte) {
        Some(SyntaxClass::Label(node)) | Some(SyntaxClass::Ref(node)) => {
            let placeholder = label_or_ref_name(&node).ok_or(RenameRefusal::SyntaxError)?;
            let range = label_or_ref_range(&node).ok_or(RenameRefusal::NotRenameable)?;
            Ok(PrepareRename {
                range,
                placeholder,
                is_label: true,
            })
        }
        Some(SyntaxClass::VarAccess(node)) | Some(SyntaxClass::Callee(node)) => {
            let placeholder = node_name(&node).ok_or(RenameRefusal::NotRenameable)?;
            match resolver(path, node.offset()) {
                None => Err(RenameRefusal::NotFound),
                Some(Definition::Std(_)) => Err(RenameRefusal::Builtin),
                Some(_) => Ok(PrepareRename {
                    range: BytePos(node.range().start)..BytePos(node.range().end),
                    placeholder,
                    is_label: false,
                }),
            }
        }
        Some(SyntaxClass::ImportPath(_) | SyntaxClass::Other) | None => {
            Err(RenameRefusal::NotRenameable)
        }
    }
}

/// Whether `name` is a usable typst identifier and not a reserved keyword.
///
/// Uses typst's own identifier rule plus the keyword list. The keyword set is
/// the spellable keywords; `none`, `auto`, `true`/`false` are excluded so they
/// stay usable as plain values.
pub fn is_valid_ident(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "let", "set", "show", "import", "include", "if", "else", "for", "while", "break",
        "continue", "return", "as", "in", "not", "and", "or", "context",
    ];
    !name.is_empty() && !RESERVED.contains(&name) && typst::syntax::is_ident(name)
}

/// Groups references into one [`FileEdits`] per file, with edits in start order.
pub fn rename_edits(references: &[ReferenceLocation], new_name: &str) -> Vec<FileEdits> {
    let mut references = references.to_vec();
    references.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.range.start.cmp(&b.range.start))
    });

    let mut files: Vec<FileEdits> = Vec::new();
    for reference in references {
        let edit = TextEdit {
            range: reference.range,
            new_text: new_name.to_owned(),
        };
        match files.last_mut() {
            Some(file) if file.path == reference.path => file.edits.push(edit),
            _ => files.push(FileEdits {
                path: reference.path,
                edits: vec![edit],
            }),
        }
    }
    files
}

/// The name of a label or reference node.
fn label_or_ref_name(node: &LinkedNode) -> Option<String> {
    if let Some(label) = node.cast::<ast::Label>() {
        return Some(label.get().to_owned());
    }
    node.cast::<ast::Ref>()
        .map(|reference| reference.target().to_owned())
}

/// The rename range of a label (minus `<>`) or reference (minus `@`).
fn label_or_ref_range(node: &LinkedNode) -> Option<ByteRange> {
    if node.cast::<ast::Label>().is_some() {
        let range = node.range();
        return (range.end >= range.start + 2)
            .then(|| BytePos(range.start + 1)..BytePos(range.end - 1));
    }
    let marker = node
        .children()
        .find(|child| child.kind() == SyntaxKind::RefMarker)?;
    let range = marker.range();
    (range.end > range.start).then(|| BytePos(range.start + 1)..BytePos(range.end))
}

/// The identifier text of an `Ident`/`MathIdent` node.
fn node_name(node: &LinkedNode) -> Option<String> {
    if let Some(ident) = node.cast::<ast::Ident>() {
        return Some(ident.get().to_string());
    }
    node.cast::<ast::MathIdent>()
        .map(|ident| ident.get().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use typst::foundations::Value;
    use typst::syntax::Span;

    fn resolved() -> impl FnMut(&Path, usize) -> Option<Definition> {
        |_: &Path, _: usize| Some(Definition::Span(Span::detached()))
    }

    fn unresolved() -> impl FnMut(&Path, usize) -> Option<Definition> {
        |_: &Path, _: usize| None
    }

    fn prepare(
        text: &str,
        byte: usize,
        is_package: bool,
        resolver: &mut dyn FnMut(&Path, usize) -> Option<Definition>,
    ) -> Result<PrepareRename, RenameRefusal> {
        let source = Source::detached(text);
        prepare_rename(&source, Path::new("main.typ"), byte, is_package, resolver)
    }

    #[test]
    fn field_access_is_refused() {
        let result = prepare("#foo.bar", 8, false, &mut resolved());
        assert_eq!(result, Err(RenameRefusal::FieldAccess));
    }

    #[test]
    fn package_flag_refuses_before_anything_else() {
        let result = prepare("#let x = 1", 6, true, &mut resolved());
        assert_eq!(result, Err(RenameRefusal::Package));
    }

    #[test]
    fn builtin_and_unresolved_targets_are_refused() {
        let mut builtin = |_: &Path, _: usize| Some(Definition::Std(Value::None));
        assert_eq!(
            prepare("#let x = 1", 6, false, &mut builtin),
            Err(RenameRefusal::Builtin)
        );
        assert_eq!(
            prepare("#let x = 1", 6, false, &mut unresolved()),
            Err(RenameRefusal::NotFound)
        );
    }

    #[test]
    fn variable_prepare_uses_the_identifier_range() {
        let result = prepare("#let x = 1", 6, false, &mut resolved()).expect("renameable");
        assert_eq!(result.range, BytePos(5)..BytePos(6));
        assert_eq!(result.placeholder, "x");
        assert!(!result.is_label);
    }

    #[test]
    fn callee_prepare_uses_the_callee_identifier_range() {
        let result = prepare("#f(x)", 2, false, &mut resolved()).expect("renameable");
        assert_eq!(result.range, BytePos(1)..BytePos(2));
        assert_eq!(result.placeholder, "f");
        assert!(!result.is_label);
    }

    #[test]
    fn label_prepare_excludes_the_delimiters() {
        let result = prepare("<intro>", 6, false, &mut unresolved()).expect("renameable");
        assert_eq!(result.range, BytePos(1)..BytePos(6));
        assert_eq!(result.placeholder, "intro");
        assert!(result.is_label);
    }

    #[test]
    fn import_path_is_not_renameable() {
        let result = prepare("#import \"x\": y", 11, false, &mut resolved());
        assert_eq!(result, Err(RenameRefusal::NotRenameable));
    }

    #[test]
    fn valid_ident_rejects_keywords_and_malformed_names() {
        assert!(is_valid_ident("foo"));
        assert!(is_valid_ident("_x1"));
        assert!(!is_valid_ident(""));
        assert!(!is_valid_ident("let"));
        assert!(!is_valid_ident("context"));
        assert!(!is_valid_ident("1abc"));
    }

    #[test]
    fn rename_edits_group_by_file_and_sort_edits() {
        let references = vec![
            ReferenceLocation {
                path: PathBuf::from("b.typ"),
                range: BytePos(3)..BytePos(4),
            },
            ReferenceLocation {
                path: PathBuf::from("a.typ"),
                range: BytePos(9)..BytePos(10),
            },
            ReferenceLocation {
                path: PathBuf::from("a.typ"),
                range: BytePos(2)..BytePos(3),
            },
        ];
        let files = rename_edits(&references, "renamed");
        assert_eq!(files.len(), 2, "{files:?}");
        assert_eq!(files[0].path, PathBuf::from("a.typ"));
        assert_eq!(
            files[0]
                .edits
                .iter()
                .map(|e| e.range.clone())
                .collect::<Vec<_>>(),
            vec![BytePos(2)..BytePos(3), BytePos(9)..BytePos(10)]
        );
        assert_eq!(files[0].edits[0].new_text, "renamed");
        assert_eq!(files[1].path, PathBuf::from("b.typ"));
        assert_eq!(files[1].edits.len(), 1);
    }
}
