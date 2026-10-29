//! Worker-local LRU cache of per-page BVH indices.
//!
//! Indices are built lazily on the first sharp region request for a page, and
//! entries of other generations are dropped as soon as a newer document arrives.
//! Within the current generation the cache is LRU-bounded by an estimated byte
//! budget.

use std::collections::HashMap;

use crate::page_bounds::{self, Extents};
use crate::page_bvh::PageBvh;
use typst_layout::Page;

use super::region::{Region, clamped_region, selected_region_page, whole_frame_region_page};

/// Pages with at least this many top-level items use the BVH-culled region render.
const SMALL_PAGE_ITEMS: usize = 512;

/// Cached BVH indices are evicted once their estimated total size exceeds this budget.
const INDEX_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Estimated bytes per BVH node: an `Extents` (4 × `f64`) plus an 8-byte enum
/// discriminant, rounded to the 8-byte alignment = 48. The node type is private to
/// `typ-edit-geom`, so the per-node size is mirrored here rather than queried with `size_of`.
const BVH_NODE_BYTES: usize = 48;

/// A cached page index together with its accounting metadata.
struct CachedIndex {
    bvh: PageBvh,
    /// Estimated heap bytes held by `bvh` (see [`bvh_byte_size`]).
    bytes: usize,
    /// Monotonic use stamp; the least recently used entry holds the smallest stamp.
    last_used: u64,
}

/// Worker-local page index cache keyed by `(generation, page)`.
///
/// Indices are built lazily on the first sharp region request for a page, and entries of other
/// generations are dropped as soon as a newer document arrives. Within the current generation
/// the cache is LRU-bounded: once its estimated size exceeds [`INDEX_BUDGET_BYTES`], the least
/// recently used entries are evicted until it fits.
pub(super) struct PageIndexCache {
    indices: HashMap<(u64, usize), CachedIndex>,
    total_bytes: usize,
    clock: u64,
    budget: usize,
}

impl Default for PageIndexCache {
    fn default() -> Self {
        Self {
            indices: HashMap::new(),
            total_bytes: 0,
            clock: 0,
            budget: INDEX_BUDGET_BYTES,
        }
    }
}

impl PageIndexCache {
    /// Drops every cached page index that does not belong to `generation`.
    pub(super) fn keep_generation(&mut self, generation: u64) {
        let mut freed = 0usize;
        self.indices.retain(|&(cached, _), entry| {
            if cached == generation {
                true
            } else {
                freed += entry.bytes;
                false
            }
        });
        self.total_bytes -= freed;
    }

    /// Returns the page's BVH, building it from conservative extents on first use.
    fn bvh_for(&mut self, key: (u64, usize), page: &Page) -> &PageBvh {
        if !self.indices.contains_key(&key) {
            let width = page.frame.size().x.to_pt();
            let extents = page_bounds::compute_extents(page.frame.items(), width);
            self.insert_index(key, PageBvh::build(&extents));
        }
        self.clock += 1;
        let entry = self.indices.get_mut(&key).expect("page index cached above");
        entry.last_used = self.clock;
        &entry.bvh
    }

    /// Stores `bvh` under `key`, then evicts LRU entries if the budget is exceeded.
    fn insert_index(&mut self, key: (u64, usize), bvh: PageBvh) {
        let bytes = bvh_byte_size(&bvh);
        if let Some(old) = self.indices.remove(&key) {
            self.total_bytes -= old.bytes;
        }
        self.clock += 1;
        self.indices.insert(
            key,
            CachedIndex {
                bvh,
                bytes,
                last_used: self.clock,
            },
        );
        self.total_bytes += bytes;
        self.evict_to_budget(key);
    }

    /// Evicts least-recently-used entries until within budget, never the `keep` key.
    fn evict_to_budget(&mut self, keep: (u64, usize)) {
        if self.total_bytes <= self.budget {
            return;
        }
        let mut order: Vec<((u64, usize), u64)> = self
            .indices
            .iter()
            .filter(|&(key, _)| *key != keep)
            .map(|(key, entry)| (*key, entry.last_used))
            .collect();
        order.sort_by_key(|&(_, last_used)| last_used);

        let mut freed = 0usize;
        let mut evicted = 0usize;
        for (key, _) in order {
            if self.total_bytes <= self.budget {
                break;
            }
            if let Some(entry) = self.indices.remove(&key) {
                self.total_bytes -= entry.bytes;
                freed += entry.bytes;
                evicted += 1;
            }
        }
        if evicted > 0 {
            log::debug!(
                "raster: evicted {} cached page indices freeing {} bytes",
                evicted,
                freed,
            );
        }
    }

