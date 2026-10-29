//! Completion candidates and the call context resolved around the caret.

use crate::pos::{ByteRange, CharCount};

/// One auto-completion candidate.
#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub name: String,
    pub kind: CompletionKind,
    /// Text written over the typed prefix when the candidate is accepted.
    pub insert: String,
    /// Chars between the end of `insert` and the caret after accepting.
    pub cursor_back: CharCount,
    /// First line of the value's documentation, for library candidates.
    pub docs: Option<String>,
}

impl CompletionItem {
    /// A plain candidate that inserts its own name.
    pub fn plain(name: impl Into<String>, kind: CompletionKind) -> Self {
        let name = name.into();
        Self {
            insert: name.clone(),
            name,
            kind,
            cursor_back: CharCount(0),
            docs: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    /// A constant or plain value (a color, a length, a module, ...).
    Value,
    /// A global type (`str`, `length`, ...).
    Type,
    /// A global function that is not a native element.
    Function,
    /// A native element constructor (`heading`, `table`, ...).
    Element,
    /// A function offered inside math (`sqrt`, `frac`, ...).
    MathFunction,
    /// A symbol offered inside math (`delta`, `eq.ast`, ...).
    MathSymbol,
    /// Legacy math-symbol kind, kept for callers that still produce it.
    Symbol,
    Local,
    Label,
    Param,
}

impl CompletionKind {
    /// Whether the kind is only offered in math mode.
    pub fn is_math(self) -> bool {
        matches!(self, Self::MathFunction | Self::MathSymbol | Self::Symbol)
    }

    /// Whether the kind is a function value that carries methods (`.where`).
    pub fn is_callable(self) -> bool {
        matches!(self, Self::Element | Self::Function | Self::MathFunction)
    }
}

/// Named parameters of the call around the parse request's cursor.
#[derive(Clone, Debug)]
pub struct CallParams {
    /// Byte range of the call's `Args` node in the parsed text.
    pub range: ByteRange,
    /// The callee's named parameters, in declaration order.
    pub items: Vec<CompletionItem>,
    /// Value-slot context when the caret sits in the value position of a named
    /// argument; `None` in every other slot.
    pub value_slot: Option<ValueSlot>,
}

/// Value-slot context: the caret sits in the value position of a named
/// argument (`fill: |`).
#[derive(Clone, Debug)]
pub struct ValueSlot {
    /// The parameter being filled (e.g. `fill`).
    pub param: String,
    /// Candidates that satisfy the parameter's required type.
    pub items: Vec<CompletionItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_inserts_its_own_name_with_no_cursor_offset() {
        let item = CompletionItem::plain("foo", CompletionKind::Local);
        assert_eq!(item.name, "foo");
        assert_eq!(item.insert, "foo", "the plain candidate inserts its name");
        assert_eq!(item.kind, CompletionKind::Local);
        assert_eq!(
            item.cursor_back,
            CharCount(0),
            "the caret lands after the inserted name"
        );
        assert_eq!(item.docs, None);
    }

    #[test]
    fn plain_accepts_owned_and_borrowed_names() {
        let borrowed = CompletionItem::plain("bar", CompletionKind::Keyword);
        let owned = CompletionItem::plain(String::from("bar"), CompletionKind::Keyword);
        assert_eq!(borrowed.name, owned.name);
        assert_eq!(borrowed.insert, owned.insert);
    }

    #[test]
    fn plain_keeps_the_requested_kind() {
        for kind in [
            CompletionKind::Keyword,
            CompletionKind::Value,
            CompletionKind::Type,
            CompletionKind::Function,
            CompletionKind::Element,
            CompletionKind::MathFunction,
            CompletionKind::MathSymbol,
            CompletionKind::Symbol,
            CompletionKind::Local,
            CompletionKind::Label,
            CompletionKind::Param,
        ] {
            assert_eq!(CompletionItem::plain("x", kind).kind, kind);
        }
    }

    #[test]
    fn math_and_callable_kind_predicates() {
        assert!(CompletionKind::MathFunction.is_math());
        assert!(CompletionKind::MathSymbol.is_math());
        assert!(CompletionKind::Symbol.is_math());
        assert!(!CompletionKind::Element.is_math());
        assert!(!CompletionKind::Value.is_math());

        assert!(CompletionKind::Element.is_callable());
        assert!(CompletionKind::Function.is_callable());
        assert!(CompletionKind::MathFunction.is_callable());
        assert!(!CompletionKind::MathSymbol.is_callable());
        assert!(!CompletionKind::Value.is_callable());
    }
}
