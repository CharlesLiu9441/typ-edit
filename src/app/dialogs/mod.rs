//! Modal dialogs. Each dialog's state lives on `TypEditApp`; rendering and
//! applying its result live in the matching submodule:
//!
//! - [`dialog_tree`] — new file/folder prompt and delete confirmation.
//! - [`close_prompt`] — unsaved-changes question for a tab or the app.
//! - [`recovery_prompt`] — crash-recovery snapshot restore question.
//! - [`settings`] — bridge to the shell settings window.
//! - [`png_export`] — PNG resolution settings before starting an export.
//! - [`new_project`] — blank-project scaffolding from the landing page.

mod close_prompt;
mod dialog_tree;
mod new_project;
mod png_export;
mod recovery_prompt;
mod settings;

pub(in crate::app) use close_prompt::ClosePromptOutcome;
