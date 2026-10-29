//! A 2D axis-aligned bounding-volume hierarchy over the items of a typst [`Frame`].
//!
//! Frame ownership, per-item child trees and the tree identity live here; the node
//! tree and its query live in the shared [`typ_edit_geom::Bvh`]. The tree indexes
//! conservative per-item [`Extents`] (see [`crate::bounds`]). Because those
//! extents over-approximate ink, a query returns a superset of the items that can
//! paint inside the region, which makes culling safe: dropping the other items
//! leaves the rendered output pixel-equal to a full traversal.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use typ_edit_geom::{Bvh, BvhStats, Extents};
use typst_library::layout::{Frame, FrameItem, Point};

use crate::bounds::{EXTENT_PAD_PT, item_extents};

/// The number of items per leaf used by [`FrameBvh::new`].
const DEFAULT_LEAF_SIZE: usize = 8;

/// Source of process-unique [`FrameBvh`] ids.
static NEXT_BVH_ID: AtomicU64 = AtomicU64::new(0);

fn next_bvh_id() -> u64 {
    NEXT_BVH_ID.fetch_add(1, AtomicOrdering::Relaxed)
}

/// One item's conservative extents plus its prebuilt child tree (groups only).
struct ItemEntry {
    extents: Extents,
    child: Option<FrameBvh>,
}

/// A static 2D AABB BVH over the items of a [`Frame`].
pub struct FrameBvh {
    id: u64,
    frame: Frame,
    entries: Vec<ItemEntry>,
    core: Bvh,
}

impl FrameBvh {
    /// Builds the tree with the default leaf size of 8.
    pub fn new(frame: Frame) -> Self {
        Self::with_leaf_size(frame, DEFAULT_LEAF_SIZE)
    }

    /// Builds the tree with at most `leaf_size` items per leaf.
    pub fn with_leaf_size(frame: Frame, leaf_size: usize) -> Self {
        let leaf_size = leaf_size.max(1);
        let entries: Vec<ItemEntry> = frame
            .items()
            .map(|(pos, item)| {
                let child = match item {
                    FrameItem::Group(group) => {
                        Some(FrameBvh::with_leaf_size(group.frame.clone(), leaf_size))
                    }
                    _ => None,
                };
                let extents = item_extents(*pos, item, child.as_ref().map(FrameBvh::bounds))
                    .pad(EXTENT_PAD_PT);
                ItemEntry { extents, child }
            })
            .collect();

        let extents: Vec<Extents> = entries.iter().map(|entry| entry.extents).collect();
        let core = Bvh::build(&extents, leaf_size);

        Self {
            id: next_bvh_id(),
            frame,
            entries,
            core,
        }
    }

    /// A process-unique identifier, stable for the life of this tree.
    ///
    /// Rendering caches key on this id, so a caller that rebuilds the tree each
    /// frame gets no reuse; build once and keep the tree.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The frame this tree was built from.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Number of indexed items.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tree contains no items.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Shape summary of the tree.
    pub fn stats(&self) -> BvhStats {
        self.core.stats()
    }

    /// The root bounding box, or a zero box for an empty tree.
    pub fn bounds(&self) -> Extents {
        self.core.bounds()
    }

    /// Collects the indices of items whose extents intersect `region`, ascending.
    pub fn query(&self, region: Extents) -> Vec<u32> {
        self.core.query(region)
    }

    /// The position and item at `index`.
    ///
    /// `index` is always one this tree produced (from [`Self::query`] or the
    /// frame's item range), so the bounds invariant is only asserted in debug
    /// builds; the release path is a plain slice index with no deliberate
    /// panic.
    pub(crate) fn item_at(&self, index: u32) -> (Point, &FrameItem) {
        let items = self.frame.items().as_slice();
        debug_assert!(
            (index as usize) < items.len(),
            "bvh item index out of range"
        );
        let (pos, item) = &items[index as usize];
        (*pos, item)
    }

