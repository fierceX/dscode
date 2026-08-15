//! In-session memory of local file reads (engine-internal; never exposed in prompts).
//!
//! A memo hit means "the file is byte-identical and unchanged since it was last
//! read, and the requested range is covered by an earlier read". The caller
//! turns a hit into a short behavioral response so the model reuses the content
//! it already has instead of paying for a full re-read.
//!
//! Guards:
//! - `len` + `mtime` must match (the file did not change on disk).
//! - `epoch` must match: compaction commits invalidate all memos (the model's
//!   context no longer contains the previously read content, so "reuse" would
//!   reference text the model cannot see).
//! - `mutation_epoch` must match: any Write/Edit success invalidates all memos
//!   of the same agent (changed files must be re-read before editing).
//!
//! Scope: local files only. Virtual-filesystem and registered-resource reads
//! are not memoized. Each agent (parent and each sub-agent) owns an independent
//! memo instance.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::tools::snapshot::canonical_snapshot_path;

/// Maximum number of memo entries per agent.
const MEMO_MAX_ENTRIES: usize = 256;

#[derive(Debug, Clone)]
struct MemoEntry {
    len: u64,
    mtime: SystemTime,
    /// True when the read was `:raw` (no line numbers / hashline header).
    raw: bool,
    /// None = the whole file was read.
    start_line: Option<usize>,
    /// Inclusive end line; None = through EOF.
    end_line: Option<usize>,
    epoch: u64,
    mutation_epoch: u64,
}

#[derive(Debug, Default)]
pub struct ReadMemo {
    entries: HashMap<PathBuf, VecDeque<MemoEntry>>,
    recency: VecDeque<PathBuf>,
    total: usize,
}

impl ReadMemo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a read of `path` covering `start_line..=end_line` (None = full file).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        path: &Path,
        len: u64,
        mtime: SystemTime,
        raw: bool,
        start_line: Option<usize>,
        end_line: Option<usize>,
        epoch: u64,
        mutation_epoch: u64,
    ) {
        let path = canonical_snapshot_path(path);
        let entry = MemoEntry {
            len,
            mtime,
            raw,
            start_line,
            end_line,
            epoch,
            mutation_epoch,
        };
        let list = self.entries.entry(path.clone()).or_default();
        let before = list.len();
        list.retain(|existing| {
            !(existing.raw == raw
                && existing.start_line == start_line
                && existing.end_line == end_line)
        });
        self.total = self.total.saturating_sub(before - list.len());
        list.push_front(entry);
        self.total += 1;
        self.touch(&path);
        self.evict();
    }

    /// True when an earlier read covers the requested range and all guards match.
    #[allow(clippy::too_many_arguments)]
    pub fn hit(
        &self,
        path: &Path,
        len: u64,
        mtime: SystemTime,
        raw: bool,
        epoch: u64,
        mutation_epoch: u64,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> bool {
        let path = canonical_snapshot_path(path);
        let Some(list) = self.entries.get(&path) else {
            return false;
        };
        list.iter().any(|entry| {
            entry.len == len
                && entry.mtime == mtime
                && entry.raw == raw
                && entry.epoch == epoch
                && entry.mutation_epoch == mutation_epoch
                && covers(entry, start_line, end_line)
        })
    }

    /// Drop every entry (used on epoch changes so stale entries cannot linger).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.total = 0;
    }

    fn touch(&mut self, path: &PathBuf) {
        self.recency.retain(|candidate| candidate != path);
        self.recency.push_back(path.clone());
    }

    fn evict(&mut self) {
        while self.total > MEMO_MAX_ENTRIES {
            let Some(path) = self.recency.pop_front() else {
                break;
            };
            if let Some(list) = self.entries.get_mut(&path) {
                self.total = self.total.saturating_sub(list.len());
                list.clear();
            }
            self.entries.remove(&path);
        }
    }
}

fn covers(entry: &MemoEntry, start_line: Option<usize>, end_line: Option<usize>) -> bool {
    match (start_line, end_line) {
        (None, None) => entry.start_line.is_none() && entry.end_line.is_none(),
        (Some(start), Some(end)) => {
            entry.start_line.is_none_or(|s| s <= start) && entry.end_line.is_none_or(|e| e >= end)
        }
        // "from line N to EOF" requires an entry that also runs to EOF.
        (Some(start), None) => {
            entry.start_line.is_none_or(|s| s <= start) && entry.end_line.is_none()
        }
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
#[path = "read_memo_tests.rs"]
mod tests;
