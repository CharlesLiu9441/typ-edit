//! Shared deterministic RNG for the buffer integration tests.
//!
//! Each `tests/*.rs` file is its own crate that pulls this in with `mod
//! common;`, so the type is local to that crate. File-specific generators
//! (`ch`/`text`) are added with inherent `impl` blocks next to their use.

/// Deterministic xorshift64; no external dependency, so failures reproduce.
pub struct XorShift(pub u64);

impl XorShift {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n`. Every call site passes `n >= 1`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
