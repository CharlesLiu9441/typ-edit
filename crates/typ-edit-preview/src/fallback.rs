//! Memory-bounded store of whole-page fallback textures and its eviction policy.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops::RangeInclusive;

/// Whole-page fallback textures are evicted once their total size exceeds this budget.
pub(crate) const FALLBACK_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// Pages within this many indices of the visible range are evicted after farther ones.
const FALLBACK_NEIGHBORHOOD: usize = 3;

/// A whole-page fallback raster (adjustable DPI, set in Settings).
pub(crate) struct FallbackFrame {
    pub(crate) pixel_per_pt: f32,
    pub(crate) texture: egui::TextureHandle,
    /// Heap bytes held by the uploaded texture (`width * height * 4`).
    pub(crate) bytes: usize,
    /// Monotonic use stamp, refreshed while the page is visible.
    pub(crate) last_used: u64,
}

/// Memory-bounded store of whole-page fallback textures keyed by page index.
///
/// Entries accumulate as pages become visible. Once the running byte total exceeds
/// [`FALLBACK_BUDGET_BYTES`] the cache evicts pages farther than [`FALLBACK_NEIGHBORHOOD`]
/// indices outside the visible range first (farthest first, least-recently-used within a
/// distance class); visible pages are only evicted when one alone exceeds the budget.
#[derive(Default)]
pub(crate) struct FallbackCache {
    frames: HashMap<usize, FallbackFrame>,
    bytes: usize,
    clock: u64,
}

impl FallbackCache {
    /// Drops every cached fallback (the document generation changed).
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Whether a fallback for `page` is cached.
    pub(crate) fn contains(&self, page: usize) -> bool {
        self.frames.contains_key(&page)
    }

    /// The cached fallback for `page`, if any.
    pub(crate) fn get(&self, page: usize) -> Option<&FallbackFrame> {
        self.frames.get(&page)
    }

    /// Stores `frame` for `page`, replacing any previous entry.
    pub(crate) fn insert(&mut self, page: usize, frame: FallbackFrame) {
        let bytes = frame.bytes;
        match self.frames.insert(page, frame) {
            Some(old) => self.bytes = self.bytes - old.bytes + bytes,
            None => self.bytes += bytes,
        }
    }

    /// Stamps every visible page that is cached as most-recently-used.
    pub(crate) fn touch_visible(&mut self, visible: &[usize]) {
        for &page in visible {
            if let Some(frame) = self.frames.get_mut(&page) {
                self.clock += 1;
                frame.last_used = self.clock;
            }
        }
    }

    /// Evicts entries until the byte total fits `budget` (or nothing is evictable).
    pub(crate) fn evict_to_budget(&mut self, visible: RangeInclusive<usize>, budget: usize) {
        if self.bytes <= budget {
            return;
        }
        let plan = eviction_plan(&self.snapshot(), visible, budget);
        if plan.is_empty() {
            return;
        }
        let mut freed = 0usize;
        for page in &plan {
            if let Some(frame) = self.frames.remove(page) {
                freed += frame.bytes;
                self.bytes -= frame.bytes;
            }
        }
        log::debug!(
            "preview: evicted {} fallback textures freeing {} bytes",
            plan.len(),
            freed,
        );
    }

    /// A `(page, bytes, last_used)` snapshot handed to the eviction policy.
    fn snapshot(&self) -> Vec<(usize, usize, u64)> {
        self.frames
            .iter()
            .map(|(&page, frame)| (page, frame.bytes, frame.last_used))
            .collect()
    }
}

