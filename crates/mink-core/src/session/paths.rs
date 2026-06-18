use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Filesystem layout used to derive the session directory from `home`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum SessionLayout {
    /// Historical CLI layout. `home` is a user/service root:
    /// `home/.mink/projects/<project_key(cwd)>/<session_id>`.
    #[serde(rename = "project")]
    ProjectScoped,
    /// Shared SDK home layout. `home` is a user/service root:
    /// `home/.mink/sessions/<session_id>`.
    #[serde(rename = "home")]
    HomeScoped,
    /// Embedded shared-root layout. `home` is a mink session collection root:
    /// `home/<session_id>`.
    #[serde(rename = "direct")]
    Direct,
    /// Embedded isolated layout. `home` is already the concrete session root.
    #[serde(rename = "isolated")]
    Isolated,
}

impl Default for SessionLayout {
    fn default() -> Self {
        Self::ProjectScoped
    }
}

/// Resolved file-system paths for a session directory.
#[derive(Debug, Clone)]
pub struct Paths {
    pub session_id: String,
    pub base_dir: PathBuf,
    pub session_dir: PathBuf,
    pub conversation: PathBuf,
    pub events: PathBuf,
    pub summary: PathBuf,
    pub metadata: PathBuf,
    pub plan: PathBuf,
    pub plan_draft: PathBuf,
    pub stats: PathBuf,
    pub usage: PathBuf,
    pub artifacts: PathBuf,
}

