use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::session::paths::{
    Paths, SessionLayout, canonical_project_path, paths_for_layout, project_base_dirs,
    session_base_dir,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionMetadata {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub path: PathBuf,
    pub metadata: SessionMetadata,
    pub modified: SystemTime,
}

#[derive(Debug, Clone)]
pub struct SessionSeed {
    pub alias: Option<String>,
    pub title: Option<String>,
    pub first_prompt: Option<String>,
}

/// Typed failure for session-reference resolution.
///
/// `None` is the normal "reference did not match any session" result.
/// Ambiguity is modeled explicitly so hosts such as mink-server can map it
/// to a conflict response without parsing human-readable strings.
#[derive(Debug)]
pub enum SessionReferenceError {
    Ambiguous(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for SessionReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambiguous(message) => f.write_str(message),
            Self::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for SessionReferenceError {}

impl From<anyhow::Error> for SessionReferenceError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

pub async fn ensure_metadata(paths: &Paths, cwd: &Path, seed: SessionSeed) -> Result<()> {
    let now = crate::session::stats::chrono_now_rfc3339();
    let mut metadata = match read_metadata(&paths.metadata).await {
        Ok(Some(metadata)) => metadata,
        Ok(None) => new_metadata(paths, cwd, &now),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "corrupt session metadata {}: {error}",
                paths.metadata.display()
            ));
        }
    };

    if metadata.id.is_empty() {
        metadata.id = paths.session_id.clone();
    }
    if metadata.cwd.is_empty() {
        metadata.cwd = cwd.display().to_string();
    }
    if metadata.alias.is_none() {
        metadata.alias = seed.alias;
    }
    if metadata.title.is_none() {
        metadata.title = seed.title.or_else(|| metadata.alias.clone());
    }
    if metadata.first_prompt.is_none() {
        metadata.first_prompt = seed.first_prompt;
    }
    if metadata.summary.is_none() {
        metadata.summary = read_summary_line(&paths.summary).await?;
    }
    metadata.updated_at = now;

    write_metadata(&paths.metadata, &metadata).await
}

#[cfg(test)]
pub async fn resolve_session_reference(
    home: &Path,
    cwd: &Path,
    reference: &str,
) -> Result<Option<String>> {
    resolve_session_reference_with_layout(home, cwd, reference, SessionLayout::ProjectScoped).await
}

#[cfg(test)]
pub async fn resolve_session_reference_with_layout(
    home: &Path,
    cwd: &Path,
    reference: &str,
    layout: SessionLayout,
) -> Result<Option<String>> {
    Ok(
        resolve_session_record_with_layout(home, cwd, reference, layout)
            .await?
            .map(|record| record.id),
    )
}

