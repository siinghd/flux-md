//! A tiny stable merge sort.
//!
//! Replaces the standard library's `slice::sort_by` (driftsort) at the two
//! call sites that need a stable sort (inline delimiter edits, footnote
//! ordering). driftsort is a large, general-purpose algorithm; pulling it into
//! the WASM binary costs several KB. Both call sites sort tiny slices, so a
//! compact O(n log n) merge sort is more than fast enough and shrinks the
//! binary.
//!
//! `stable_order` returns the sorted *permutation* of indices rather than
//! reordering the slice, so it works for non-`Copy` element types (e.g. an
//! edit carrying an owned `String`) without cloning them — the caller iterates
//! `v[i]` in the returned order.

/// Stable sort: returns the indices `0..v.len()` permuted into sorted order.
///
/// `before(a, b)` must be a **total, reflexive "≤"**: it returns `true` when
/// `a` should sort before-or-equal `b`. Returning `true` on ties is what makes
/// the sort stable (equal elements keep their original relative order), so pass
/// `<=` / `>=`, never strict `<` / `>`.
pub(crate) fn stable_order<T>(v: &[T], before: impl Fn(&T, &T) -> bool) -> Vec<usize> {
    let n = v.len();
    let mut idx: Vec<usize> = (0..n).collect();
    if n < 2 {
        return idx;
    }
    let mut buf: Vec<usize> = Vec::with_capacity(n);
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut a, mut b) = (i, mid);
            while a < mid && b < hi {
                // Take from the left run when it is before-or-equal the right
                // run's head — on a tie the left (lower original index) wins,
                // preserving stability.
                if before(&v[idx[a]], &v[idx[b]]) {
                    buf.push(idx[a]);
                    a += 1;
                } else {
                    buf.push(idx[b]);
                    b += 1;
                }
            }
            buf.extend_from_slice(&idx[a..mid]);
            buf.extend_from_slice(&idx[b..hi]);
            i = hi;
        }
        std::mem::swap(&mut idx, &mut buf);
        buf.clear();
        width *= 2;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::stable_order;

    fn sorted_by_key(v: &[(i32, i32)]) -> Vec<(i32, i32)> {
        // ascending by .0, stable on ties (must keep .1 order)
        stable_order(v, |a, b| a.0 <= b.0).iter().map(|&i| v[i]).collect()
    }

    #[test]
    fn empty_and_single() {
        assert_eq!(stable_order::<i32>(&[], |a, b| a <= b), Vec::<usize>::new());
        assert_eq!(stable_order(&[42], |a, b| a <= b), vec![0]);
    }

    #[test]
    fn ascending_matches_std_and_is_stable() {
        // Tie on key 1 (the three (1, k) must keep k order: 0,1,2).
        let v = [(3, 0), (1, 0), (2, 0), (1, 1), (1, 2), (2, 1)];
        assert_eq!(
            sorted_by_key(&v),
            vec![(1, 0), (1, 1), (1, 2), (2, 0), (2, 1), (3, 0)],
        );
        // Cross-check against std's stable sort on random-ish data.
        for n in [2usize, 3, 7, 16, 31, 64, 100] {
            let data: Vec<(i32, i32)> = (0..n as i32).map(|x| ((x * 37) % 11, x)).collect();
            let mut expect = data.clone();
            expect.sort_by(|a, b| a.0.cmp(&b.0)); // std stable
            assert_eq!(sorted_by_key(&data), expect, "n={n}");
        }
    }

    #[test]
    fn descending_is_stable() {
        // Mirrors the inline-edits use: descending by key, ties keep order.
        let v = [(1, 0), (3, 0), (1, 1), (3, 1), (2, 0)];
        let got: Vec<(i32, i32)> = stable_order(&v, |a, b| a.0 >= b.0).iter().map(|&i| v[i]).collect();
        assert_eq!(got, vec![(3, 0), (3, 1), (2, 0), (1, 0), (1, 1)]);
    }
}
