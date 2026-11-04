//! Background search over the history: hash prefix, message summary, author.

use std::sync::Arc;

use archipel_model::ports::HistoryReader;
use archipel_model::{CancelToken, Oid, RepoPath};

use crate::error::GraphError;
use crate::topology::TopologyFile;

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// Text matched case-insensitively against the commit summary, author
    /// name and email, and as a prefix of the full hexadecimal id.
    pub text: String,
    /// Stop after this many hits.
    pub max_hits: usize,
}

impl SearchQuery {
    /// Default maximum number of hits.
    pub const DEFAULT_MAX_HITS: usize = 1000;

    /// A query for `text` with the default hit limit.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            max_hits: Self::DEFAULT_MAX_HITS,
        }
    }
}

/// Rows matching a query, in display order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResult {
    /// Matching rows.
    pub rows: Vec<u32>,
    /// Whether the search stopped at `max_hits`.
    pub truncated: bool,
}

/// A detached, thread-safe search handle over one snapshot of the history
/// (from [`HistoryView::searcher`](crate::HistoryView::searcher)).
///
/// Searches run in chunks of rows: ids are read from the cache file, metadata
/// is loaded per chunk from the backend (not kept: it would evict the visible
/// window from the LRU), and the cancel token is checked between chunks.
#[derive(Clone)]
pub struct Searcher {
    history: Arc<dyn HistoryReader>,
    repo: RepoPath,
    file: Arc<TopologyFile>,
    chunk_rows: u32,
}

impl Searcher {
    pub(crate) fn new(
        history: Arc<dyn HistoryReader>,
        repo: RepoPath,
        file: Arc<TopologyFile>,
        chunk_rows: usize,
    ) -> Self {
        Self {
            history,
            repo,
            file,
            chunk_rows: u32::try_from(chunk_rows.max(1)).unwrap_or(u32::MAX),
        }
    }

    /// Number of rows of the snapshot searched.
    #[must_use]
    pub fn row_count(&self) -> u32 {
        self.file.rows()
    }

    /// Runs `query` over the whole history. An empty query matches nothing.
    pub fn search(
        &self,
        query: &SearchQuery,
        cancel: &CancelToken,
    ) -> Result<SearchResult, GraphError> {
        let needle = query.text.trim().to_lowercase();
        let mut result = SearchResult::default();
        if needle.is_empty() || query.max_hits == 0 {
            return Ok(result);
        }
        let hex_prefix = is_hex(&needle).then_some(needle.as_str());
        let rows = self.file.rows();
        let mut start = 0u32;
        while start < rows {
            cancel.check()?;
            let end = start.saturating_add(self.chunk_rows).min(rows);
            let oids = self.file.read_oids(start..end)?;
            let metas = self.history.commit_meta(&self.repo, &oids)?;
            let mut metas = metas.iter().peekable();
            for (row, oid) in (start..).zip(&oids) {
                // Backends return metadata in request order; a fake may skip ids.
                let meta = metas.next_if(|m| m.oid == *oid);
                let by_hash = hex_prefix.is_some_and(|p| oid.to_hex().starts_with(p));
                let by_meta = meta.is_some_and(|m| {
                    contains_ci(&m.summary, &needle)
                        || contains_ci(&m.author_name, &needle)
                        || contains_ci(&m.author_email, &needle)
                });
                if by_hash || by_meta {
                    result.rows.push(row);
                    if result.rows.len() >= query.max_hits {
                        result.truncated = true;
                        return Ok(result);
                    }
                }
            }
            start = end;
        }
        Ok(result)
    }

    /// Row of `oid` in the snapshot, by scanning the ids chunk by chunk.
    pub fn row_of(&self, oid: Oid, cancel: &CancelToken) -> Result<Option<u32>, GraphError> {
        let rows = self.file.rows();
        let chunk = self.chunk_rows.saturating_mul(OID_SCAN_FACTOR);
        let mut start = 0u32;
        while start < rows {
            cancel.check()?;
            let end = start.saturating_add(chunk).min(rows);
            let oids = self.file.read_oids(start..end)?;
            if let Some(i) = oids.iter().position(|o| *o == oid) {
                return Ok(Some(start + u32::try_from(i).unwrap_or(u32::MAX)));
            }
            start = end;
        }
        Ok(None)
    }
}

/// Id-only scans read this many times more rows per chunk than metadata searches.
const OID_SCAN_FACTOR: u32 = 16;

fn is_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle_lower.as_bytes());
        return n.len() <= h.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_contains() {
        assert!(contains_ci("Fix the Bug", "the bug"));
        assert!(!contains_ci("Fix", "fixes"));
        assert!(contains_ci("Écrire", "écr"));
        assert!(is_hex("abc123") && !is_hex("xyz"));
    }
}
