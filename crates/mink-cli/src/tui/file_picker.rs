use crate::tui::sanitize::normalize_tui_input;
use std::path::{Component, Path, PathBuf};

const MAX_CANDIDATES: usize = 20_000;
const MAX_RESULTS: usize = 200;
const DEFAULT_MAX_PARENT_DEPTH: usize = 3;
const DIRECT_CHILD_SCAN_DEPTH: usize = 1;

#[derive(Clone, Debug)]
pub(crate) struct FilePickerPolicy {
    cwd: PathBuf,
    allowed_roots: Option<Vec<PathBuf>>,
    max_parent_depth: usize,
}

impl FilePickerPolicy {
    pub(crate) fn from_sandbox(cwd: PathBuf, sandbox: &crate::config::SandboxConfig) -> Self {
        let cwd = canonical_or_lexical(cwd);
        let allowed_roots = if sandbox.is_active() && !cfg!(target_os = "macos") {
            let mut roots = Vec::new();
            for dir in sandbox.read_dirs.iter().chain(sandbox.write_dirs.iter()) {
                roots.push(resolve_policy_dir(&cwd, dir));
            }
            if roots.is_empty() {
                roots.push(cwd.clone());
            }
            Some(roots)
        } else {
            None
        };
        Self {
            cwd,
            allowed_roots,
            max_parent_depth: DEFAULT_MAX_PARENT_DEPTH,
        }
    }

    fn default_for_cwd() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            cwd: canonical_or_lexical(cwd),
            allowed_roots: None,
            max_parent_depth: DEFAULT_MAX_PARENT_DEPTH,
        }
    }

    fn allows_path(&self, path: &Path) -> bool {
        let path = canonical_or_lexical(path.to_path_buf());
        match &self.allowed_roots {
            Some(allowed_roots) => allowed_roots
                .iter()
                .any(|allowed| path.starts_with(allowed) || allowed.starts_with(&path)),
            None => true,
        }
    }

    fn allowed_roots_below(&self, root: &Path) -> Vec<PathBuf> {
        let Some(allowed_roots) = &self.allowed_roots else {
            return Vec::new();
        };
        let root = canonical_or_lexical(root.to_path_buf());
        let mut roots: Vec<PathBuf> = allowed_roots
            .iter()
            .filter(|allowed| allowed.starts_with(&root) && *allowed != &root)
            .cloned()
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    #[cfg(test)]
    pub(crate) fn restricted_for_tests(
        cwd: PathBuf,
        allowed_roots: Vec<PathBuf>,
        max_parent_depth: usize,
    ) -> Self {
        Self {
            cwd: canonical_or_lexical(cwd),
            allowed_roots: Some(
                allowed_roots
                    .into_iter()
                    .map(canonical_or_lexical)
                    .collect(),
            ),
            max_parent_depth,
        }
    }
}

