//! Smart editing: the pure text transforms and predicates in [`logic`]
//! (auto-indent/dedent, bracket matching, multi-line indent/unindent) that the
//! virtualized text path applies after an edit.

mod logic;

pub(crate) use logic::*;
