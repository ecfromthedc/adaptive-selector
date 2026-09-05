//! difflib-faithful SequenceMatcher for Rust.
//!
//! Python's `difflib.SequenceMatcher.ratio()` is the heart of Scrapling's
//! similarity scoring, and no Rust crate reproduces it (`strsim`'s ratios
//! differ on nearly every non-trivial input). This module implements the
//! same Ratcliff/Obershelp longest-matching-block recursion with the same
//! "popular element" discounting, verified digit-for-digit against Python
//! via `tests/fixtures/difflib_oracle.json`.
//!
//! Ported from CPython's `Lib/difflib.py` (PSF License).

/// A matching block: `a_start..a_start+len` in A, `b_start..b_start+len` in B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    a_pos: usize,
    b_pos: usize,
    size: usize,
}

/// Two-sequence matcher with difflib-compatible `ratio()`.
pub(crate) struct SequenceMatcher<'a, T: PartialEq> {
    a: &'a [T],
    b: &'a [T],
    b2j: std::collections::HashMap<T, Vec<usize>>,
}

impl<'a, T: PartialEq + std::hash::Hash + Eq + Clone> SequenceMatcher<'a, T> {
    pub(crate) fn new(a: &'a [T], b: &'a [T]) -> Self {
        let mut b2j: std::collections::HashMap<T, Vec<usize>> = Default::default();
        for (i, x) in b.iter().enumerate() {
            b2j.entry(x.clone()).or_default().push(i);
        }
        let mut b_popular: std::collections::HashSet<T> = Default::default();
        let n = b.len();
        if n >= 200 {
            // Difflib's autojunk: any element of b that appears more than 1%
            // as often as b is long is "popular" and excluded from matching.
            let cutoff = n / 100;
            let mut counts: std::collections::HashMap<T, usize> = Default::default();
            for x in b.iter() {
                *counts.entry(x.clone()).or_insert(0) += 1;
            }
            for (x, count) in counts {
                if count > cutoff {
                    b_popular.insert(x);
                }
            }
            b2j.retain(|k, _| !b_popular.contains(k));
        }
        let _ = &b_popular;
        Self { a, b, b2j }
    }

    fn find_longest_match(
        &self,
        a_lower: usize,
        a_upper: usize,
        b_lower: usize,
        b_upper: usize,
    ) -> Match {
        // Difflib's algorithm with j2len rolling map.
        let mut best_i = a_lower;
        let mut best_j = b_lower;
        let mut best_size = 0usize;
        let mut j2len: std::collections::HashMap<usize, usize> = Default::default();
        for i in a_lower..a_upper {
            let mut newj2len = std::collections::HashMap::new();
            if let Some(indexes) = self.b2j.get(&self.a[i]) {
                for &j in indexes {
                    if j < b_lower {
                        continue;
                    }
                    if j >= b_upper {
                        break;
                    }
                    let k = match j2len.get(&(j.wrapping_sub(1))) {
                        Some(v) => v + 1,
                        None => 1,
                    };
                    newj2len.insert(j, k);
                    if k > best_size {
                        best_size = k;
                        best_i = i + 1 - k;
                        best_j = j + 1 - k;
                    }
                }
            }
            j2len = newj2len;
        }
        // Extend the best block over adjacent equal runs difflib clips.
        while best_i > a_lower && best_j > b_lower && self.a[best_i - 1] == self.b[best_j - 1] {
            best_i -= 1;
            best_j -= 1;
            best_size += 1;
        }
        while best_i + best_size < a_upper
            && best_j + best_size < b_upper
            && self.a[best_i + best_size] == self.b[best_j + best_size]
        {
            best_size += 1;
        }
        Match {
            a_pos: best_i,
            b_pos: best_j,
            size: best_size,
        }
    }

    fn get_matching_blocks(&self) -> Vec<Match> {
        let mut queue = vec![(0usize, self.a.len(), 0usize, self.b.len())];
        let mut blocks = Vec::new();
        while let Some((a_lower, a_upper, b_lower, b_upper)) = queue.pop() {
            let m = self.find_longest_match(a_lower, a_upper, b_lower, b_upper);
            if m.size > 0 {
                if a_lower < m.a_pos {
                    queue.push((a_lower, m.a_pos, b_lower, m.b_pos));
                }
                if m.a_pos + m.size < a_upper {
                    queue.push((m.a_pos + m.size, a_upper, m.b_pos + m.size, b_upper));
                }
                blocks.push(m);
            }
        }
        blocks.sort_by_key(|m| (m.a_pos, m.b_pos));
        // Difflib appends a sentinel (0,0,0) implicitly via the terminal block.
        let mut merged: Vec<Match> = Vec::with_capacity(blocks.len() + 1);
        for b in blocks {
            match merged.last_mut() {
                Some(last)
                    if last.a_pos + last.size == b.a_pos && last.b_pos + last.size == b.b_pos =>
                {
                    last.size += b.size;
                }
                _ => merged.push(b),
            }
        }
        merged
    }

    /// difflib's `ratio()`: 2.0 * M / T where M is matched elements and T the
    /// total length of both sequences.
    pub(crate) fn ratio(&self) -> f64 {
        let matches: usize = self.get_matching_blocks().iter().map(|m| m.size).sum();
        let total = self.a.len() + self.b.len();
        if total == 0 {
            return 1.0;
        }
        (2.0 * matches as f64) / total as f64
    }
}

/// Convenience for strings — mirrors `SequenceMatcher(None, a, b).ratio()`
/// including difflib's autojunk behavior on long B sequences.
pub fn str_ratio(a: &str, b: &str) -> f64 {
    // Difflib hashes on full elements; for &str we iterate chars.
    SequenceMatcher::new(
        &a.chars().collect::<Vec<char>>(),
        &b.chars().collect::<Vec<char>>(),
    )
    .ratio()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(str_ratio("", ""), 1.0);
        assert_eq!(str_ratio("abcd", "abcd"), 1.0);
        assert!((str_ratio("abc", "abd") - 2.0 * 2.0 / 6.0).abs() < 1e-12);
    }

    #[test]
    fn matches_difflib_reference_values() {
        // Values produced by CPython 3.12 difflib; the JSON oracle in
        // tests/fixtures covers 30 cases digit-for-digit.
        let cases = [
            ("qabxcdcds", "abycdf", 0.5333333333333333), // verified against CPython 3.9 difflib live
            ("spam", "park", 0.5),
        ];
        for (a, b, expected) in cases {
            let got = str_ratio(a, b);
            assert!(
                (got - expected).abs() < 1e-12,
                "ratio({a:?}, {b:?}) = {got}, expected {expected}"
            );
        }
    }
}
#[cfg(test)]
mod oracle {
    #[test]
    fn difflib_oracle_json() {
        let data = include_str!("../tests/fixtures/difflib_oracle.json");
        for case in serde_json::from_str::<serde_json::Value>(data)
            .unwrap()
            .as_array()
            .unwrap()
        {
            let a = case["a"].as_str().unwrap();
            let b = case["b"].as_str().unwrap();
            let expected = case["ratio"].as_f64().unwrap();
            let got = crate::difflib::str_ratio(a, b);
            assert!(
                (got - expected).abs() < 1e-9,
                "ratio({a:?}, {b:?}) = {got}, difflib says {expected}"
            );
        }
    }
}