impl Default for FilePickerPolicy {
    fn default() -> Self {
        Self::default_for_cwd()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FilePickerState {
    pub replace_start: usize,
    pub replace_end: usize,
    pub query: String,
    pub candidates: Vec<FilePickCandidate>,
    pub items: Vec<FilePickItem>,
    pub selected: usize,
    pub scroll: usize,
    scan_key: Option<ScanKey>,
    scan_cache: Vec<ScanCacheEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilePickCandidate {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilePickItem {
    pub path: String,
    pub is_dir: bool,
    pub score: i64,
}

impl FilePickerState {
    pub(crate) fn open(input: &str, cursor: usize, policy: &FilePickerPolicy) -> Self {
        let (_, _, query) = path_query_at_cursor(input, cursor);
        let (scan_key, candidates) = scan_candidates_for_query(&query, policy);
        Self::open_with_candidates_and_key(input, cursor, candidates, scan_key)
    }

    #[cfg(test)]
    pub(crate) fn open_with_candidates(
        input: &str,
        cursor: usize,
        candidates: Vec<FilePickCandidate>,
    ) -> Self {
        Self::open_with_candidates_and_key(input, cursor, candidates, None)
    }

    fn open_with_candidates_and_key(
        input: &str,
        cursor: usize,
        candidates: Vec<FilePickCandidate>,
        scan_key: Option<ScanKey>,
    ) -> Self {
        let (replace_start, replace_end, query) = path_query_at_cursor(input, cursor);
        let mut state = Self {
            replace_start,
            replace_end,
            query,
            candidates,
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            scan_key,
            scan_cache: Vec::new(),
        };
        if let Some(key) = state.scan_key.clone() {
            state.scan_cache.push(ScanCacheEntry {
                key,
                candidates: state.candidates.clone(),
            });
        }
        state.refresh(input, cursor);
        state
    }

    pub(crate) fn refresh(&mut self, input: &str, cursor: usize) {
        let (replace_start, replace_end, query) = path_query_at_cursor(input, cursor);
        self.replace_start = replace_start;
        self.replace_end = replace_end;
        self.query = query;
        self.items = filter_candidates(&self.candidates, &self.query);
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll = 0;
        } else {
            self.selected = self.selected.min(self.items.len().saturating_sub(1));
            self.scroll = self.scroll.min(self.selected);
        }
    }

    pub(crate) fn refresh_with_policy(
        &mut self,
        input: &str,
        cursor: usize,
        policy: &FilePickerPolicy,
    ) {
        let (_, _, query) = path_query_at_cursor(input, cursor);
        let next_target = scan_key_for_query(&query, policy);
        let next_key = next_target.as_ref().map(|target| target.key.clone());
        if next_key != self.scan_key {
            self.candidates = next_target
                .as_ref()
                .map(|target| self.cached_candidates(target, policy))
                .unwrap_or_default();
            self.scan_key = next_key;
            self.selected = 0;
            self.scroll = 0;
        }
        self.refresh(input, cursor);
    }

    fn cached_candidates(
        &mut self,
        target: &ScanTarget,
        policy: &FilePickerPolicy,
    ) -> Vec<FilePickCandidate> {
        if let Some(entry) = self.scan_cache.iter().find(|entry| entry.key == target.key) {
            return entry.candidates.clone();
        }
        let candidates = scan_candidates_for_target(target, policy);
        self.scan_cache.push(ScanCacheEntry {
            key: target.key.clone(),
            candidates: candidates.clone(),
        });
        if self.scan_cache.len() > 8 {
            self.scan_cache.remove(0);
        }
        candidates
    }

    pub(crate) fn selected_path(&self) -> Option<String> {
        let item = self.items.get(self.selected)?;
        let mut path = item.path.clone();
        if item.is_dir && !path.ends_with('/') {
            path.push('/');
        }
        Some(path)
    }

    pub(crate) fn move_selection(&mut self, delta: isize, visible_rows: usize) {
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        let max = self.items.len().saturating_sub(1);
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(max)
        };
        self.clamp_scroll(visible_rows);
    }

    pub(crate) fn clamp_scroll(&mut self, visible_rows: usize) {
        if visible_rows == 0 || self.items.is_empty() {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected + 1 - visible_rows;
        }
        self.scroll = self
            .scroll
            .min(self.items.len().saturating_sub(visible_rows));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScanKey {
    root: PathBuf,
    display_prefix: String,
    max_depth: usize,
    include_hidden: bool,
}

struct ScanTarget {
    key: ScanKey,
    max_depth: usize,
    include_hidden: bool,
}

fn scan_candidates_for_query(
    query: &str,
    policy: &FilePickerPolicy,
) -> (Option<ScanKey>, Vec<FilePickCandidate>) {
    let Some(target) = scan_key_for_query(query, policy) else {
        return (None, Vec::new());
    };
    let candidates = scan_candidates_for_target(&target, policy);
    (Some(target.key), candidates)
}

fn scan_candidates_for_target(
    target: &ScanTarget,
    policy: &FilePickerPolicy,
) -> Vec<FilePickCandidate> {
    scan_candidates_with_prefix(
        &target.key.root,
        &target.key.display_prefix,
        target.max_depth,
        target.include_hidden,
        policy,
    )
}

fn scan_key_for_query(query: &str, policy: &FilePickerPolicy) -> Option<ScanTarget> {
    let parent_depth = leading_parent_depth(query);
    if parent_depth > policy.max_parent_depth {
        return None;
    }
    let mut root = policy.cwd.clone();
    for _ in 0..parent_depth {
        root.push("..");
    }
    let parent_prefix = "../".repeat(parent_depth);
    let rest = query.strip_prefix(&parent_prefix).unwrap_or(query);
    let (current_prefix, rest) = leading_current_prefix(rest);
    let (dir_part, leaf) = split_query_dir(rest);
    let include_hidden = leaf.starts_with('.');
    for segment in dir_part.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return None;
        }
        root.push(segment);
    }
    let root = canonical_or_lexical(root);
    if !root.is_dir() || !policy.allows_path(&root) {
        return None;
    }
    Some(ScanTarget {
        key: ScanKey {
            root,
            display_prefix: format!("{parent_prefix}{current_prefix}{dir_part}"),
            max_depth: DIRECT_CHILD_SCAN_DEPTH,
            include_hidden,
        },
        max_depth: DIRECT_CHILD_SCAN_DEPTH,
        include_hidden,
    })
}

fn leading_parent_depth(query: &str) -> usize {
    let mut depth = 0usize;
    let mut rest = query;
    while let Some(next) = rest.strip_prefix("../") {
        depth += 1;
        rest = next;
    }
    depth
}

fn leading_current_prefix(query_without_parent_prefix: &str) -> (&str, &str) {
    if let Some(rest) = query_without_parent_prefix.strip_prefix("./") {
        ("./", rest)
    } else {
        ("", query_without_parent_prefix)
    }
}

fn split_query_dir(query_without_parent_prefix: &str) -> (&str, &str) {
    if query_without_parent_prefix.ends_with('/') {
        return (query_without_parent_prefix, "");
    }
    match query_without_parent_prefix.rsplit_once('/') {
        Some((dir, leaf)) => {
            let dir_end = dir.len() + '/'.len_utf8();
            (&query_without_parent_prefix[..dir_end], leaf)
        }
        None => ("", query_without_parent_prefix),
    }
}

fn scan_candidates_with_prefix(
    root: &Path,
    display_prefix: &str,
    max_depth: usize,
    include_hidden: bool,
    policy: &FilePickerPolicy,
) -> Vec<FilePickCandidate> {
    let allowed_below = policy.allowed_roots_below(root);
    if !allowed_below.is_empty() {
        return scan_allowed_root_entrypoints(root, display_prefix, &allowed_below, policy);
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(!include_hidden)
        .max_depth(Some(max_depth))
        .build();
    let mut out = Vec::new();
    for entry in walker {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !(ft.is_file() || ft.is_dir()) {
            continue;
        }
        let path = entry.path();
        if path == root {
            continue;
        }
        if !policy.allows_path(path) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let mut display = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if ft.is_dir() && !display.ends_with('/') {
            display.push('/');
        }
        if !display.is_empty() {
            out.push(FilePickCandidate {
                path: format!("{display_prefix}{display}"),
                is_dir: ft.is_dir(),
            });
        }
    }
    out
}

fn scan_allowed_root_entrypoints(
    root: &Path,
    display_prefix: &str,
    allowed_roots: &[PathBuf],
    policy: &FilePickerPolicy,
) -> Vec<FilePickCandidate> {
    let mut out = Vec::new();
    for allowed in allowed_roots {
        let Ok(rel) = allowed.strip_prefix(root) else {
            continue;
        };
        let Some(first) = rel.components().next() else {
            continue;
        };
        let entry_path = root.join(first.as_os_str());
        if !entry_path.is_dir() || !policy.allows_path(&entry_path) {
            continue;
        }
        let mut display = entry_path
            .strip_prefix(root)
            .unwrap_or(&entry_path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if !display.ends_with('/') {
            display.push('/');
        }
        let path = format!("{display_prefix}{display}");
        if !out.iter().any(|item: &FilePickCandidate| item.path == path) {
            out.push(FilePickCandidate { path, is_dir: true });
        }
    }
    out
}

#[derive(Clone, Debug)]
struct ScanCacheEntry {
    key: ScanKey,
    candidates: Vec<FilePickCandidate>,
}

fn resolve_policy_dir(cwd: &Path, dir: &str) -> PathBuf {
    let path = Path::new(dir);
    if path.is_absolute() {
        canonical_or_lexical(path.to_path_buf())
    } else {
        canonical_or_lexical(cwd.join(path))
    }
}

fn canonical_or_lexical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or_else(|_| normalize_lexically(path))
}

fn normalize_lexically(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn filter_candidates(candidates: &[FilePickCandidate], raw_query: &str) -> Vec<FilePickItem> {
    let query = filter_query_leaf(raw_query);
    let mut items = Vec::new();
    for candidate in candidates {
        let basename = path_basename(&candidate.path);
        if !query.starts_with('.') && basename.starts_with('.') {
            continue;
        }
        let Some(score) = score_path(&candidate.path, &query) else {
            continue;
        };
        items.push(FilePickItem {
            path: candidate.path.clone(),
            is_dir: candidate.is_dir,
            score,
        });
    }
    items.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.path.cmp(&b.path))
    });
    items.truncate(MAX_RESULTS);
    items
}

fn filter_query_leaf(raw_query: &str) -> String {
    let normalized = normalize_tui_input(raw_query);
    let parent_prefix = "../".repeat(leading_parent_depth(&normalized));
    let rest = normalized
        .strip_prefix(&parent_prefix)
        .unwrap_or(normalized.as_str());
    let (_, rest) = leading_current_prefix(rest);
    let (_, leaf) = split_query_dir(rest);
    leaf.to_ascii_lowercase()
}

fn score_path(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let path_l = path.to_ascii_lowercase();
    let basename = path_basename(&path_l);
    let score = if basename == query {
        1200
    } else if basename.starts_with(query) {
        1000
    } else if basename.contains(query) {
        700
    } else {
        fuzzy_score(basename, query)?
    };
    Some(score)
}

fn path_basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(path)
}

