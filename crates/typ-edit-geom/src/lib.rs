//! Standard-library-only geometry shared by the engine's page index and the GPU
//! canvas's frame index: a conservative axis-aligned [`Extents`] box and a
//! centroid-split [`Bvh`] over a slice of them.
//!
//! The crate deliberately has no dependencies — not on `typst`, not on
//! `egui`/`wgpu` — so both `typ-edit-engine` and `typst-gpu-canvas` can depend on
//! it without either depending on the other. Item extents are caller-supplied:
//! the engine derives them from plain typst frames, the GPU canvas from frames
//! plus group-child bounds, and neither derivation belongs here.

mod bvh;
mod extents;

pub use bvh::{Bvh, BvhStats};
pub use extents::Extents;
