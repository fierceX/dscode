use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::session::paths::{Paths, SessionLayout, paths_for_layout, session_base_dir};

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

pub async fn ensure_metadata(paths: &Paths, cwd: &Path, seed: SessionSeed) -> Result<()> {
    let now = crate::session::stats::chrono_now_rfc3339();
    let mut metadata = match read_metadata(&paths.metadata).await {
        Ok(Some(metadata)) => metadata,
        Ok(None) | Err(_) => new_metadata(paths, cwd, &now),
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

pub async fn resolve_session_reference(
    home: &Path,
    cwd: &Path,
    reference: &str,
) -> Result<Option<String>> {
    resolve_session_reference_with_layout(home, cwd, reference, SessionLayout::ProjectScoped).await
}

pub async fn resolve_session_reference_with_layout(
    home: &Path,
    cwd: &Path,
    reference: &str,
    layout: SessionLayout,
) -> Result<Option<String>> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    let records = list_sessions_with_layout(home, cwd, layout).await?;
    if records.is_empty() {
        return Ok(None);
    }

    if records.iter().any(|record| record.id == reference) {
        return Ok(Some(reference.to_string()));
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
        return Ok(Some(alias_matches[0].id.clone()));
    }
    if alias_matches.len() > 1 {
        bail!(
            "session alias '{}' is ambiguous: {}",
            reference,
            alias_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let id_prefix_matches: Vec<_> = records
        .iter()
        .filter(|record| record.id.starts_with(reference))
        .collect();
    if id_prefix_matches.len() == 1 {
        return Ok(Some(id_prefix_matches[0].id.clone()));
    }
    if id_prefix_matches.len() > 1 {
        bail!(
            "session id prefix '{}' is ambiguous: {}",
            reference,
            id_prefix_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
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
        return Ok(Some(title_matches[0].id.clone()));
    }
    if title_matches.len() > 1 {
        bail!(
            "session title '{}' is ambiguous: {}",
            reference,
            title_matches
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(None)
}

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
            Ok(None) | Err(_) => fallback_metadata("isolated", cwd, &paths),
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

    let dir = session_base_dir(home, cwd, layout);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let session_paths = paths_for_layout(home, cwd, &id, layout);
        let metadata = match read_metadata(&session_paths.metadata).await {
            Ok(Some(metadata)) => metadata,
            Ok(None) | Err(_) => fallback_metadata(&id, cwd, &session_paths),
        };
        let modified = session_activity_mod_time(&path).await?;
        records.push(SessionRecord {
            id,
            path,
            metadata,
            modified,
        });
    }
    Ok(records)
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
    tokio::fs::write(path, format!("{text}\n")).await?;
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

fn fallback_metadata(id: &str, cwd: &Path, paths: &Paths) -> SessionMetadata {
    SessionMetadata {
        id: id.to_string(),
        alias: None,
        title: None,
        created_at: String::new(),
        updated_at: String::new(),
        cwd: cwd.display().to_string(),
        parent: None,
        first_prompt: None,
        summary: std::fs::read_to_string(&paths.summary)
            .ok()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(ToString::to_string)
            }),
    }
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
mod tests {
    use super::*;
    use crate::session::paths::paths_for;

    fn temp_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mink-session-meta-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn metadata_resolves_alias_and_id_prefix() {
        let home = temp_home("resolve");
        let cwd = home.join("workspace");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
        tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
        ensure_metadata(
            &paths,
            &cwd,
            SessionSeed {
                alias: Some("feature-x".into()),
                title: Some("Feature X work".into()),
                first_prompt: Some("implement feature x".into()),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_session_reference(&home, &cwd, "feature-x")
                .await
                .unwrap(),
            Some("20260604-120000-abcd".into())
        );
        assert_eq!(
            resolve_session_reference(&home, &cwd, "20260604")
                .await
                .unwrap(),
            Some("20260604-120000-abcd".into())
        );
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn metadata_resolves_sanitized_alias_reference() {
        let home = temp_home("resolve-sanitized");
        let cwd = home.join("workspace");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
        tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
        ensure_metadata(
            &paths,
            &cwd,
            SessionSeed {
                alias: Some("feature-x".into()),
                title: Some("Feature X work".into()),
                first_prompt: Some("implement feature x".into()),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_session_reference(&home, &cwd, "feature x")
                .await
                .unwrap(),
            Some("20260604-120000-abcd".into())
        );
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn malformed_metadata_falls_back_to_legacy_summary() {
        let home = temp_home("malformed");
        let cwd = home.join("workspace");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
        tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
        tokio::fs::write(&paths.metadata, "{not-json")
            .await
            .unwrap();
        tokio::fs::write(&paths.summary, "legacy summary\n")
            .await
            .unwrap();

        let records = list_project_sessions(&home, &cwd).await.unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].metadata.summary.as_deref(),
            Some("legacy summary")
        );
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn ensure_metadata_recovers_malformed_file() {
        let home = temp_home("recover");
        let cwd = home.join("workspace");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
        tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
        tokio::fs::write(&paths.metadata, "{not-json")
            .await
            .unwrap();

        ensure_metadata(
            &paths,
            &cwd,
            SessionSeed {
                alias: Some("feature-x".into()),
                title: Some("Feature X work".into()),
                first_prompt: None,
            },
        )
        .await
        .unwrap();
        let metadata = read_metadata(&paths.metadata).await.unwrap().unwrap();

        assert_eq!(metadata.alias.as_deref(), Some("feature-x"));
        assert_eq!(metadata.title.as_deref(), Some("Feature X work"));
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[test]
    fn title_from_prompt_uses_first_non_empty_line() {
        assert_eq!(
            title_from_prompt("\n  fix session naming\nmore").as_deref(),
            Some("fix session naming")
        );
    }

    #[test]
    fn sanitize_alias_rejects_path_like_names() {
        assert_eq!(sanitize_alias("../x"), None);
        assert_eq!(sanitize_alias("feature x").as_deref(), Some("feature-x"));
    }
}
