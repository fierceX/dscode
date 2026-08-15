//! Read-only session discovery and inspection for hosts embedding mink.
//!
//! Runtime construction remains the only supported way to mutate a session.
//! This module intentionally exposes records rather than the internal session
//! stores and path-building machinery.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use crate::session::artifacts::{ArtifactManager, ArtifactRecord};
pub use crate::session::metadata::{SessionMetadata, SessionSeed};
pub use crate::session::paths::Paths as SessionPaths;
pub use crate::session::todo::{TodoSnapshot, TodoStatus};
pub use crate::session::usage::{
    PricingCatalog, TokenUsage, UsageCost, UsageKind, UsageRecord, UsageStatus, UsageSummary,
};

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub path: PathBuf,
    pub alias: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub cwd: String,
    pub parent: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub modified: SystemTime,
}

impl From<crate::session::metadata::SessionRecord> for SessionRecord {
    fn from(record: crate::session::metadata::SessionRecord) -> Self {
        let metadata = record.metadata;
        Self {
            id: record.id,
            path: record.path,
            alias: metadata.alias,
            title: metadata.title,
            summary: metadata.summary,
            cwd: metadata.cwd,
            parent: metadata.parent,
            created_at: metadata.created_at,
            updated_at: metadata.updated_at,
            modified: record.modified,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionCatalog {
    home: PathBuf,
    cwd: PathBuf,
    layout: super::SessionLayout,
}

impl SessionCatalog {
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            cwd: cwd.into(),
            layout: super::SessionLayout::ProjectScoped,
        }
    }

    pub fn with_layout(mut self, layout: super::SessionLayout) -> Self {
        self.layout = layout;
        self
    }

    pub async fn list(&self) -> Result<Vec<SessionRecord>> {
        crate::session::metadata::list_sessions_with_layout(&self.home, &self.cwd, self.layout)
            .await
            .map(|records| records.into_iter().map(SessionRecord::from).collect())
    }

    pub async fn resolve(&self, reference: &str) -> Result<Option<SessionRecord>> {
        crate::session::metadata::resolve_session_record_with_layout(
            &self.home,
            &self.cwd,
            reference,
            self.layout,
        )
        .await
        .map(|record| record.map(SessionRecord::from))
    }
}

#[derive(Debug, Clone)]
pub struct SessionReader {
    directory: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionUsage {
    pub summary: UsageSummary,
    pub last_context_tokens: u64,
}

impl SessionReader {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn usage(&self) -> Result<UsageSummary> {
        self.usage_snapshot().map(|usage| usage.summary)
    }

    pub fn usage_snapshot(&self) -> Result<SessionUsage> {
        let records = crate::session::usage::read_records(&self.directory.join("usage.jsonl"))?;
        let last_context_tokens = records
            .iter()
            .rev()
            .find_map(|record| record.tokens.as_ref())
            .map(|tokens| {
                tokens
                    .input_tokens
                    .saturating_add(tokens.cache_read_tokens)
                    .saturating_add(tokens.cache_creation_tokens)
            })
            .unwrap_or_default();
        Ok(SessionUsage {
            summary: UsageSummary::from_records(&records),
            last_context_tokens,
        })
    }

    pub fn todo(&self) -> Result<TodoSnapshot> {
        crate::session::todo::TodoStore::load(self.directory.join("todos.json"))
            .map(|store| store.snapshot())
    }
}

pub fn title_from_prompt(prompt: &str) -> Option<String> {
    crate::session::metadata::title_from_prompt(prompt)
}

pub fn first_line(text: &str) -> &str {
    crate::session::store::first_line(text)
}

pub fn build_tool_summary_from_json(name: &str, event: &serde_json::Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, event)
}

pub fn sanitize_alias(raw: &str) -> Option<String> {
    crate::session::metadata::sanitize_alias(raw)
}

pub fn project_key(cwd: &Path) -> String {
    crate::session::paths::project_key(cwd)
}

pub fn new_session_id() -> String {
    crate::session::paths::chrono_session_id()
}

pub fn paths_for(home: &Path, cwd: &Path, session_id: &str) -> SessionPaths {
    crate::session::paths::paths_for(home, cwd, session_id)
}

pub async fn resolve_record(
    home: &Path,
    cwd: &Path,
    reference: &str,
    layout: super::SessionLayout,
) -> Result<Option<crate::session::metadata::SessionRecord>> {
    crate::session::metadata::resolve_session_record_with_layout(home, cwd, reference, layout).await
}

pub async fn ensure_metadata(paths: &SessionPaths, cwd: &Path, seed: SessionSeed) -> Result<()> {
    crate::session::metadata::ensure_metadata(paths, cwd, seed).await
}