/// Derive a filesystem-safe project key from the working directory path.
pub fn project_key(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let stripped = s.strip_prefix(std::path::MAIN_SEPARATOR).unwrap_or(&s);
    let mut clean = stripped.replace(std::path::MAIN_SEPARATOR, "-");
    while clean.contains("--") {
        clean = clean.replace("--", "-");
    }
    let mut out = String::from("-");
    for ch in clean.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Build all session paths for a given home directory, working directory, and session ID.
pub fn paths_for(home: &Path, cwd: &Path, session_id: &str) -> Paths {
    paths_for_layout(home, cwd, session_id, SessionLayout::ProjectScoped)
}

/// Build all session paths with an explicit layout strategy.
pub fn paths_for_layout(home: &Path, cwd: &Path, session_id: &str, layout: SessionLayout) -> Paths {
    let base_dir = session_base_dir(home, cwd, layout);
    let session_dir = if layout == SessionLayout::Isolated {
        base_dir.clone()
    } else {
        base_dir.join(session_id)
    };
    Paths {
        session_id: session_id.to_string(),
        base_dir,
        session_dir: session_dir.clone(),
        conversation: session_dir.join("conversation.jsonl"),
        events: session_dir.join("events.jsonl"),
        summary: session_dir.join("summary.txt"),
        metadata: session_dir.join("session.json"),
        plan: session_dir.join("plan.md"),
        plan_draft: session_dir.join("plan.draft"),
        stats: session_dir.join("stats.json"),
        usage: session_dir.join("usage.jsonl"),
        artifacts: session_dir.join("artifacts"),
    }
}

pub fn session_base_dir(home: &Path, cwd: &Path, layout: SessionLayout) -> PathBuf {
    match layout {
        SessionLayout::ProjectScoped => home.join(".mink/projects").join(project_key(cwd)),
        SessionLayout::HomeScoped => home.join(".mink/sessions"),
        SessionLayout::Direct => home.to_path_buf(),
        SessionLayout::Isolated => home.to_path_buf(),
    }
}

pub async fn ensure_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

pub async fn continue_session(home: &Path, cwd: &Path) -> Result<String> {
    continue_session_with_layout(home, cwd, SessionLayout::ProjectScoped).await
}

pub async fn continue_session_with_layout(
    home: &Path,
    cwd: &Path,
    layout: SessionLayout,
) -> Result<String> {
    if layout == SessionLayout::Isolated {
        if !home.exists() {
            bail!("no sessions found");
        }
        let paths = paths_for_layout(home, cwd, "isolated", layout);
        if let Ok(text) = tokio::fs::read_to_string(&paths.metadata).await
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(id) = value.get("id").and_then(|id| id.as_str())
            && !id.trim().is_empty()
        {
            return Ok(id.to_string());
        }
        return Ok(home
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(chrono_session_id));
    }

    let dir = session_base_dir(home, cwd, layout);
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    if !dir.exists() {
        bail!("no sessions found");
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let mt = session_activity_mod_time(&entry.path()).await?;
        match &newest {
            Some((ts, _)) if *ts >= mt => {}
            _ => newest = Some((mt, name)),
        }
    }
    if let Some((_, sid)) = newest {
        Ok(sid)
    } else {
        bail!("no sessions found")
    }
}

async fn session_activity_mod_time(session_dir: &Path) -> Result<std::time::SystemTime> {
    let events = session_dir.join("events.jsonl");
    if let Ok(meta) = tokio::fs::metadata(&events).await {
        return Ok(meta.modified()?);
    }
    Ok(tokio::fs::metadata(session_dir).await?.modified()?)
}

/// Generate a unique session ID: YYYYMMDD-HHmmss-XXXX.
pub fn chrono_session_id() -> String {
    use time::format_description::FormatItem;
    use time::macros::format_description;
    static FMT: &[FormatItem<'_>] =
        format_description!("[year][month][day]-[hour][minute][second]");
    let base = time::OffsetDateTime::now_utc()
        .format(FMT)
        .unwrap_or_else(|_| String::new());
    let rand_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u16)
        .unwrap_or(0);
    format!("{}-{:04x}", base, rand_suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_strips_leading_slash() {
        let key = project_key(std::path::Path::new("/Users/test/project"));
        assert!(!key.starts_with("--"));
    }

    #[test]
    fn project_key_replaces_special_chars() {
        let key = project_key(std::path::Path::new("/tmp/my project!"));
        assert!(!key.contains('!'));
        assert!(!key.contains(' '));
    }

    #[test]
    fn chrono_session_id_format() {
        let id = chrono_session_id();
        // Format: YYYYMMDD-HHmmss-XXXX
        let parts: Vec<_> = id.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 8); // YYYYMMDD
        assert_eq!(parts[1].len(), 6); // HHmmss
        assert_eq!(parts[2].len(), 4); // XXXX
    }

    #[test]
    fn project_key_different_dirs_different_keys() {
        let k1 = project_key(std::path::Path::new("/a/b"));
        let k2 = project_key(std::path::Path::new("/a/c"));
        assert_ne!(k1, k2);
    }

    #[test]
    fn paths_for_layout_project_scoped_keeps_existing_shape() {
        let paths = paths_for_layout(
            Path::new("/home/mink"),
            Path::new("/work/project"),
            "sid",
            SessionLayout::ProjectScoped,
        );
        assert_eq!(
            paths.session_dir,
            PathBuf::from("/home/mink/.mink/projects/-work-project/sid")
        );
    }

    #[test]
    fn paths_for_layout_home_scoped_skips_project_key() {
        let paths = paths_for_layout(
            Path::new("/home/mink"),
            Path::new("/work/project"),
            "sid",
            SessionLayout::HomeScoped,
        );
        assert_eq!(
            paths.session_dir,
            PathBuf::from("/home/mink/.mink/sessions/sid")
        );
    }

    #[test]
    fn paths_for_layout_direct_uses_home_as_session_root() {
        let paths = paths_for_layout(
            Path::new("/home/mink"),
            Path::new("/work/project"),
            "sid",
            SessionLayout::Direct,
        );
        assert_eq!(paths.session_dir, PathBuf::from("/home/mink/sid"));
    }

    #[test]
    fn paths_for_layout_isolated_uses_home_as_session_dir() {
        let paths = paths_for_layout(
            Path::new("/home/mink/session-root"),
            Path::new("/work/project"),
            "sid",
            SessionLayout::Isolated,
        );
        assert_eq!(paths.session_id, "sid");
        assert_eq!(paths.base_dir, PathBuf::from("/home/mink/session-root"));
        assert_eq!(paths.session_dir, PathBuf::from("/home/mink/session-root"));
        assert_eq!(
            paths.conversation,
            PathBuf::from("/home/mink/session-root/conversation.jsonl")
        );
    }
}
