//! Fuzzy matching for the project switcher (`Ctrl+P`) and action palette.

/// Score of `query` against `candidate` (higher is better), or `None` when
/// the query's characters do not all appear in order (case-insensitive).
///
/// Bonuses: consecutive characters, word starts (after `/`, `-`, `_`, space,
/// or a lower→upper case change), and a match at the very start.
#[must_use]
fn score(query: &str, candidate: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let mut total = 0u32;
    let mut pos = 0usize;
    let mut previous: Option<usize> = None;
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let q = q.to_lowercase().next().unwrap_or(q);
        let found = (pos..cand.len()).find(|&i| cand[i].to_lowercase().next() == Some(q))?;
        let mut s = 1;
        // Contiguity outweighs word starts, so exact substrings rank first.
        if previous == Some(found.wrapping_sub(1)) {
            s += 8;
        }
        if found == 0 {
            s += 8;
        } else {
            let before = cand[found - 1];
            if matches!(before, '/' | '\\' | '-' | '_' | ' ' | '.')
                || (before.is_lowercase() && cand[found].is_uppercase())
            {
                s += 6;
            }
        }
        total += s;
        previous = Some(found);
        pos = found + 1;
    }
    // Shorter candidates win ties.
    Some(total * 100 / (100 + u32::try_from(cand.len()).unwrap_or(u32::MAX).min(1000) / 10))
}

/// Ranks `items` by score against `query`, best first; non-matches are dropped.
/// Ties keep the input order (e.g. most recent first).
pub fn rank<'a, T>(query: &str, items: &'a [T], text: impl Fn(&T) -> &str) -> Vec<&'a T> {
    let mut scored: Vec<(u32, usize, &T)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| score(query, text(item)).map(|s| (s, i, item)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_in_order_case_insensitively() {
        assert!(score("abc", "a-b-c").is_some());
        assert!(score("ABC", "abc").is_some());
        assert!(score("acb", "abc").is_none());
    }

    #[test]
    fn prefers_word_starts_and_contiguous() {
        let items = ["my-product-backend", "microservices", "mp"];
        let ranked = rank("mp", &items, |s| s);
        assert_eq!(*ranked[0], "mp");
        assert_eq!(*ranked[1], "my-product-backend");
        let ranked = rank("serv", &["observer", "services"], |s| s);
        assert_eq!(*ranked[0], "services");
    }

    #[test]
    fn empty_query_keeps_order() {
        let items = ["b", "a"];
        assert_eq!(rank("", &items, |s| s), [&"b", &"a"]);
    }
}