pub async fn resolve_session_record_with_layout(
    home: &Path,
    cwd: &Path,
    reference: &str,
    layout: SessionLayout,
) -> std::result::Result<Option<SessionRecord>, SessionReferenceError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    let records = list_sessions_with_layout(home, cwd, layout).await?;
    if records.is_empty() {
        return Ok(None);
    }

    if let Some(record) = records.iter().find(|record| record.id == reference) {
        return Ok(Some(record.clone()));
    }
    let normalized_alias = sanitize_alias(reference);
    let alias_matches: Vec<_> = records
        .iter()
        .filter(|record| {
            record.metadata.alias.as_deref() == Some(reference)
                || normalized_alias
                    .as_deref()
                    .is_some_and(|alias| record.metadata.alias.as_deref() == Some(alias))
        })
        .collect();
    if alias_matches.len() == 1 {
        return Ok(Some(alias_matches[0].clone()));
    }
    if alias_matches.len() > 1 {
        return Err(SessionReferenceError::Ambiguous(format!(
            "session alias '{}' is ambiguous: {}",
            reference,
            alias_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let id_prefix_matches: Vec<_> = records
        .iter()
        .filter(|record| record.id.starts_with(reference))
        .collect();
    if id_prefix_matches.len() == 1 {
        return Ok(Some(id_prefix_matches[0].clone()));
    }
    if id_prefix_matches.len() > 1 {
        return Err(SessionReferenceError::Ambiguous(format!(
            "session id prefix '{}' is ambiguous: {}",
            reference,
            id_prefix_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let title_matches: Vec<_> = records
        .iter()
        .filter(|record| {
            record
                .metadata
                .title
                .as_deref()
                .is_some_and(|title| title.contains(reference))
        })
        .collect();
    if title_matches.len() == 1 {
        return Ok(Some(title_matches[0].clone()));
    }
    if title_matches.len() > 1 {
        return Err(SessionReferenceError::Ambiguous(format!(
            "session title '{}' is ambiguous: {}",
            reference,
            title_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    Ok(None)
}

#[cfg(test)]
pub async fn list_project_sessions(home: &Path, cwd: &Path) -> Result<Vec<SessionRecord>> {
    list_sessions_with_layout(home, cwd, SessionLayout::ProjectScoped).await
}

pub async fn list_sessions_with_layout(
    home: &Path,
    cwd: &Path,
    layout: SessionLayout,
) -> Result<Vec<SessionRecord>> {
    if layout == SessionLayout::Isolated {
        if !home.exists() {
            return Ok(Vec::new());
        }
        let paths = paths_for_layout(home, cwd, "isolated", layout);
        let metadata = match read_metadata(&paths.metadata).await {
            Ok(Some(metadata)) => metadata,
            Ok(None) if !session_dir_has_data(&paths.session_dir).await? => return Ok(Vec::new()),
            Ok(None) => {
                bail!(
                    "session metadata missing for existing session {}",
                    paths.metadata.display()
                )
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "corrupt session metadata {}: {error}",
                    paths.metadata.display()
                ));
            }
        };
        let id = if metadata.id.is_empty() {
            "isolated".to_string()
        } else {
            metadata.id.clone()
        };
        let modified = session_activity_mod_time(&paths.session_dir).await?;
        return Ok(vec![SessionRecord {
            id,
            path: paths.session_dir.clone(),
            metadata,
            modified,
        }]);
    }

    let dirs = if layout == SessionLayout::ProjectScoped {
        project_base_dirs(home, cwd)
    } else {
        vec![session_base_dir(home, cwd, layout)]
    };
    if dirs.iter().all(|dir| !dir.exists()) {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let is_legacy_dir = layout == SessionLayout::ProjectScoped
            && dir.file_name().is_some_and(|name| {
                name == std::ffi::OsStr::new(&crate::session::paths::legacy_project_key(cwd))
            });
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let session_paths = Paths::from_session_dir(&id, path.clone());
            let metadata = match read_metadata(&session_paths.metadata).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) if is_legacy_dir => continue,
                Ok(None) => {
                    bail!(
                        "session metadata missing for existing session {}",
                        session_paths.metadata.display()
                    )
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "corrupt session metadata {}: {error}",
                        session_paths.metadata.display()
                    ));
                }
            };
            if layout == SessionLayout::ProjectScoped
                && canonical_project_path(Path::new(&metadata.cwd)) != canonical_project_path(cwd)
            {
                continue;
            }
            let record_id = if metadata.id.is_empty() {
                id
            } else {
                metadata.id.clone()
            };
            let modified = session_activity_mod_time(&path).await?;
            records.push(SessionRecord {
                id: record_id,
                path,
                metadata,
                modified,
            });
        }
    }
    let mut identities = std::collections::BTreeMap::<String, Vec<PathBuf>>::new();
    for record in &records {
        identities
            .entry(record.id.clone())
            .or_default()
            .push(record.path.clone());
        if let Some(alias) = &record.metadata.alias {
            identities
                .entry(format!("alias:{alias}"))
                .or_default()
                .push(record.path.clone());
        }
    }
    if let Some((identity, paths)) = identities.into_iter().find(|(_, paths)| paths.len() > 1) {
        bail!(
            "session identity '{identity}' is ambiguous across: {}",
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(records)
}

pub async fn session_dir_has_data(session_dir: &Path) -> Result<bool> {
    let mut entries = match tokio::fs::read_dir(session_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        if name != std::ffi::OsStr::new("session.lock") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn new_metadata(paths: &Paths, cwd: &Path, now: &str) -> SessionMetadata {
    SessionMetadata {
        id: paths.session_id.clone(),
        alias: None,
        title: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        cwd: cwd.display().to_string(),
        parent: None,
        first_prompt: None,
        summary: None,
    }
}

async fn read_metadata(path: &Path) -> Result<Option<SessionMetadata>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn write_metadata(path: &Path, metadata: &SessionMetadata) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let text = serde_json::to_string_pretty(metadata)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::session::atomic_file::atomic_replace(&path, format!("{text}\n").as_bytes())
    })
    .await??;
    Ok(())
}

async fn read_summary_line(path: &Path) -> Result<Option<String>> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string))
}

async fn session_activity_mod_time(session_dir: &Path) -> Result<SystemTime> {
    let events = session_dir.join("events.jsonl");
    if let Ok(meta) = tokio::fs::metadata(&events).await {
        return Ok(meta.modified()?);
    }
    Ok(tokio::fs::metadata(session_dir).await?.modified()?)
}

pub fn sanitize_alias(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('/') || raw.contains('\\') || raw.contains("..") {
        return None;
    }
    Some(
        raw.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string(),
    )
    .filter(|alias| !alias.is_empty())
}

pub fn title_from_prompt(prompt: &str) -> Option<String> {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut title = String::new();
    for ch in line.chars().take(80) {
        title.push(ch);
    }
    Some(title)
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
