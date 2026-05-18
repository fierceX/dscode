use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolved file-system paths for a session directory.
#[derive(Debug, Clone)]
pub struct Paths {
    pub base_dir: PathBuf,
    pub session_dir: PathBuf,
    pub conversation: PathBuf,
    pub events: PathBuf,
    pub summary: PathBuf,
    pub plan: PathBuf,
    pub plan_draft: PathBuf,
    pub stats: PathBuf,
    pub model_beliefs: PathBuf,
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
    let project_dir = home.join(".dscode/projects").join(project_key(cwd));
    let session_dir = project_dir.join(session_id);
    Paths {
        base_dir: project_dir,
        session_dir: session_dir.clone(),
        conversation: session_dir.join("conversation.jsonl"),
        events: session_dir.join("events.jsonl"),
        summary: session_dir.join("summary.txt"),
        plan: session_dir.join("plan.md"),
        plan_draft: session_dir.join("plan.draft"),
        stats: session_dir.join("stats.json"),
        model_beliefs: session_dir.join("model_beliefs.json"),
    }
}

pub async fn ensure_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

pub async fn continue_session(home: &Path, cwd: &Path) -> Result<String> {
    let dir = home.join(".dscode/projects").join(project_key(cwd));
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
        assert_eq!(parts[0].len(), 8);  // YYYYMMDD
        assert_eq!(parts[1].len(), 6);  // HHmmss
        assert_eq!(parts[2].len(), 4);  // XXXX
    }

    #[test]
    fn project_key_different_dirs_different_keys() {
        let k1 = project_key(std::path::Path::new("/a/b"));
        let k2 = project_key(std::path::Path::new("/a/c"));
        assert_ne!(k1, k2);
    }
}
