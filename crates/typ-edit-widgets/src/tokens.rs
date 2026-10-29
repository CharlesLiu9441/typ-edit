//! Shared design tokens: the raw alpha values behind the app's structural
//! colors.
//!
//! The root binary's `theme` module derives its `band` and `hairline` colors
//! from these values, but that module lives in the root crate which this one
//! (and the editor) cannot depend on. The single source of truth for the raw
//! alpha constants therefore lives here; the root and editor crates adopt them
//! in a later phase. Pair the light/dark constants with
//! [`egui::Color32::from_white_alpha`] / [`egui::Color32::from_black_alpha`] for
//! the active theme.

/// Alpha of the alternating-row / current-line band in the dark theme (a white
/// overlay). Mirrors root `src\theme.rs` `band()`.
pub const BAND_WHITE_ALPHA: u8 = 8;

/// Alpha of the alternating-row / current-line band in the light theme (a black
/// overlay). Mirrors root `src\theme.rs` `band()`.
pub const BAND_BLACK_ALPHA: u8 = 10;

/// Alpha of structural hairlines (gutter separator, tab underline, error-bar
/// top) in both themes. Mirrors root `src\theme.rs` `hairline()`.
pub const HAIRLINE_ALPHA: u8 = 22;
