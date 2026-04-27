//! Tiny fuzzy file picker. Walks a root directory, collects relative paths
//! (skipping `.git`, `target`, `node_modules`), and exposes a character-skip
//! scorer for `@query` autocomplete.

use std::path::{Path, PathBuf};

/// In-memory list of candidate relative paths under `root`.
#[derive(Debug, Clone)]
pub struct FuzzyIndex {
    root: PathBuf,
    paths: Vec<PathBuf>,
}

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".next", "dist", ".cache"];

impl FuzzyIndex {
    /// Build the index by walking `root` recursively.
    pub fn index(root: &Path) -> Self {
        let mut paths = Vec::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.file_name()
                    .to_str()
                    .is_none_or(|n| !SKIP_DIRS.contains(&n))
            })
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    paths.push(rel.to_path_buf());
                }
            }
        }
        paths.sort();
        Self {
            root: root.to_path_buf(),
            paths,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Score every path against `query` and return the top `limit` matches in
    /// descending score order. Empty `query` returns the first `limit` paths.
    #[allow(clippy::should_implement_trait)]
    pub fn r#match(&self, query: &str, limit: usize) -> Vec<(&Path, i32)> {
        let lower_query: String = query.to_lowercase();
        let mut scored: Vec<(&Path, i32)> = self
            .paths
            .iter()
            .filter_map(|p| {
                let s = p.to_string_lossy();
                let score = score(&s, &lower_query)?;
                Some((p.as_path(), score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        scored.truncate(limit);
        scored
    }
}

/// Character-skip fuzzy scorer.
/// - Returns `None` when characters of `query` cannot be matched in order.
/// - `Some(0)` for empty queries.
/// - Scoring: +10 for consecutive char hits, +20 if matched character starts a
///   word boundary (`/` or `_` or beginning of string), +50 boost when the
///   path's basename equals `query` exactly, +30 when the basename starts with
///   `query`.
pub fn score(haystack: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let hay_lower = haystack.to_lowercase();
    let q_lower = query.to_lowercase();

    // Walk the haystack and consume query chars greedily in order.
    let mut q_chars = q_lower.chars().peekable();
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut prev_char_break = true;

    for (i, c) in hay_lower.char_indices() {
        let want = match q_chars.peek() {
            Some(w) => *w,
            None => break,
        };
        if c == want {
            q_chars.next();
            score += 1;
            if last_match.is_some_and(|l| l + 1 == i) {
                score += 10;
            }
            if prev_char_break {
                score += 20;
            }
            last_match = Some(i);
        }
        prev_char_break = matches!(c, '/' | '_' | '-' | '.' | ' ');
    }

    if q_chars.peek().is_some() {
        return None;
    }

    // Basename boosts.
    let base = std::path::Path::new(haystack)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(haystack)
        .to_lowercase();
    if base == q_lower {
        score += 200;
    } else if base.starts_with(&q_lower) {
        score += 100;
    }

    // Penalise longer haystacks slightly so shorter paths sort first when ties.
    score -= (haystack.len() / 32) as i32;

    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(root: &Path, rel: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "x").unwrap();
    }

    #[test]
    fn fuzzy_match_orders_by_score() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/main.rs");
        write_file(dir.path(), "src/manifest.rs");
        write_file(dir.path(), "README.md");
        let idx = FuzzyIndex::index(dir.path());
        let hits = idx.r#match("main", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0.to_string_lossy(), "src/main.rs");
    }

    #[test]
    fn exact_basename_outranks_partial() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/main.rs");
        write_file(dir.path(), "src/main_alt.rs");
        let idx = FuzzyIndex::index(dir.path());
        let hits = idx.r#match("main.rs", 5);
        assert_eq!(hits[0].0.to_string_lossy(), "src/main.rs");
    }

    #[test]
    fn git_directory_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), ".git/HEAD");
        write_file(dir.path(), ".git/config");
        write_file(dir.path(), "src/main.rs");
        let idx = FuzzyIndex::index(dir.path());
        for p in idx.paths() {
            assert!(
                !p.starts_with(".git"),
                "found .git path: {}",
                p.to_string_lossy()
            );
        }
    }

    #[test]
    fn target_and_node_modules_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "target/debug/foo");
        write_file(dir.path(), "node_modules/foo/index.js");
        write_file(dir.path(), "src/main.rs");
        let idx = FuzzyIndex::index(dir.path());
        for p in idx.paths() {
            let s = p.to_string_lossy();
            assert!(!s.starts_with("target"));
            assert!(!s.starts_with("node_modules"));
        }
    }

    #[test]
    fn score_unmatched_query_is_none() {
        assert!(score("foo.rs", "xyz").is_none());
    }

    #[test]
    fn score_empty_query_is_zero() {
        assert_eq!(score("anything", ""), Some(0));
    }
}