fn fuzzy_score(path: &str, query: &str) -> Option<i64> {
    let mut score = 250i64;
    let mut last_idx = 0usize;
    for ch in query.chars() {
        let rel = path[last_idx..].find(ch)?;
        score -= rel as i64;
        last_idx += rel + ch.len_utf8();
    }
    Some(score)
}

fn path_query_at_cursor(input: &str, cursor: usize) -> (usize, usize, String) {
    let cursor = crate::tui::state::clamp_char_boundary(input, cursor);
    let mut start = cursor;
    while start > 0 {
        let prev = prev_char_boundary(input, start);
        let Some(ch) = input.get(prev..).and_then(|s| s.chars().next()) else {
            break;
        };
        if ch.is_whitespace() || matches!(ch, '"' | '\'' | '`') {
            break;
        }
        start = prev;
    }
    let token_end = token_end_from(input, cursor);
    let replace_start = if input[start..cursor].starts_with('@') {
        start + '@'.len_utf8()
    } else {
        start
    };
    let token = input.get(replace_start..token_end).unwrap_or("");
    let selector_pos = selector_start(token).map(|idx| replace_start + idx);
    let replace_end =
        selector_pos.unwrap_or_else(|| completion_replace_end(input, cursor, token_end));
    let query_end = selector_pos.unwrap_or(cursor).min(cursor).min(replace_end);
    (
        replace_start,
        replace_end,
        input
            .get(replace_start..query_end)
            .unwrap_or("")
            .to_string(),
    )
}

