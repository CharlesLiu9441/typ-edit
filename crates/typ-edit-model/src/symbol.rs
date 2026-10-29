//! Lexical symbols: the document outline tree.

use crate::pos::ByteRange;

/// Which construct a symbol in the document outline represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    /// A section heading; the payload is its depth (the number of `=` markers).
    Heading(u16),
    /// A top-level variable binding (`#let` or a `for`-loop/destructuring target).
    Variable,
    /// A function definition: a named closure at the top level.
    Function,
    /// A parameter of a key function, e.g. `a` in `#let f(a) = a`.
    Parameter,
    /// A label: `<intro>`.
    Label,
}

/// A node in the document's lexical symbol tree.
///
/// `range` is a UTF-8 byte range into the source text (matching highlight and
/// fold ranges). Children are emitted in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolNode {
    pub name: String,
    pub kind: SymbolKind,
    pub range: ByteRange,
    pub children: Vec<SymbolNode>,
}
