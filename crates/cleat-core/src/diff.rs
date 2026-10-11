//! Line diffs of rendered documents, for comparing two versions of a
//! program's output.

use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line<'a> {
    Same(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// The lines of `a` and `b` with what was removed and added, by longest
/// common subsequence. Documents are small, so the quadratic table is fine.
pub fn lines<'a>(a: &'a str, b: &'a str) -> Vec<Line<'a>> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    // Common prefix and suffix don't need the table.
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (am.len(), bm.len());
    // lcs[i][j]: the LCS length of am[i..] and bm[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if am[i] == bm[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let mut out: Vec<Line> = a[..pre].iter().map(|l| Line::Same(l)).collect();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            out.push(Line::Same(am[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[at(i, j + 1)] >= lcs[at(i + 1, j)]) {
            out.push(Line::Added(bm[j]));
            j += 1;
        } else {
            out.push(Line::Removed(am[i]));
            i += 1;
        }
    }
    out.extend(a[a.len() - suf..].iter().map(|l| Line::Same(l)));
    // Removals read better before the additions that replace them.
    for k in 1..out.len() {
        let mut k = k;
        while k > 0 && matches!((out[k - 1], out[k]), (Line::Added(_), Line::Removed(_))) {
            out.swap(k - 1, k);
            k -= 1;
        }
    }
    out
}

/// A unified diff (`@@ -l,n +l,n @@` hunks with `context` lines around each
/// change), or an empty string when the texts are equal.
pub fn unified(a: &str, b: &str, context: usize) -> String {
    let ls = lines(a, b);
    let changed: Vec<usize> = (0..ls.len()).filter(|&k| !matches!(ls[k], Line::Same(_))).collect();
    let mut out = String::new();
    let mut k = 0;
    while k < changed.len() {
        // Group changes whose context would overlap into one hunk.
        let start = changed[k].saturating_sub(context);
        let mut end = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= end + 2 * context + 1 {
            k += 1;
            end = changed[k];
        }
        let end = (end + context + 1).min(ls.len());
        let (mut a_line, mut b_line) = (1, 1);
        for l in &ls[..start] {
            match l {
                Line::Same(_) => (a_line, b_line) = (a_line + 1, b_line + 1),
                Line::Removed(_) => a_line += 1,
                Line::Added(_) => b_line += 1,
            }
        }
        let hunk = &ls[start..end];
        let a_n = hunk.iter().filter(|l| !matches!(l, Line::Added(_))).count();
        let b_n = hunk.iter().filter(|l| !matches!(l, Line::Removed(_))).count();
        out.push_str(&format!("@@ -{a_line},{a_n} +{b_line},{b_n} @@\n"));
        for l in hunk {
            let (c, t) = match l {
                Line::Same(t) => (' ', t),
                Line::Removed(t) => ('-', t),
                Line::Added(t) => ('+', t),
            };
            out.push(c);
            out.push_str(t);
            out.push('\n');
        }
        k += 1;
    }
    out
}