fn token_end_from(input: &str, cursor: usize) -> usize {
    let mut end = cursor;
    while end < input.len() {
        let Some(ch) = input.get(end..).and_then(|s| s.chars().next()) else {
            break;
        };
        if ch.is_whitespace() || matches!(ch, '"' | '\'' | '`') {
            break;
        }
        end += ch.len_utf8();
    }
    end
}

fn completion_replace_end(input: &str, cursor: usize, token_end: usize) -> usize {
    let suffix = input.get(cursor..token_end).unwrap_or("");
    if selector_start(suffix) == Some(0) {
        return cursor;
    }
    if let Some(idx) = selector_start(suffix) {
        return cursor + idx;
    }
    token_end
}

fn selector_start(s: &str) -> Option<usize> {
    for (idx, _) in s.match_indices(':') {
        if is_selector_suffix(&s[idx..]) {
            return Some(idx);
        }
    }
    None
}

fn is_selector_suffix(suffix: &str) -> bool {
    let Some(rest) = suffix.strip_prefix(':') else {
        return false;
    };
    if rest == "raw" {
        return true;
    }
    if let Some(selector) = rest.strip_prefix("raw:") {
        return is_line_selector(selector);
    }
    if let Some(selector) = rest.strip_suffix(":raw") {
        return is_line_selector(selector);
    }
    is_line_selector(rest)
}

