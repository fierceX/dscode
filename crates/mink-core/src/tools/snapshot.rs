//! Session-local hashline snapshots.
//!
//! Behavior is ported from oh-my-pi at commit
//! `c53b85aaf4f584c86fd17399af6ff0274d798496` (MIT, Can Bölük and
//! contributors). This Rust module keeps Mink's runtime and filesystem
//! boundaries while preserving the upstream content-tag/history contracts.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

const MAX_PATHS: usize = 30;
const MAX_VERSIONS_PER_PATH: usize = 4;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub tag: String,
    pub path: PathBuf,
    /// UTF-8 BOM-free, LF-normalized complete file text.
    pub text: String,
    pub seen_lines: BTreeSet<usize>,
    sequence: u64,
}

#[derive(Debug, Clone, Default)]
struct NoopState {
    payload: String,
    count: u8,
}

#[derive(Debug, Default)]
pub struct FileSnapshotStore {
    by_path: HashMap<PathBuf, VecDeque<FileSnapshot>>,
    path_recency: VecDeque<PathBuf>,
    total_bytes: usize,
    sequence: u64,
    named_clipboard: BTreeMap<String, Vec<String>>,
    noop_by_path: HashMap<PathBuf, NoopState>,
    edit_result_tags: HashMap<PathBuf, BTreeSet<String>>,
}

impl FileSnapshotStore {
    pub fn record<I>(&mut self, path: &Path, content: &str, visible_lines: I) -> FileSnapshot
    where
        I: IntoIterator<Item = usize>,
    {
        let path = canonical_snapshot_path(path);
        let text = normalize_snapshot_text(content);
        let tag = compute_file_tag(&text);
        let visible = visible_lines
            .into_iter()
            .filter(|line| *line > 0)
            .collect::<BTreeSet<_>>();
        self.sequence = self.sequence.wrapping_add(1);

        let versions = self.by_path.entry(path.clone()).or_default();
        if let Some(index) = versions.iter().position(|version| version.text == text) {
            let mut existing = versions
                .remove(index)
                .expect("snapshot index came from this history");
            existing.seen_lines.extend(visible);
            existing.sequence = self.sequence;
            let snapshot = existing.clone();
            versions.push_front(existing);
            self.touch_path(&path);
            return snapshot;
        }

        let snapshot = FileSnapshot {
            tag,
            path: path.clone(),
            text,
            seen_lines: visible,
            sequence: self.sequence,
        };
        self.total_bytes = self.total_bytes.saturating_add(snapshot.text.len());
        versions.push_front(snapshot.clone());
        while versions.len() > MAX_VERSIONS_PER_PATH {
            if let Some(removed) = versions.pop_back() {
                self.total_bytes = self.total_bytes.saturating_sub(removed.text.len());
            }
        }
        self.touch_path(&path);
        self.evict();
        snapshot
    }

    /// Record content whose tag was returned by a successful Edit result.
    ///
    /// This provenance is deliberately narrower than ordinary snapshots from
    /// Read/Grep/Write: stale recovery may recommend direct tag reuse only for
    /// a header that the model actually received from an earlier Edit.
    pub fn record_edit<I>(&mut self, path: &Path, content: &str, visible_lines: I) -> FileSnapshot
    where
        I: IntoIterator<Item = usize>,
    {
        let path = canonical_snapshot_path(path);
        let snapshot = self.record(&path, content, visible_lines);
        let retained = self
            .by_path
            .get(&path)
            .into_iter()
            .flatten()
            .map(|version| version.tag.clone())
            .collect::<BTreeSet<_>>();
        let tags = self.edit_result_tags.entry(path).or_default();
        tags.retain(|tag| retained.contains(tag));
        tags.insert(snapshot.tag.clone());
        snapshot
    }

    pub fn is_edit_result_tag(&self, path: &Path, tag: &str) -> bool {
        self.edit_result_tags
            .get(&canonical_snapshot_path(path))
            .is_some_and(|tags| tags.iter().any(|item| item.eq_ignore_ascii_case(tag)))
    }

    /// Tag of the most recent snapshot for a path, when one exists.
    pub fn latest_tag(&self, path: &Path) -> Option<String> {
        let path = canonical_snapshot_path(path);
        self.by_path
            .get(&path)?
            .front()
            .map(|snapshot| snapshot.tag.clone())
    }

