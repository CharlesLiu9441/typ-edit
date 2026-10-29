//! Syntax-highlight token categories.

/// Token categories produced by the syntax-tree highlighter.
///
/// Mirrors `typst_syntax::Tag` (kept separate so UI code does not depend on typst-syntax).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Comment,
    Punctuation,
    Escape,
    Strong,
    Emph,
    Link,
    Raw,
    Label,
    Ref,
    Heading,
    ListMarker,
    ListTerm,
    MathDelimiter,
    MathOperator,
    MathGroupingParens,
    /// A math-mode identifier naming a Typst symbol (`integral`, `delta`, `eq`).
    MathSymbol,
    Keyword,
    Operator,
    Number,
    String,
    Function,
    Interpolated,
    Error,
    ParamName,
}
