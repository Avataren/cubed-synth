//! Small helpers for porting the DAW's TypeScript: JavaScript's number
//! semantics where they differ from Rust's, and the ordered maps a port that
//! must pick the same winner on a tie needs (a JS `Map` iterates in insertion
//! order).

/// JavaScript's `Math.round`: halves round up (towards +infinity), unlike
/// Rust's `round`, which rounds them away from zero.
#[inline]
pub fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// `Math.round` of an exact ratio of two integers (`a / b`, `b > 0`), as an integer.
#[inline]
pub fn round_div(a: i64, b: i64) -> i64 {
    js_round(a as f64 / b as f64) as i64
}

/// A map that iterates in insertion order (what a JS `Map` does), for the few
/// places where the order of iteration decides a tie. Small: lookups are linear.
#[derive(Clone, Debug)]
pub struct OrdMap<K, V> {
    items: Vec<(K, V)>,
}

impl<K: PartialEq, V> Default for OrdMap<K, V> {
    fn default() -> Self {
        OrdMap { items: Vec::new() }
    }
}

impl<K: PartialEq, V> OrdMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.items
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// The value of `key`, inserted as `make()` first if it is not there.
    pub fn entry_or_insert_with(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        let at = match self.items.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                self.items.push((key, make()));
                self.items.len() - 1
            }
        };
        &mut self.items[at].1
    }

    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.items.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.items.iter().map(|(_, v)| v)
    }
}

/// The value that comes up most often in `values`, the first to reach the
/// highest count winning a tie (the DAW's `mode`), or `None` for none.
pub fn mode<T: PartialEq + Clone>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut counts: Vec<(T, usize)> = Vec::new();
    let mut best: Option<T> = None;
    let mut count = 0;
    for v in values {
        let c = match counts.iter_mut().find(|(k, _)| *k == v) {
            Some(entry) => {
                entry.1 += 1;
                entry.1
            }
            None => {
                counts.push((v.clone(), 1));
                1
            }
        };
        if c > count {
            best = Some(v);
            count = c;
        }
    }
    best
}

/// The median of `values` as the DAW takes it: sorted, the element at `len >> 1`.
pub fn median_i(values: &[i64]) -> i64 {
    let mut s = values.to_vec();
    s.sort_unstable();
    s[s.len() >> 1]
}
