//! Raster worker: `typst-render` rasterization of whole pages and clipped regions.
//!
//! One thread keeps the latest [`PreviewDoc`] and services [`RasterRequest`]s. It
//! coalesces requests to the newest one (the UI keeps a single render in flight), so
//! a fast pan/zoom never queues up work. Stale generations are dropped, and every
//! failure path is logged rather than panicked.
//!
//! Sharp region renders on large pages are culled with a per-page BVH
//! ([`crate::page_bvh`]): only the items whose conservative extents intersect the
//! region are cloned into a single region-clipped group, which the experiment proved
//! pixel-equal to the whole-frame baseline.

use std::thread::JoinHandle;
use std::time::Instant;

use crate::latest::LatestReceiver;
use crate::results::PreviewDoc;
use crate::supervisor::{EngineHealth, Pipeline, supervise};
use crate::wake::Waker;
use crossbeam_channel::{Sender, select};
use typ_edit_model::{RasterRequest, RasterResult};

mod cache;
mod region;

use cache::PageIndexCache;
use region::options;

/// Spawns the raster worker thread. Reads the document and request mailboxes and
/// is supervised; the current document survives a restart, the page-index cache
/// is rebuilt.
pub fn spawn(
    doc: LatestReceiver<PreviewDoc>,
    req: LatestReceiver<RasterRequest>,
    res_tx: Sender<RasterResult>,
    waker: Waker,
    health: Sender<EngineHealth>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-raster".to_owned())
        .spawn(move || {
            // Durable across restarts: dropping the compiled document on every
            // restart would blank the preview until the next compile.
            let mut current: Option<PreviewDoc> = None;
            supervise(Pipeline::Raster, &health, || {
                // Derived page indexes are rebuilt fresh per session.
                let mut cache = PageIndexCache::default();
                loop {
                    select! {
                        recv(doc.tickle_rx()) -> message => {
                            if message.is_err() {
                                break;
                            }
                            if let Some(document) = doc.take() {
                                store(&mut current, &mut cache, document);
                            }
                        },
                        recv(req.tickle_rx()) -> message => {
                            if message.is_err() {
                                break;
                            }
                            let Some(request) = req.take() else {
                                continue;
                            };
                            // Fold in a document delivered with (or just before) the
                            // request; the two mailboxes need no ordering guarantee.
                            if let Some(document) = doc.take() {
                                store(&mut current, &mut cache, document);
                            }
                            let Some(result) = render_request(&current, &request, &mut cache) else {
                                continue;
                            };
                            if res_tx.send(result).is_err() {
                                break;
                            }
                            waker.wake();
                        },
                    }
                }
            });
        })
        .expect("spawn raster worker")
}

/// Stores a freshly compiled document unless the slot already holds a newer one.
///
/// A newly stored generation invalidates every cached page index of the other generations.
fn store(current: &mut Option<PreviewDoc>, cache: &mut PageIndexCache, doc: PreviewDoc) {
    let is_newer = current
        .as_ref()
        .is_none_or(|old| doc.generation >= old.generation);
    if is_newer {
        cache.keep_generation(doc.generation);
        *current = Some(doc);
    }
}

/// Rasterizes one request, or `None` when the generation/page is stale or missing.
fn render_request(
    current: &Option<PreviewDoc>,
    request: &RasterRequest,
    cache: &mut PageIndexCache,
) -> Option<RasterResult> {
    let doc = current.as_ref()?;
    if doc.generation != request.generation {
        return None;
    }
    let page = doc.document.pages().get(request.page)?;
    let size = page.frame.size();
    let page_size = (size.x.to_pt() as f32, size.y.to_pt() as f32);

    let region = request
        .region
        .unwrap_or((0.0, 0.0, page_size.0, page_size.1));
    let options = options(request.pixel_per_pt);
    let pixmap = match request.region {
        Some(region) => {
            let start = Instant::now();
            let (synthetic, used, total) =
                cache.region_page(page, (request.generation, request.page), region);
            let pixmap = typst_render::render(&synthetic, &options);
            log::debug!(
                "raster region generation={} page={} items={}/{} region=({:.1},{:.1},{:.1},{:.1}) elapsed_ms={:.2}",
                request.generation,
                request.page,
                used,
                total,
                region.0,
                region.1,
                region.2,
                region.3,
                start.elapsed().as_secs_f64() * 1000.0,
            );
            pixmap
        }
        None => typst_render::render(page, &options),
    };

    let size_px = (pixmap.width(), pixmap.height());
    Some(RasterResult {
        generation: request.generation,
        page: request.page,
        region,
        pixel_per_pt: request.pixel_per_pt,
        size_px,
        rgba: pixmap.take(),
    })
}

/// A compiled document with generation 1, shared by the worker and region tests.
#[cfg(test)]
fn compiled_document() -> (u64, std::sync::Arc<typst_layout::PagedDocument>) {
    use crate::fonts::FontCatalog;
    use crate::world::WorldState;
    use std::sync::Arc;

    let fonts = FontCatalog::new();
    let root = std::env::temp_dir();
    let main = root.join("typ-edit-raster-test.typ");
    let world = WorldState::new(
        root,
        main.clone(),
        vec![(main, "= Hello\n\nRegion rendering test text.".to_owned())],
        &fonts,
        Arc::new(crate::packages::system_packages()),
    );
    let document = typst::compile::<typst_layout::PagedDocument>(&world)
        .output
        .expect("compile raster test document");
    (1, Arc::new(document))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_page_raster_has_the_requested_page_size() {
        let (generation, document) = compiled_document();
        let current = Some(PreviewDoc {
            generation,
            document,
        });
        let result = render_request(
            &current,
            &RasterRequest {
                generation,
                page: 0,
                region: None,
                pixel_per_pt: 1.0,
            },
            &mut PageIndexCache::default(),
        )
        .expect("whole-page raster");

        assert!(result.size_px.0 > 0 && result.size_px.1 > 0);
        let (x, y, width, height) = result.region;
        assert_eq!((x, y), (0.0, 0.0));
        assert!((width - result.size_px.0 as f32).abs() <= 1.0);
        assert!((height - result.size_px.1 as f32).abs() <= 1.0);
        assert_eq!(
            result.rgba.len(),
            (result.size_px.0 * result.size_px.1 * 4) as usize
        );
    }

    #[test]
    fn clipped_region_raster_has_the_region_size_and_draws_text() {
        let (generation, document) = compiled_document();
        let current = Some(PreviewDoc {
            generation,
            document,
        });
        let region = (50.0, 50.0, 300.0, 150.0);
        let result = render_request(
            &current,
            &RasterRequest {
                generation,
                page: 0,
                region: Some(region),
                pixel_per_pt: 2.0,
            },
            &mut PageIndexCache::default(),
        )
        .expect("region raster");

        assert_eq!(result.size_px, (600, 300));
        assert_eq!(result.region, region);
        assert!(
            result
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] < 128 && pixel[3] > 0),
            "region raster contained no dark text pixels"
        );
    }

    #[test]
    fn stale_generation_yields_no_result() {
        let (generation, document) = compiled_document();
        let current = Some(PreviewDoc {
            generation,
            document,
        });
        let result = render_request(
            &current,
            &RasterRequest {
                generation: generation + 1,
                page: 0,
                region: None,
                pixel_per_pt: 1.0,
            },
            &mut PageIndexCache::default(),
        );
        assert!(result.is_none());
    }
}