/// Pages in eviction priority order: pages farther than `neighborhood` indices outside
/// `visible` first (farthest first), then nearer non-visible pages (farthest first), with
/// the least-recently-used page first within a distance class, and visible pages last.
///
/// `entries` are `(page, bytes, last_used)`.
fn eviction_order(
    entries: &[(usize, usize, u64)],
    visible: RangeInclusive<usize>,
    neighborhood: usize,
) -> Vec<usize> {
    let (start, end) = (*visible.start(), *visible.end());
    let distance = |page: usize| -> usize {
        if page < start {
            start - page
        } else {
            page.saturating_sub(end)
        }
    };

    let mut ordered = entries.to_vec();
    ordered.sort_by_key(|&(page, _, last_used)| {
        let distance = distance(page);
        let class = if distance == 0 {
            2u8
        } else if distance > neighborhood {
            0
        } else {
            1
        };
        (class, Reverse(distance), last_used)
    });
    ordered.into_iter().map(|(page, _, _)| page).collect()
}

/// Pages to evict, in order, until the running total fits `budget`.
///
/// Visible pages are skipped unless one alone holds more than `budget` bytes; when no
/// evictable page remains the plan ends, so the caller never loops.
fn eviction_plan(
    entries: &[(usize, usize, u64)],
    visible: RangeInclusive<usize>,
    budget: usize,
) -> Vec<usize> {
    let mut remaining: usize = entries.iter().map(|&(_, bytes, _)| bytes).sum();
    if remaining <= budget {
        return Vec::new();
    }

    let mut plan = Vec::new();
    for page in eviction_order(entries, visible.clone(), FALLBACK_NEIGHBORHOOD) {
        if remaining <= budget {
            break;
        }
        let Some((_, bytes, _)) = entries.iter().find(|&&(candidate, _, _)| candidate == page)
        else {
            continue;
        };
        if visible.contains(&page) && *bytes <= budget {
            continue;
        }
        remaining = remaining.saturating_sub(*bytes);
        plan.push(page);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_order_is_farthest_first_then_lru_and_visible_last() {
        let entries = [
            (10usize, 100usize, 1u64),
            (8, 100, 5),
            (12, 100, 3),
            (0, 100, 2),
        ];

        let order = eviction_order(&entries, 10..=10, 3);

        assert_eq!(order, vec![0, 12, 8, 10]);
    }

    #[test]
    fn eviction_order_uses_lru_within_one_distance_class() {
        let entries = [(5usize, 10usize, 30u64), (15, 10, 10), (25, 10, 20)];

        let order = eviction_order(&entries, 10..=10, 3);

        assert_eq!(order, vec![25, 15, 5]);
    }

    #[test]
    fn eviction_plan_evicts_exactly_until_under_budget() {
        let entries = [(10usize, 40usize, 1u64), (20, 40, 2), (30, 40, 3)];

        let plan = eviction_plan(&entries, 1..=1, 100);

        assert_eq!(plan, vec![30]);
        let freed: usize = plan
            .iter()
            .map(|page| entries.iter().find(|entry| entry.0 == *page).unwrap().1)
            .sum();
        assert_eq!(120 - freed, 80);
    }

    #[test]
    fn eviction_plan_keeps_visible_pages_and_drops_far_ones_first() {
        let entries = [(5usize, 100usize, 1u64), (100, 100, 2)];

        let plan = eviction_plan(&entries, 5..=5, 150);

        assert_eq!(plan, vec![100]);
    }

    #[test]
    fn eviction_plan_evicts_a_visible_page_only_when_it_alone_exceeds_budget() {
        let entries = [(5usize, 300usize, 1u64), (100, 50, 2)];

        let plan = eviction_plan(&entries, 5..=5, 256);

        assert_eq!(plan, vec![100, 5]);
    }

    #[test]
    fn eviction_plan_stops_when_only_in_budget_visible_pages_remain() {
        let entries = [(5usize, 200usize, 1u64), (6, 200, 2)];

        let plan = eviction_plan(&entries, 5..=6, 256);

        assert!(plan.is_empty());
    }

    #[test]
    fn eviction_plan_is_empty_when_within_budget() {
        let entries = [(5usize, 100usize, 1u64)];

        assert!(eviction_plan(&entries, 5..=5, 256).is_empty());
    }
}
