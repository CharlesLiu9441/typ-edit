//! A 2D axis-aligned bounding-volume hierarchy over per-item conservative extents.
//!
//! The tree itself lives in [`typ_edit_geom::Bvh`], shared with the GPU canvas's frame index;
//! this module is the engine-facing wrapper. Build is a median split on the axis with the
//! larger centroid spread; recursion stops at [`LEAF_SIZE`] items per leaf. Query returns
//! every item whose AABB intersects the render region, ascending. The AABBs over-approximate
//! ink (see [`crate::page_bounds`]), so a returned set is a superset of the items that can
//! paint inside the region. Items outside it are safe to drop — which is what makes the
//! culled render pixel-equal to the whole-page baseline (validated in `experiments/band-bench`).

use typ_edit_geom::{Bvh, BvhStats, Extents};

/// Items per leaf. 16 matched the experiment's best selectivity/cost trade-off.
pub const LEAF_SIZE: usize = 16;

/// Leaf sizes exercised by the brute-force equivalence tests.
#[cfg(test)]
const TEST_LEAF_SIZES: [usize; 3] = [4, 8, 16];

/// A static 2D AABB BVH over a page's item extents.
pub struct PageBvh {
    core: Bvh,
}

impl PageBvh {
    /// Builds the tree with [`LEAF_SIZE`] items per leaf.
    pub fn build(extents: &[Extents]) -> Self {
        Self::build_with_leaf_size(extents, LEAF_SIZE)
    }

    /// Builds the tree with an explicit leaf size (tests sweep smaller leaves).
    fn build_with_leaf_size(extents: &[Extents], leaf_size: usize) -> Self {
        Self {
            core: Bvh::build(extents, leaf_size),
        }
    }

    /// Number of items indexed.
    pub fn len(&self) -> usize {
        self.core.len()
    }

    /// Whether the tree is empty.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.core.is_empty()
    }

    /// Shape summary.
    pub fn stats(&self) -> BvhStats {
        self.core.stats()
    }

    /// Collects the indices of items whose extents intersect `region`, ascending.
    pub fn query(&self, region: Extents) -> Vec<u32> {
        self.core.query(region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(x0: f64, y0: f64, x1: f64, y1: f64) -> Extents {
        Extents { x0, x1, y0, y1 }
    }

    fn region(x: f64, y: f64, w: f64, h: f64) -> Extents {
        Extents::from_rect(x, y, w, h)
    }

    #[test]
    fn query_returns_exactly_the_intersecting_items() {
        let extents = vec![
            e(0.0, 0.0, 10.0, 10.0),
            e(100.0, 0.0, 110.0, 10.0),
            e(0.0, 100.0, 10.0, 110.0),
            e(100.0, 100.0, 110.0, 110.0),
        ];
        let bvh = PageBvh::build_with_leaf_size(&extents, 2);
        let hit = bvh.query(region(5.0, 5.0, 200.0, 200.0));
        assert_eq!(hit, vec![0, 1, 2, 3]);

        let corner = bvh.query(region(95.0, 95.0, 20.0, 20.0));
        assert_eq!(corner, vec![3]);

        let none = bvh.query(region(-20.0, -20.0, 10.0, 10.0));
        assert!(none.is_empty());
    }

    #[test]
    fn empty_tree_queries_are_safe() {
        let bvh = PageBvh::build(&[]);
        assert!(bvh.is_empty());
        assert_eq!(bvh.stats().nodes, 0);
        assert!(bvh.query(region(0.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn matches_brute_force_over_a_deterministic_grid() {
        let extents: Vec<Extents> = (0..256)
            .map(|i| {
                let cx = ((i * 37) % 64) as f64 * 7.0;
                let cy = ((i * 53) % 64) as f64 * 9.0;
                e(cx, cy, cx + 3.0, cy + 5.0)
            })
            .collect();

        for leaf in TEST_LEAF_SIZES {
            let bvh = PageBvh::build_with_leaf_size(&extents, leaf);
            for q in 0..40 {
                let x = (q * 41 % 400) as f64;
                let y = (q * 67 % 500) as f64;
                let rect = region(x, y, 60.0, 80.0);
                let expected: Vec<u32> = (0..extents.len() as u32)
                    .filter(|&i| extents[i as usize].intersects(rect))
                    .collect();
                let got = bvh.query(rect);
                assert_eq!(got, expected, "leaf {leaf}, query {q}");
            }
        }
    }

    #[test]
    fn stats_report_leaves_and_depth_for_a_balanced_split() {
        let extents: Vec<Extents> = (0..100)
            .map(|i| e(i as f64, 0.0, i as f64 + 1.0, 1.0))
            .collect();
        let bvh = PageBvh::build_with_leaf_size(&extents, 4);
        let stats = bvh.stats();
        assert!(stats.leaves >= 25);
        assert!(stats.max_depth <= 6, "depth {}", stats.max_depth);
        assert_eq!(bvh.len(), 100);
    }
}
