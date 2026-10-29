//! Pure, UI-free source analysis over a parsed typst [`typst::syntax::Source`].
//!
//! This crate owns document-level analyses that are independent of the editor
//! and the compiler: the lexical symbol tree (document outline) lives in
//! [`lexical`], and the find-references / rename logic lives in [`classify`],
//! [`index`], [`refs`] and [`rename`]. Keeping them in a dedicated crate lets
//! the parse worker reuse them without pulling in egui or the IDE layer.

pub mod classify;
pub mod index;
pub mod lexical;
pub mod lint;
pub mod lint_font;
pub mod refs;
pub mod rename;