    pub fn versions(&self, path: &Path, tag: &str) -> Vec<FileSnapshot> {
        let path = canonical_snapshot_path(path);
        self.by_path
            .get(&path)
            .into_iter()
            .flatten()
            .filter(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
            .cloned()
            .collect()
    }

    pub fn unique_path_for_tag_and_name(
        &self,
        tag: &str,
        file_name: &std::ffi::OsStr,
        excluded: &Path,
    ) -> Result<PathBuf, String> {
        let excluded = canonical_snapshot_path(excluded);
        let mut paths = self
            .by_path
            .iter()
            .filter(|(path, versions)| {
                **path != excluded
                    && path.file_name() == Some(file_name)
                    && versions
                        .iter()
                        .any(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
            })
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        match paths.as_slice() {
            [path] => Ok(path.clone()),
            [] => Err(format!(
                "no retained snapshot has both filename {:?} and tag #{tag}",
                file_name
            )),
            _ => Err(format!(
                "snapshot tag #{tag} is ambiguous across {} session paths: {}",
                paths.len(),
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    pub fn add_seen_lines<I>(
        &mut self,
        path: &Path,
        tag: &str,
        snapshot_text: &str,
        lines: I,
    ) -> bool
    where
        I: IntoIterator<Item = usize>,
    {
        let path = canonical_snapshot_path(path);
        let Some(versions) = self.by_path.get_mut(&path) else {
            return false;
        };
        let lines = lines
            .into_iter()
            .filter(|line| *line > 0)
            .collect::<Vec<_>>();
        let mut found = false;
        for snapshot in versions.iter_mut().filter(|snapshot| {
            snapshot.tag.eq_ignore_ascii_case(tag) && snapshot.text == snapshot_text
        }) {
            snapshot.seen_lines.extend(lines.iter().copied());
            found = true;
        }
        found
    }

    pub fn relocate(&mut self, source: &Path, destination: &Path) {
        let source = canonical_snapshot_path(source);
        let destination = canonical_snapshot_path(destination);
        if source == destination {
            return;
        }
        let Some(mut source_versions) = self.by_path.remove(&source) else {
            return;
        };
        for snapshot in &mut source_versions {
            snapshot.path = destination.clone();
        }
        let destination_versions = self.by_path.entry(destination.clone()).or_default();
        let mut merged = VecDeque::new();
        for version in source_versions
            .into_iter()
            .chain(destination_versions.drain(..))
        {
            if !merged
                .iter()
                .any(|existing: &FileSnapshot| existing.text == version.text)
            {
                merged.push_back(version);
            }
        }
        *destination_versions = merged;
        while destination_versions.len() > MAX_VERSIONS_PER_PATH {
            if let Some(removed) = destination_versions.pop_back() {
                self.total_bytes = self.total_bytes.saturating_sub(removed.text.len());
            }
        }
        self.path_recency
            .retain(|path| path != &source && path != &destination);
        self.path_recency.push_back(destination.clone());
        self.noop_by_path.remove(&source);
        if let Some(source_tags) = self.edit_result_tags.remove(&source) {
            self.edit_result_tags
                .entry(destination)
                .or_default()
                .extend(source_tags);
        }
        self.total_bytes = self
            .by_path
            .values()
            .flatten()
            .map(|snapshot| snapshot.text.len())
            .sum();
    }

    pub fn named_clipboard(&self) -> &BTreeMap<String, Vec<String>> {
        &self.named_clipboard
    }

    pub fn set_named_clipboard(&mut self, registers: BTreeMap<String, Vec<String>>) {
        self.named_clipboard = registers;
    }

    pub fn begin_noop_attempt(&mut self, path: &Path, payload: &str) {
        let path = canonical_snapshot_path(path);
        if self
            .noop_by_path
            .get(&path)
            .is_some_and(|state| state.payload != payload)
        {
            self.noop_by_path.remove(&path);
        }
    }

    pub fn note_noop(&mut self, path: &Path, payload: &str) -> u8 {
        let path = canonical_snapshot_path(path);
        let state = self.noop_by_path.entry(path).or_default();
        if state.payload == payload {
            state.count = state.count.saturating_add(1);
        } else {
            state.payload = payload.to_string();
            state.count = 1;
        }
        state.count
    }

    pub fn reset_noop(&mut self, path: &Path) {
        self.noop_by_path.remove(&canonical_snapshot_path(path));
    }

    fn touch_path(&mut self, path: &Path) {
        self.path_recency.retain(|candidate| candidate != path);
        self.path_recency.push_back(path.to_path_buf());
    }

    fn evict(&mut self) {
        while self.by_path.len() > MAX_PATHS {
            let Some(path) = self.path_recency.pop_front() else {
                break;
            };
            if let Some(versions) = self.by_path.remove(&path) {
                self.total_bytes = self
                    .total_bytes
                    .saturating_sub(versions.iter().map(|item| item.text.len()).sum());
            }
            self.noop_by_path.remove(&path);
            self.edit_result_tags.remove(&path);
        }

        while self.total_bytes > MAX_SNAPSHOT_BYTES {
            let Some(path) = self.path_recency.pop_front() else {
                break;
            };
            if let Some(versions) = self.by_path.remove(&path) {
                self.total_bytes = self
                    .total_bytes
                    .saturating_sub(versions.iter().map(|item| item.text.len()).sum());
            }
            self.noop_by_path.remove(&path);
            self.edit_result_tags.remove(&path);
        }
    }
}

pub fn canonical_snapshot_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn normalize_snapshot_text(content: &str) -> String {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    content.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn compute_file_tag(content: &str) -> String {
    let normalized = normalize_snapshot_text(content)
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t', '\r']))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{:04X}",
        xxhash_rust::xxh32::xxh32(normalized.as_bytes(), 0) & 0xffff
    )
}

pub fn split_content_lines(content: &str) -> Vec<String> {
    let normalized = normalize_snapshot_text(content);
    let mut lines = normalized
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if normalized.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_reuses_version_and_merges_seen_lines() {
        let mut store = FileSnapshotStore::default();
        let path = PathBuf::from("missing-a.rs");
        let first = store.record(&path, "a\nb\n", [1]);
        let second = store.record(&path, "a\nb\n", [2]);
        assert_eq!(first.tag, second.tag);
        let stored = store.versions(&path, &first.tag);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].seen_lines, BTreeSet::from([1, 2]));
    }

    #[test]
    fn tag_ignores_crlf_and_trailing_horizontal_space() {
        assert_eq!(compute_file_tag("a  \r\nb\r\n"), compute_file_tag("a\nb\n"));
    }

    #[test]
    fn named_clipboard_survives_independent_snapshot_records() {
        let mut store = FileSnapshotStore::default();
        store.set_named_clipboard(BTreeMap::from([("saved".into(), vec!["x".into()])]));
        store.record(Path::new("a"), "a", [1]);
        assert_eq!(
            store.named_clipboard().get("saved"),
            Some(&vec!["x".to_string()])
        );
    }
}
