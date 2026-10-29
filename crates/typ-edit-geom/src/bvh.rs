//! A 2D axis-aligned bounding-volume hierarchy over conservative [`Extents`].
//!
//! Build is a median split on the axis with the larger centroid spread; recursion
//! stops at `leaf_size` items per leaf. Query returns every item whose extents
//! intersect the region, ascending. Because callers' extents over-approximate
//! ink, a returned set is a superset of the items that can paint inside the
//! region, so dropping the rest is safe.

use std::cmp::Ordering;

use crate::extents::Extents;

/// A flat tree node. Leaves cover a range of `order` slots; branches store their
/// two children.
#[derive(Clone, Copy, Debug)]
enum Node {
    Leaf {
        start: u32,
        end: u32,
        bounds: Extents,
    },
    Branch {
        left: u32,
        right: u32,
        bounds: Extents,
    },
}

impl Node {
    fn bounds(self) -> Extents {
        match self {
            Self::Leaf { bounds, .. } | Self::Branch { bounds, .. } => bounds,
        }
    }
}

/// Shape summary of a built tree.
#[derive(Clone, Copy, Debug)]
pub struct BvhStats {
    /// Number of tree nodes (leaves plus branches).
    pub nodes: usize,
    /// Number of leaf nodes.
    pub leaves: usize,
    /// Deepest leaf level.
    pub max_depth: usize,
    /// Maximum items per leaf.
    pub leaf_size: usize,
}

/// A static 2D AABB BVH over a slice of item extents.
pub struct Bvh {
    extents: Vec<Extents>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    root: Option<u32>,
    leaf_size: usize,
    max_depth: usize,
}

impl Bvh {
    /// Builds the tree with at most `leaf_size` items per leaf.
    ///
    /// `leaf_size` is clamped to at least 1, so an empty slice yields an empty
    /// tree and a one-item slice a single leaf.
    pub fn build(extents: &[Extents], leaf_size: usize) -> Self {
        let leaf_size = leaf_size.max(1);
        let mut builder = Builder {
            extents,
            order: (0..extents.len() as u32).collect(),
            leaf_size,
            nodes: Vec::new(),
            max_depth: 0,
        };
        let root = if extents.is_empty() {
            None
        } else {
            Some(builder.build(0, extents.len() as u32, 0))
        };
        Self {
            extents: extents.to_vec(),
            nodes: builder.nodes,
            order: builder.order,
            root,
            leaf_size,
            max_depth: builder.max_depth,
        }
    }

    /// Number of items indexed.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Shape summary.
    pub fn stats(&self) -> BvhStats {
        let leaves = self
            .nodes
            .iter()
            .filter(|node| matches!(node, Node::Leaf { .. }))
            .count();
        BvhStats {
            nodes: self.nodes.len(),
            leaves,
            max_depth: self.max_depth,
            leaf_size: self.leaf_size,
        }
    }

    /// The root bounding box, or a zero box for an empty tree.
    pub fn bounds(&self) -> Extents {
        self.root
            .map(|root| self.nodes[root as usize].bounds())
            .unwrap_or_else(|| Extents::from_rect(0.0, 0.0, 0.0, 0.0))
    }

    /// Collects the indices of items whose extents intersect `region`, ascending.
    pub fn query(&self, region: Extents) -> Vec<u32> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut items = Vec::new();
        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            match self.nodes[index as usize] {
                Node::Leaf { start, end, bounds } => {
                    if !bounds.intersects(region) {
                        continue;
                    }
                    for slot in start..end {
                        let item = self.order[slot as usize];
                        if self.extents[item as usize].intersects(region) {
                            items.push(item);
                        }
                    }
                }
                Node::Branch {
                    left,
                    right,
                    bounds,
                } => {
                    if bounds.intersects(region) {
                        stack.push(left);
                        stack.push(right);
                    }
                }
            }
        }
        items.sort_unstable();
        items
    }
}

/// Mutable state of one depth-first build.
struct Builder<'a> {
    extents: &'a [Extents],
    order: Vec<u32>,
    leaf_size: usize,
    nodes: Vec<Node>,
    max_depth: usize,
}

impl Builder<'_> {
    fn build(&mut self, start: u32, end: u32, depth: usize) -> u32 {
        let bounds = range_bounds(self.extents, &self.order[start as usize..end as usize]);
        if (end - start) as usize <= self.leaf_size {
            self.max_depth = self.max_depth.max(depth);
            let index = self.nodes.len() as u32;
            self.nodes.push(Node::Leaf { start, end, bounds });
            return index;
        }

        let extents = self.extents;
        let (axis_x, mid) = choose_split(extents, &self.order, start, end);
        self.order[start as usize..end as usize].sort_by(|&a, &b| {
            let (ca, cb) = if axis_x {
                (
                    extents[a as usize].center_x(),
                    extents[b as usize].center_x(),
                )
            } else {
                (
                    extents[a as usize].center_y(),
                    extents[b as usize].center_y(),
                )
            };
            ca.partial_cmp(&cb).unwrap_or(Ordering::Equal)
        });

        let left = self.build(start, mid, depth + 1);
        let right = self.build(mid, end, depth + 1);
        let index = self.nodes.len() as u32;
        self.nodes.push(Node::Branch {
            left,
            right,
            bounds,
        });
        index
    }
}

/// Picks the split axis (larger centroid spread) and the median slot.
fn choose_split(extents: &[Extents], order: &[u32], start: u32, end: u32) -> (bool, u32) {
    let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y_min, mut y_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for &item in &order[start as usize..end as usize] {
        let e = extents[item as usize];
        let (cx, cy) = (e.center_x(), e.center_y());
        x_min = x_min.min(cx);
        x_max = x_max.max(cx);
        y_min = y_min.min(cy);
        y_max = y_max.max(cy);
    }
    (x_max - x_min >= y_max - y_min, start + (end - start) / 2)
}

/// Union of the extents of the items in `slots`.
fn range_bounds(extents: &[Extents], slots: &[u32]) -> Extents {
    let mut iter = slots.iter().map(|&item| extents[item as usize]);
    let Some(first) = iter.next() else {
        return Extents::from_rect(0.0, 0.0, 0.0, 0.0);
    };
    iter.fold(first, Extents::union)
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
    fn empty_tree_queries_are_safe() {
        let bvh = Bvh::build(&[], 4);
        assert!(bvh.is_empty());
        assert_eq!(bvh.len(), 0);
        assert_eq!(bvh.stats().nodes, 0);
        assert!(bvh.query(region(0.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn query_matches_brute_force_over_a_deterministic_grid() {
        let extents: Vec<Extents> = (0..256)
            .map(|i| {
                let cx = ((i * 37) % 64) as f64 * 7.0;
                let cy = ((i * 53) % 64) as f64 * 9.0;
                e(cx, cy, cx + 3.0, cy + 5.0)
            })
            .collect();

        for leaf in [4, 8, 16] {
            let bvh = Bvh::build(&extents, leaf);
            for q in 0..40 {
                let x = (q * 41 % 400) as f64;
                let y = (q * 67 % 500) as f64;
                let rect = region(x, y, 60.0, 80.0);
                let expected: Vec<u32> = (0..extents.len() as u32)
                    .filter(|&i| extents[i as usize].intersects(rect))
                    .collect();
                assert_eq!(bvh.query(rect), expected, "leaf {leaf}, query {q}");
            }
        }
    }
}
