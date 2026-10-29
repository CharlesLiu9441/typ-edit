//! UI-agnostic typst engine: parse/compile/raster workers, the typst
//! [`World`](typst::World), the system font catalog and the IDE helpers (hover,
//! completions).
//!
//! The crate has no egui dependency; background workers wake the UI through the
//! [`Wake`] handle the app supplies.

pub mod accept;
pub mod completions;
pub mod fonts;
pub mod hover;
pub mod parse;
pub mod registry;
pub mod results;
pub mod template;
pub mod wake;
pub mod world;

mod cast;
mod compile;
mod cursor;
mod engine;
mod ide;
mod latest;
mod packages;
mod page_bounds;
mod page_bvh;
mod raster;
mod search;
mod spell;
mod stdlib;
mod supervisor;
mod sync;
pub(crate) mod values;

pub use cursor::CursorAnchor;
pub use cursor::map_frame_rect;
pub use engine::{Engine, EngineEvents};
pub use results::{CompileResult, PreviewDoc};
pub use supervisor::{EngineHealth, Pipeline};
pub use wake::{Wake, Waker};
