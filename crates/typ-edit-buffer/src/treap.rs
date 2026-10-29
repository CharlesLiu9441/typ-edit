//! Incremental line index: an FHQ treap storing the char position of every
//! `\n` in ascending, unique order, with lazy range-add so shifting a whole
//! suffix is O(log n).
//!
//! # Lazy invariant
//!
//! A node's `key` already reflects its **own** `lazy`, but not the `lazy` of any
//! ancestor: the true position is `key + sum(ancestor lazies)`. `push` folds a
//! node's lazy into both children (updating their keys and lazies) and zeroes
//! it. `split` and `merge` both `push` every node they descend into, so any node
//! whose child links change has no pending lazy of its own before the change.
//! Read-only traversals (`count_lt`, `kth`) instead carry the accumulated
//! ancestor lazy down so they never need `&mut`.

const RNG_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

struct Node {
    /// Position including this node's own `lazy`, excluding ancestor lazies.
    key: i64,
    prio: u64,
    lazy: i64,
    size: usize,
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
}

fn subtree_size(n: &Option<Box<Node>>) -> usize {
    n.as_ref().map_or(0, |node| node.size)
}

fn update(n: &mut Node) {
    n.size = 1 + subtree_size(&n.left) + subtree_size(&n.right);
}

/// Folds `n`'s pending lazy into its children, clearing it on `n`.
fn push(n: &mut Node) {
    let delta = n.lazy;
    if delta != 0 {
        if let Some(child) = n.left.as_mut() {
            child.key += delta;
            child.lazy += delta;
        }
        if let Some(child) = n.right.as_mut() {
            child.key += delta;
            child.lazy += delta;
        }
        n.lazy = 0;
    }
}

/// Splits `root` into `(< key, >= key)`.
fn split(root: Option<Box<Node>>, key: i64) -> (Option<Box<Node>>, Option<Box<Node>>) {
    match root {
        None => (None, None),
        Some(mut n) => {
            push(&mut n);
            if n.key < key {
                let (left, right) = split(n.right.take(), key);
                n.right = left;
                update(&mut n);
                (Some(n), right)
            } else {
                let (left, right) = split(n.left.take(), key);
                n.left = right;
                update(&mut n);
                (left, Some(n))
            }
        }
    }
}

/// Merges two trees where every key in `a` is strictly less than every key in
/// `b`. Pushes the chosen root so newly attached nodes never inherit its lazy.
fn merge(a: Option<Box<Node>>, b: Option<Box<Node>>) -> Option<Box<Node>> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(mut a), Some(mut b)) => {
            if a.prio >= b.prio {
                push(&mut a);
                let right = merge(a.right.take(), Some(b));
                a.right = right;
                update(&mut a);
                Some(a)
            } else {
                push(&mut b);
                let left = merge(Some(a), b.left.take());
                b.left = left;
                update(&mut b);
                Some(b)
            }
        }
    }
}

/// The FHQ treap line index (internal; positions of all newline characters).
pub(crate) struct LineIndex {
    root: Option<Box<Node>>,
    rng: u64,
}

impl LineIndex {
    pub(crate) fn new() -> Self {
        LineIndex {
            root: None,
            rng: RNG_SEED,
        }
    }

    /// xorshift64*: deterministic, dependency-free priority source.
    fn next_rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Number of indexed newline positions.
    pub(crate) fn len(&self) -> usize {
        subtree_size(&self.root)
    }

    /// Inserts `pos`; callers guarantee positions stay unique.
    pub(crate) fn insert(&mut self, pos: usize) {
        let prio = self.next_rand();
        let (left, right) = split(self.root.take(), pos as i64);
        let node = Box::new(Node {
            key: pos as i64,
            prio,
            lazy: 0,
            size: 1,
            left: None,
            right: None,
        });
        self.root = merge(merge(left, Some(node)), right);
    }

    /// Removes an existing `pos` (no-op if absent).
    pub(crate) fn remove(&mut self, pos: usize) {
        let (left, mid) = split(self.root.take(), pos as i64);
        let (_exact, right) = split(mid, pos as i64 + 1);
        self.root = merge(left, right);
    }

    /// Adds `delta` to every position `>= pos`.
    pub(crate) fn shift_ge(&mut self, pos: usize, delta: i64) {
        if delta == 0 {
            return;
        }
        let (left, mut right) = split(self.root.take(), pos as i64);
        if let Some(node) = right.as_mut() {
            node.key += delta;
            node.lazy += delta;
        }
        self.root = merge(left, right);
    }

    /// Number of positions strictly less than `pos` (rank / line lookup).
    pub(crate) fn count_lt(&self, pos: usize) -> usize {
        let target = pos as i64;
        let mut acc = 0i64;
        let mut count = 0usize;
        let mut cur = self.root.as_deref();
        while let Some(n) = cur {
            let key = n.key + acc;
            acc += n.lazy;
            if key < target {
                count += 1 + subtree_size(&n.left);
                cur = n.right.as_deref();
            } else {
                cur = n.left.as_deref();
            }
        }
        count
    }

    /// The `k`th smallest position (0-based).
    pub(crate) fn kth(&self, k: usize) -> Option<usize> {
        let mut acc = 0i64;
        let mut k = k;
        let mut cur = self.root.as_deref();
        loop {
            let n = cur?;
            let left_size = subtree_size(&n.left);
            let lazy = n.lazy;
            let key = n.key + acc;
            if k < left_size {
                acc += lazy;
                cur = n.left.as_deref();
            } else if k == left_size {
                return Some(key as usize);
            } else {
                k -= left_size + 1;
                acc += lazy;
                cur = n.right.as_deref();
            }
        }
    }
}

#[cfg(test)]
fn collect(n: &Option<Box<Node>>, acc: i64, out: &mut Vec<usize>) {
    if let Some(n) = n {
        let child_acc = acc + n.lazy;
        collect(&n.left, child_acc, out);
        out.push((n.key + acc) as usize);
        collect(&n.right, child_acc, out);
    }
}

impl LineIndex {
    /// In-order positions, ascending; used by tests to check invariants.
    #[cfg(test)]
    pub(crate) fn to_vec(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.len());
        collect(&self.root, 0, &mut out);
        out
    }
}

#[cfg(test)]
mod tests;
