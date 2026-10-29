use super::*;

struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        XorShift(seed)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Naive model: a sorted `Vec<usize>` of newline positions.
#[derive(Default)]
struct Naive {
    v: Vec<usize>,
}

impl Naive {
    fn insert(&mut self, pos: usize) -> bool {
        if let Err(i) = self.v.binary_search(&pos) {
            self.v.insert(i, pos);
            true
        } else {
            false
        }
    }

    fn remove(&mut self, pos: usize) -> bool {
        if let Ok(i) = self.v.binary_search(&pos) {
            self.v.remove(i);
            true
        } else {
            false
        }
    }

    fn try_shift_ge(&mut self, pos: usize, delta: i64) -> bool {
        let mut next = self.v.clone();
        for x in &mut next {
            if *x >= pos {
                let shifted = *x as i64 + delta;
                if shifted < 0 {
                    return false;
                }
                *x = shifted as usize;
            }
        }
        if !next.windows(2).all(|w| w[0] < w[1]) {
            return false;
        }
        self.v = next;
        true
    }

    fn count_lt(&self, pos: usize) -> usize {
        self.v.partition_point(|&x| x < pos)
    }

    fn kth(&self, k: usize) -> Option<usize> {
        self.v.get(k).copied()
    }
}

fn assert_ascending_unique(index: &LineIndex) {
    let v = index.to_vec();
    assert!(
        v.windows(2).all(|w| w[0] < w[1]),
        "not strictly ascending: {v:?}"
    );
    assert_eq!(v.len(), index.len());
}

#[test]
fn insert_remove_keeps_order() {
    let mut index = LineIndex::new();
    for pos in [5usize, 1, 9, 3, 7] {
        index.insert(pos);
        assert_ascending_unique(&index);
    }
    assert_eq!(index.to_vec(), vec![1, 3, 5, 7, 9]);
    index.remove(5);
    assert_ascending_unique(&index);
    assert_eq!(index.to_vec(), vec![1, 3, 7, 9]);
    index.remove(1);
    index.remove(9);
    assert_eq!(index.to_vec(), vec![3, 7]);
    index.remove(100);
    assert_eq!(index.to_vec(), vec![3, 7]);
}

#[test]
fn shift_moves_only_suffix() {
    let mut index = LineIndex::new();
    for pos in [1usize, 3, 5, 7] {
        index.insert(pos);
    }
    index.shift_ge(5, 10);
    assert_ascending_unique(&index);
    assert_eq!(index.to_vec(), vec![1, 3, 15, 17]);
    index.shift_ge(0, 3);
    assert_eq!(index.to_vec(), vec![4, 6, 18, 20]);
    index.shift_ge(4, -4);
    assert_eq!(index.to_vec(), vec![0, 2, 14, 16]);
    index.shift_ge(999, 50);
    assert_eq!(index.to_vec(), vec![0, 2, 14, 16]);
}

#[test]
fn count_lt_and_kth_agree_with_in_order() {
    let mut index = LineIndex::new();
    for pos in [2usize, 4, 6, 8, 10] {
        index.insert(pos);
    }
    index.shift_ge(6, 5);
    let expected = [2usize, 4, 11, 13, 15];
    assert_eq!(index.to_vec(), expected);
    for probe in 0usize..20 {
        assert_eq!(
            index.count_lt(probe),
            expected.iter().filter(|&&x| x < probe).count()
        );
    }
    for (k, &value) in expected.iter().enumerate() {
        assert_eq!(index.kth(k), Some(value));
    }
    assert_eq!(index.kth(expected.len()), None);
}

#[test]
fn differential_against_naive_model() {
    let mut rng = XorShift::new(0xDEAD_BEEF_1234_5678);
    let mut index = LineIndex::new();
    let mut naive = Naive::default();

    for step in 0..20_000 {
        match rng.below(6) {
            0 | 1 => {
                let pos = rng.below(600);
                if naive.insert(pos) {
                    index.insert(pos);
                }
            }
            2 | 3 => {
                let pos = rng.below(600);
                if naive.remove(pos) {
                    index.remove(pos);
                }
            }
            4 => {
                let pos = rng.below(600);
                let delta = rng.below(7) as i64 - 3;
                if naive.try_shift_ge(pos, delta) {
                    index.shift_ge(pos, delta);
                }
            }
            _ => {
                let pos = rng.below(650);
                assert_eq!(
                    index.count_lt(pos),
                    naive.count_lt(pos),
                    "rank mismatch at step {step}, pos {pos}"
                );
                if !naive.v.is_empty() {
                    let k = rng.below(naive.v.len());
                    assert_eq!(index.kth(k), naive.kth(k), "kth mismatch at step {step}");
                }
            }
        }
        assert_eq!(index.to_vec(), naive.v, "state mismatch at step {step}");
        assert_ascending_unique(&index);
    }
    assert!(index.len() > 0);
}