fn is_line_selector(selector: &str) -> bool {
    if selector.is_empty() {
        return false;
    }
    if let Some((start, count)) = selector.split_once('+') {
        return is_nonzero_digits(start)
            && is_nonzero_digits(count)
            && !count.contains('-')
            && !count.contains('+');
    }
    if let Some((start, end)) = selector.split_once('-') {
        return is_nonzero_digits(start)
            && (end.is_empty() || is_nonzero_digits(end))
            && !end.contains('-')
            && !end.contains('+');
    }
    is_nonzero_digits(selector)
}

fn is_nonzero_digits(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|ch| ch.is_ascii_digit())
        && value.parse::<usize>().is_ok_and(|parsed| parsed > 0)
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut prev = pos.saturating_sub(1);
    while prev > 0 && !s.is_char_boundary(prev) {
        prev -= 1;
    }
    prev
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "mink-file-picker-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn current_dir_prefix_keeps_dot_slash_in_candidates() {
        let root = temp_dir("dot-slash");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

        let (_, candidates) = scan_candidates_for_query("./", &policy);

        assert!(candidates.iter().any(|item| item.path == "./src/"));
        assert!(!candidates.iter().any(|item| item.path == "./src/main.rs"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_dir_prefix_filters_current_level_candidates() {
        let root = temp_dir("dot-slash-filter");
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::write(root.join("src/bin/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

        let (_, candidates) = scan_candidates_for_query("./src", &policy);

        assert!(candidates.iter().any(|item| item.path == "./src/"));
        assert!(!candidates.iter().any(|item| item.path == "./src/lib.rs"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_prefix_lists_only_direct_children() {
        let root = temp_dir("direct-children");
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::write(root.join("src/bin/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

        let (_, candidates) = scan_candidates_for_query("./src/", &policy);

        assert!(candidates.iter().any(|item| item.path == "./src/bin/"));
        assert!(candidates.iter().any(|item| item.path == "./src/lib.rs"));
        assert!(
            !candidates
                .iter()
                .any(|item| item.path == "./src/bin/main.rs")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn filtering_matches_leaf_and_sorts_directories_first() {
        let candidates = vec![
            FilePickCandidate {
                path: "src/test.rs".into(),
                is_dir: false,
            },
            FilePickCandidate {
                path: "src/tools/".into(),
                is_dir: true,
            },
            FilePickCandidate {
                path: "src/tui/".into(),
                is_dir: true,
            },
            FilePickCandidate {
                path: "src/runtime.rs".into(),
                is_dir: false,
            },
        ];

        let items = filter_candidates(&candidates, "src/t");

        let paths: Vec<&str> = items.iter().map(|item| item.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["src/tools/", "src/tui/", "src/test.rs", "src/runtime.rs"]
        );
    }

    #[test]
    fn current_dir_prefix_hides_dot_entries_until_dot_is_typed() {
        let candidates = vec![
            FilePickCandidate {
                path: "./src/".into(),
                is_dir: true,
            },
            FilePickCandidate {
                path: "./.../".into(),
                is_dir: true,
            },
            FilePickCandidate {
                path: "./.github/".into(),
                is_dir: true,
            },
        ];

        let visible = filter_candidates(&candidates, "./");
        let visible_paths: Vec<&str> = visible.iter().map(|item| item.path.as_str()).collect();
        assert_eq!(visible_paths, vec!["./src/"]);

        let hidden = filter_candidates(&candidates, "./.");
        let hidden_paths: Vec<&str> = hidden.iter().map(|item| item.path.as_str()).collect();
        assert!(hidden_paths.contains(&"./.../"));
        assert!(hidden_paths.contains(&"./.github/"));
    }

    #[test]
    fn dot_query_rescans_with_hidden_directories_enabled() {
        let root = temp_dir("hidden-scan");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("...")).unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

        let visible = FilePickerState::open("./", "./".len(), &policy);
        let visible_paths: Vec<&str> = visible
            .items
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        assert_eq!(visible_paths, vec!["./src/"]);

        let hidden = FilePickerState::open("./.", "./.".len(), &policy);
        let hidden_paths: Vec<&str> = hidden.items.iter().map(|item| item.path.as_str()).collect();
        assert!(hidden_paths.contains(&"./.../"));
        assert!(hidden_paths.contains(&"./.git/"));
        let _ = fs::remove_dir_all(root);
    }
}