    /// Builds the synthetic region page, culling through the BVH once a page is large.
    ///
    /// Returns the page together with the number of items it draws and the page's total item
    /// count. Small pages keep the whole-frame clipped group; large pages draw only the queried
    /// items as one region-clipped group — the construction the experiment proved pixel-exact.
    pub(super) fn region_page(
        &mut self,
        page: &Page,
        key: (u64, usize),
        region: Region,
    ) -> (Page, usize, usize) {
        let total = page.frame.items().count();
        if total < SMALL_PAGE_ITEMS {
            return (whole_frame_region_page(page, region), total, total);
        }

        let size = page.frame.size();
        let bvh = self.bvh_for(key, page);
        let selected = bvh.query(clamped_region(region, size.x.to_pt(), size.y.to_pt()));
        let used = selected.len();
        (selected_region_page(page, &selected, region), used, total)
    }
}

/// Estimated heap bytes held by `bvh`, from its item and node counts.
///
/// The owned storage is one [`Extents`] plus one `u32` order slot per item, plus one
/// [`BVH_NODE_BYTES`] per node. This is a budgeting estimate, not a measured allocation.
fn bvh_byte_size(bvh: &PageBvh) -> usize {
    let items = bvh.len();
    let nodes = bvh.stats().nodes;
    items * (std::mem::size_of::<Extents>() + std::mem::size_of::<u32>()) + nodes * BVH_NODE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BVH over `count` unit extents laid out on a 64-wide grid.
    fn bvh_with_items(count: usize) -> PageBvh {
        let extents: Vec<Extents> = (0..count)
            .map(|i| {
                let x = (i % 64) as f64 * 4.0;
                let y = (i / 64) as f64 * 4.0;
                Extents::from_rect(x, y, 2.0, 2.0)
            })
            .collect();
        PageBvh::build(&extents)
    }

    /// A cache entry with an explicit size and use stamp, for eviction tests.
    fn cached(bytes: usize, last_used: u64) -> CachedIndex {
        CachedIndex {
            bvh: bvh_with_items(8),
            bytes,
            last_used,
        }
    }

    #[test]
    fn bvh_byte_size_is_positive_and_scales_with_item_count() {
        let small = bvh_with_items(64);
        let large = bvh_with_items(4096);

        assert!(bvh_byte_size(&small) > 0);
        assert!(bvh_byte_size(&large) > bvh_byte_size(&small));
        assert_eq!(
            bvh_byte_size(&large),
            large.len() * (std::mem::size_of::<Extents>() + std::mem::size_of::<u32>())
                + large.stats().nodes * BVH_NODE_BYTES
        );
    }

    #[test]
    fn bvh_byte_size_for_a_large_page_is_around_a_few_megabytes() {
        let bvh = bvh_with_items(40_000);
        let bytes = bvh_byte_size(&bvh);

        assert!(
            (1_000_000..=3_000_000).contains(&bytes),
            "40k-item page estimated at {bytes} bytes"
        );
    }

    #[test]
    fn index_cache_evicts_least_recently_used_entries() {
        let mut cache = PageIndexCache {
            budget: 250,
            ..PageIndexCache::default()
        };
        cache.indices.insert((1, 0), cached(100, 1));
        cache.indices.insert((1, 1), cached(100, 2));
        cache.indices.insert((1, 2), cached(100, 3));
        cache.total_bytes = 300;

        cache.evict_to_budget((1, 2));

        assert!(!cache.indices.contains_key(&(1, 0)));
        assert!(cache.indices.contains_key(&(1, 1)));
        assert!(cache.indices.contains_key(&(1, 2)));
        assert_eq!(cache.total_bytes, 200);
    }

    #[test]
    fn index_cache_never_evicts_the_key_it_is_inserting() {
        let mut cache = PageIndexCache {
            budget: 0,
            ..PageIndexCache::default()
        };
        cache.indices.insert((1, 0), cached(100, 1));
        cache.indices.insert((1, 1), cached(100, 2));
        cache.total_bytes = 200;

        cache.evict_to_budget((1, 1));

        assert!(cache.indices.contains_key(&(1, 1)));
        assert!(!cache.indices.contains_key(&(1, 0)));
        assert_eq!(cache.total_bytes, 100);
    }

    #[test]
    fn index_cache_budget_is_respected_across_inserts() {
        let one = bvh_byte_size(&bvh_with_items(256));
        let mut cache = PageIndexCache {
            budget: one,
            ..PageIndexCache::default()
        };
        for page in 0..3 {
            cache.insert_index((1, page), bvh_with_items(256));
        }

        assert!(cache.total_bytes <= cache.budget);
        assert!(cache.indices.contains_key(&(1, 2)));
    }

    #[test]
    fn index_cache_keeps_only_the_current_generation() {
        let mut cache = PageIndexCache::default();
        cache.insert_index((1, 0), bvh_with_items(64));
        cache.insert_index((1, 1), bvh_with_items(64));
        cache.insert_index((2, 0), bvh_with_items(64));

        cache.keep_generation(2);

        assert!(cache.indices.keys().all(|&(generation, _)| generation == 2));
        let expected: usize = cache.indices.values().map(|entry| entry.bytes).sum();
        assert!(expected > 0);
        assert_eq!(cache.total_bytes, expected);
    }
}
