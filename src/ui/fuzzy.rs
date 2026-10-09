//! Fuzzy matching with relevance scoring for the command palette.
//!
//! [`match_score`] rates how well a query matches one candidate string, so
//! the palette can rank rather than just filter: exact matches outrank
//! prefix matches, which outrank substring matches, which outrank fuzzy
//! subsequence matches (`o_items` finds `order_items`). It returns `None`
//! when the query does not match at all. Matching is case-insensitive
//! throughout.

/// Score tiers, best first. A fuzzy hit can never reach the substring tier.
const EXACT: i64 = 1_000_000_000;
const PREFIX: i64 = 1_000_000;
const SUBSTRING: i64 = 100_000;
/// A substring starting on a word boundary outranks a mid-word one.
const BOUNDARY_BONUS: i64 = 10_000;

/// Score `query` against `candidate`: a larger value is a better match,
/// `None` is no match at all.
///
/// An empty query matches everything with the same score, so a stable sort
/// keeps the candidates' original order.
pub(crate) fn match_score(query: &str, candidate: &str) -> Option<i64> {
    let query = query.trim();
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_lowercase();
    let candidate = candidate.to_lowercase();
    let (q, c) = (query.as_str(), candidate.as_str());

    if c == q {
        return Some(EXACT);
    }
    if c.starts_with(q) {
        // Shorter prefix matches read as more specific: `orders` before
        // `orders_archive`.
        return Some(PREFIX - c.len() as i64);
    }
    if let Some(pos) = c.find(q) {
        let boundary = if is_word_boundary(c, pos) {
            BOUNDARY_BONUS
        } else {
            0
        };
        // Earlier hits first; a word-boundary start outranks a mid-word one.
        return Some(SUBSTRING + boundary - pos as i64);
    }
    fuzzy_score(q, c).map(|score| score.min(SUBSTRING - 1))
}

/// A byte index starts a word when it opens the string or follows a
/// non-alphanumeric. `find` only returns char boundaries, so slicing is safe.
fn is_word_boundary(text: &str, byte_pos: usize) -> bool {
    byte_pos == 0
        || text[..byte_pos]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_alphanumeric())
}

/// Subsequence matching: every query character appears in the candidate in
/// order. Consecutive runs and word-boundary hits score higher; gaps cost.
fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let query: Vec<char> = query.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    if query.len() > candidate.len() {
        return None;
    }

    let mut score: i64 = 0;
    let mut from = 0;
    // One past the previous hit; `None` before the first one.
    let mut prev_end: Option<usize> = None;
    let mut run: usize = 0;

    for (qi, &qc) in query.iter().enumerate() {
        let mut best: Option<(usize, i64, usize)> = None;
        for (j, &tc) in candidate.iter().enumerate().skip(from) {
            if tc != qc {
                continue;
            }
            let consecutive = prev_end == Some(j);
            let new_run = if consecutive { run + 1 } else { 1 };
            let mut s: i64 = 0;
            if qi == 0 && j == 0 {
                s += 30; // the query opens the candidate
            }
            if j == 0 || !candidate[j - 1].is_alphanumeric() {
                s += 20; // word-boundary hit
            }
            if consecutive {
                s += 10 + new_run as i64 * 5; // longer runs win
            }
            s -= (j - from) as i64 * 3; // gaps cost
            if best.is_none_or(|(_, bs, _)| s > bs) {
                best = Some((j, s, new_run));
            }
        }
        let (j, s, new_run) = best?;
        score += s;
        from = j + 1;
        prev_end = Some(j + 1);
        run = new_run;
    }
    // Shorter candidates win ties.
    Some(score - candidate.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scores of `candidates` for `query`, best first, as the palette
    /// would show them.
    fn ranked<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        let mut scored: Vec<(i64, &str)> = candidates
            .iter()
            .filter_map(|&candidate| match_score(query, candidate).map(|s| (s, candidate)))
            .collect();
        scored.sort_by_key(|item| std::cmp::Reverse(item.0));
        scored.into_iter().map(|(_, candidate)| candidate).collect()
    }

    #[test]
    fn an_exact_match_sorts_first() {
        assert_eq!(
            ranked("order", &["order_items", "order", "my_order"]),
            ["order", "order_items", "my_order"]
        );
    }

    #[test]
    fn a_prefix_match_beats_a_substring_match() {
        assert_eq!(
            ranked("ord", &["my_orders", "orders"]),
            ["orders", "my_orders"]
        );
    }

    #[test]
    fn a_word_boundary_substring_beats_a_mid_word_one() {
        assert_eq!(
            ranked("items", &["myitems", "order_items"]),
            ["order_items", "myitems"]
        );
    }

    #[test]
    fn fuzzy_characters_in_order_match() {
        assert!(match_score("o_items", "order_items").is_some());
        assert!(match_score("o_items", "order").is_none());
        assert!(match_score("o_items", "items").is_none());
    }

    #[test]
    fn consecutive_fuzzy_runs_beat_scattered_ones() {
        assert_eq!(ranked("abc", &["a_b_c", "xabcx"]), ["xabcx", "a_b_c"]);
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(match_score("ORDER", "order"), Some(EXACT));
        assert!(match_score("O_ITEMS", "order_items").is_some());
    }

    #[test]
    fn an_empty_query_matches_everything_equally() {
        assert_eq!(match_score("", "order"), Some(0));
        assert_eq!(match_score("   ", "order"), Some(0));
    }

    #[test]
    fn a_query_longer_than_the_candidate_cannot_match() {
        assert_eq!(match_score("order_items", "order"), None);
    }

    #[test]
    fn tiers_never_overlap() {
        let exact = match_score("order", "order").unwrap();
        let prefix = match_score("order", "order_items").unwrap();
        let substring = match_score("order", "my_order_x").unwrap();
        let fuzzy = match_score("o_x", "order_x").unwrap();
        assert!(exact > prefix && prefix > substring && substring > fuzzy);
    }
}
