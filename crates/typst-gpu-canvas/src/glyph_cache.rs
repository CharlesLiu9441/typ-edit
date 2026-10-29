//! Cross-frame caches for glyph outlines and baked glyph meshes.
//!
//! Follows the two-phase pattern of the CPU renderer this module was extracted
//! from: a lookup and an insert each take the egui context data lock briefly,
//! and `build` runs between them with the lock released. Glyph extraction and
//! baking do not re-enter the context, so the split is only about keeping the
//! lock windows minimal.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Context, Id};
use typst_library::text::FontInstance;

use crate::tess::{Path2, TessMesh};

/// The one context-data key under which [`GlyphCaches`] is stored.
fn caches_id() -> Id {
    Id::new("typst-gpu-canvas::glyph-caches")
}

/// Every per-context glyph cache, in one place.
#[derive(Clone, Default)]
struct GlyphCaches {
    /// Glyph outlines in font design units, keyed by `(font, glyph id)`.
    outlines: HashMap<(FontInstance, u16), Arc<Path2>>,
    /// Baked glyph meshes in absolute mip-level pixels, keyed by
    /// `(font, glyph id, mip level)`.
    cores: HashMap<(FontInstance, u16, u32), Arc<TessMesh>>,
}

/// Returns the shared outline for `(font, id)`, extracting and caching it on
/// first use.
pub(crate) fn cached_outline(ctx: &Context, font: &FontInstance, id: u16) -> Arc<Path2> {
    cached_value(
        ctx,
        |caches| &mut caches.outlines,
        (font.clone(), id),
        || Arc::new(crate::glyphs::glyph_outline(font, id).unwrap_or_default()),
    )
}

/// Returns the shared baked mesh for `(font, id)` at `level`, baking it on
/// first use.
pub(crate) fn cached_core(
    ctx: &Context,
    font: &FontInstance,
    id: u16,
    level: usize,
) -> Arc<TessMesh> {
    cached_value(
        ctx,
        |caches| &mut caches.cores,
        (font.clone(), id, level as u32),
        || {
            let outline = crate::glyphs::glyph_outline(font, id).unwrap_or_default();
            Arc::new(crate::glyphs::bake_glyph(
                &outline,
                font.units_per_em(),
                level,
            ))
        },
    )
}

/// Runs `f` with mutable access to the context's glyph caches.
fn update<R>(ctx: &Context, f: impl FnOnce(&mut GlyphCaches) -> R) -> R {
    ctx.data_mut(|data| f(data.get_temp_mut_or_default::<GlyphCaches>(caches_id())))
}

/// Returns the cached value for `key` in the map selected by `slot`, calling
/// `build` outside the context lock when the key is absent.
fn cached_value<K, V>(
    ctx: &Context,
    slot: impl Fn(&mut GlyphCaches) -> &mut HashMap<K, V>,
    key: K,
    build: impl FnOnce() -> V,
) -> V
where
    K: std::hash::Hash + Eq + Clone,
    V: Clone,
{
    if let Some(value) = update(ctx, |caches| slot(caches).get(&key).cloned()) {
        return value;
    }
    let value = build();
    update(ctx, |caches| {
        slot(caches).insert(key, value.clone());
    });
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    use typst_library::foundations::Bytes;
    use typst_library::layout::Abs;
    use typst_library::text::{Font, FontVariant, FontVariations};

    /// A 20pt instance of the first bundled `typst-assets` font.
    fn instance() -> FontInstance {
        let data = typst_assets::fonts().next().expect("bundled font");
        let font = Font::new(Bytes::new(data), 0).expect("font parses");
        font.instantiate(
            FontVariant::default(),
            Abs::pt(20.0),
            &FontVariations::default(),
        )
    }

    #[test]
    fn cached_outline_is_shared_across_frames() {
        let font = instance();
        let id = font.ttf().glyph_index('A').expect("glyph").0;

        let ctx = egui::Context::default();
        let first = cached_outline(&ctx, &font, id);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        let second = cached_outline(&ctx, &font, id);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn cached_core_is_shared_across_frames() {
        let font = instance();
        let id = font.ttf().glyph_index('A').expect("glyph").0;

        let ctx = egui::Context::default();
        let first = cached_core(&ctx, &font, id, 3);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        let second = cached_core(&ctx, &font, id, 3);
        assert!(Arc::ptr_eq(&first, &second));
    }
}