    /// The prebuilt child tree for a group item at `index`, if any.
    pub(crate) fn child_bvh(&self, index: u32) -> Option<&FrameBvh> {
        self.entries[index as usize].child.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst_library::layout::{Abs, Size};
    use typst_library::visualize::{Color, Geometry};
    use typst_syntax::Span;

    const LEAF_SIZES: [usize; 3] = [4, 8, 16];

    fn e(x0: f64, y0: f64, x1: f64, y1: f64) -> Extents {
        Extents { x0, x1, y0, y1 }
    }

    fn region(x: f64, y: f64, w: f64, h: f64) -> Extents {
        Extents::from_rect(x, y, w, h)
    }

    fn rect_frame(size: f64, items: &[(f64, f64, f64, f64)]) -> Frame {
        let mut frame = Frame::soft(Size::new(Abs::pt(size), Abs::pt(size)));
        for &(x, y, w, h) in items {
            frame.push(
                Point::new(Abs::pt(x), Abs::pt(y)),
                FrameItem::Shape(
                    Geometry::Rect(Size::new(Abs::pt(w), Abs::pt(h))).filled(Color::RED),
                    Span::detached(),
                ),
            );
        }
        frame
    }

    #[test]
    fn query_returns_exactly_the_intersecting_items() {
        // Four shapes placed in the corners of a 120x120 frame.
        let frame = rect_frame(
            120.0,
            &[
                (0.0, 0.0, 10.0, 10.0),
                (100.0, 0.0, 10.0, 10.0),
                (0.0, 100.0, 10.0, 10.0),
                (100.0, 100.0, 10.0, 10.0),
            ],
        );
        let bvh = FrameBvh::with_leaf_size(frame, 2);

        let all = bvh.query(region(-5.0, -5.0, 200.0, 200.0));
        assert_eq!(all, vec![0, 1, 2, 3]);

        let corner = bvh.query(region(95.0, 95.0, 20.0, 20.0));
        assert_eq!(corner, vec![3]);

        let none = bvh.query(region(-50.0, -50.0, 10.0, 10.0));
        assert!(none.is_empty());
    }

    #[test]
    fn empty_tree_queries_are_safe() {
        let bvh = FrameBvh::new(Frame::soft(Size::new(Abs::pt(10.0), Abs::pt(10.0))));
        assert!(bvh.is_empty());
        assert_eq!(bvh.len(), 0);
        assert_eq!(bvh.stats().nodes, 0);
        assert!(bvh.query(region(0.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn query_matches_brute_force_over_a_deterministic_grid() {
        let items: Vec<(f64, f64, f64, f64)> = (0..256)
            .map(|i| {
                let cx = ((i * 37) % 64) as f64 * 7.0;
                let cy = ((i * 53) % 64) as f64 * 9.0;
                (cx, cy, 3.0, 5.0)
            })
            .collect();
        let frame = rect_frame(4096.0, &items);

        for leaf in LEAF_SIZES {
            let bvh = FrameBvh::with_leaf_size(frame.clone(), leaf);
            let extents: Vec<Extents> = bvh.entries.iter().map(|entry| entry.extents).collect();
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
        let items: Vec<(f64, f64, f64, f64)> =
            (0..100).map(|i| (i as f64, 0.0, 1.0, 1.0)).collect();
        let frame = rect_frame(200.0, &items);
        let bvh = FrameBvh::with_leaf_size(frame, 4);
        let stats = bvh.stats();
        assert_eq!(stats.leaf_size, 4);
        assert!(stats.leaves >= 25, "leaves {}", stats.leaves);
        assert!(stats.max_depth <= 6, "depth {}", stats.max_depth);
        assert_eq!(bvh.len(), 100);
    }

    #[test]
    fn item_extents_are_padded() {
        let frame = rect_frame(10.0, &[(2.0, 3.0, 4.0, 5.0)]);
        let bvh = FrameBvh::new(frame);
        // Raw geometry is (2,3)-(6,8); padded by EXTENT_PAD_PT (1).
        assert_eq!(bvh.entries[0].extents, e(1.0, 2.0, 7.0, 9.0));
    }

    #[test]
    fn group_bounds_union_child_items() {
        let mut child = Frame::soft(Size::new(Abs::pt(20.0), Abs::pt(20.0)));
        child.push(
            Point::new(Abs::pt(0.0), Abs::pt(0.0)),
            FrameItem::Shape(
                Geometry::Rect(Size::new(Abs::pt(4.0), Abs::pt(4.0))).filled(Color::RED),
                Span::detached(),
            ),
        );
        let mut root = Frame::soft(Size::new(Abs::pt(20.0), Abs::pt(20.0)));
        root.push(
            Point::new(Abs::pt(1.0), Abs::pt(2.0)),
            FrameItem::Group(typst_library::layout::GroupItem::new(child)),
        );
        let bvh = FrameBvh::new(root);
        // Child item padded to (-1,-1)-(5,5); group at (1,2) -> (0,1)-(6,7);
        // then the root's own EXTENT_PAD_PT -> (-1,0)-(7,8).
        assert_eq!(bvh.bounds(), e(-1.0, 0.0, 7.0, 8.0));
        assert!(bvh.child_bvh(0).is_some());
        assert!(bvh.child_bvh(0).unwrap().len() == 1);
    }
}
