use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub tag: String,
    pub path: PathBuf,
    pub file_hash: String,
    pub start_line: usize,
    pub line_hashes: Vec<String>,
}

#[derive(Debug, Default)]
pub struct FileSnapshotStore {
    counter: u64,
    by_key: HashMap<(PathBuf, String), FileSnapshot>,
}

impl FileSnapshotStore {
    pub fn record(&mut self, path: &Path, content: &str, start_line: usize) -> FileSnapshot {
        self.counter += 1;
        let shown_lines = split_content_lines(content);
        let file_hash = hash_text(&shown_lines.join("\n"));
        let tag = format!("{:04X}", (self.counter as u16) ^ short_hash(&file_hash));
        let snapshot = FileSnapshot {
            tag: tag.clone(),
            path: path.to_path_buf(),
            file_hash,
            start_line: start_line.max(1),
            line_hashes: shown_lines.iter().map(|line| hash_text(line)).collect(),
        };
        self.by_key
            .insert((path.to_path_buf(), tag.clone()), snapshot.clone());
        snapshot
    }

    pub fn get(&self, path: &Path, tag: &str) -> Option<&FileSnapshot> {
        self.by_key.get(&(path.to_path_buf(), tag.to_string()))
    }

    pub fn invalidate_path(&mut self, path: &Path) {
        self.by_key
            .retain(|(snapshot_path, _), _| snapshot_path != path);
    }
}

pub fn split_content_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = content.split('\n').map(ToString::to_string).collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub fn hash_text(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn short_hash(hash: &str) -> u16 {
    u16::from_str_radix(hash.get(..4).unwrap_or("0000"), 16).unwrap_or(0)
}

impl FileSnapshot {
    pub fn covers_line(&self, line: usize) -> bool {
        line >= self.start_line && line < self.start_line + self.line_hashes.len()
    }

    pub fn expected_hash(&self, line: usize) -> Option<&str> {
        if !self.covers_line(line) {
            return None;
        }
        self.line_hashes
            .get(line - self.start_line)
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_fetches_snapshot() {
        let mut store = FileSnapshotStore::default();
        let path = PathBuf::from("a.rs");
        let snapshot = store.record(&path, "a\nb\n", 10);
        assert_eq!(snapshot.start_line, 10);
        assert!(snapshot.covers_line(10));
        assert!(snapshot.covers_line(11));
        assert!(!snapshot.covers_line(12));
        assert!(store.get(&path, &snapshot.tag).is_some());
    }

    #[test]
    fn invalidates_snapshots_for_path() {
        let mut store = FileSnapshotStore::default();
        let path = PathBuf::from("a.rs");
        let other = PathBuf::from("b.rs");
        let snapshot = store.record(&path, "a\n", 1);
        let other_snapshot = store.record(&other, "b\n", 1);

        store.invalidate_path(&path);

        assert!(store.get(&path, &snapshot.tag).is_none());
        assert!(store.get(&other, &other_snapshot.tag).is_some());
    }

    #[test]
    fn split_content_lines_drops_only_trailing_separator() {
        assert_eq!(split_content_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_content_lines("a\n\nb"), vec!["a", "", "b"]);
    }
}
