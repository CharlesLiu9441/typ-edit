//! Reusable egui widgets shared by the editor: the overflow scrollbar, the
//! vector icons, the tab bar, the segmented selector, the striped settings grid
//! and the design system.
//!
//! Every widget here is UI-only: the crate depends on `egui` (and the
//! `egui_taffy` layout engine, re-exported through [`tui`]) so the app can
//! reuse them without pulling in the rest of its stack.

pub mod button;
pub mod checkbox;
pub mod editable_combo;
pub mod icons;
pub mod layout_probe;
pub mod list;
pub mod menu;
pub mod scrollbar;
pub mod search_combo;
pub mod segmented;
pub mod separator;
pub mod slider;
pub mod striped_grid;
pub mod tab_bar;
pub mod text_input;
pub mod theme;
pub mod tokens;
pub mod tui;
pub mod wheel;

mod interact;
