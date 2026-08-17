use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Filesystem layout used to derive the session directory from `home`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum SessionLayout {
    /// Historical CLI layout. `home` is a user/service root:
    /// `home/.mink/projects/<project_key(cwd)>/<session_id>`.
    #[serde(rename = "project")]
    #[default]
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
    pub todos: PathBuf,
    pub stats: PathBuf,
    pub usage: PathBuf,
    pub artifacts: PathBuf,
}

impl Paths {
    pub fn from_session_dir(session_id: impl Into<String>, session_dir: PathBuf) -> Self {
        let session_id = session_id.into();
        let base_dir = session_dir
            .parent()
            .map_or_else(|| session_dir.clone(), Path::to_path_buf);
        Self {
            session_id,
            base_dir,
            conversation: session_dir.join("conversation.jsonl"),
            events: session_dir.join("events.jsonl"),
            summary: session_dir.join("summary.txt"),
            metadata: session_dir.join("session.json"),
            plan: session_dir.join("plan.md"),
            plan_draft: session_dir.join("plan.draft"),
            todos: session_dir.join("todos.json"),
            stats: session_dir.join("stats.json"),
            usage: session_dir.join("usage.jsonl"),
            artifacts: session_dir.join("artifacts"),
            session_dir,
        }
    }
}

/// Derive a filesystem-safe project key from the working directory path.
pub fn project_key(cwd: &Path) -> String {
    let normalized = normalized_project_path(cwd);
    let digest = Sha256::digest(normalized.as_bytes());
    let hash = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut readable = normalized
        .trim_start_matches('/')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while readable.contains("--") {
        readable = readable.replace("--", "-");
    }
    readable = readable.trim_matches('-').chars().take(48).collect();
    if readable.is_empty() {
        readable.push_str("root");
    }
    format!("{readable}--{hash}")
}

/// Historical project key retained for read-only compatibility.
pub fn legacy_project_key(cwd: &Path) -> String {
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

pub fn canonical_project_path(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| {
        let absolute = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(cwd)
        };
        lexical_normalize(&absolute)
    })
}

fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn normalized_project_path(cwd: &Path) -> String {
    let mut value = canonical_project_path(cwd)
        .to_string_lossy()
        .replace('\\', "/");
    if value.as_bytes().get(1) == Some(&b':') {
        value.replace_range(0..1, &value[..1].to_ascii_lowercase());
    }
    value
}

pub fn project_base_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let root = home.join(".mink/projects");
    let new = root.join(project_key(cwd));
    let old = root.join(legacy_project_key(cwd));
    if new == old {
        vec![new]
    } else {
        vec![new, old]
    }
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
        todos: session_dir.join("todos.json"),
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

/// Generate a unique session ID: YYYYMMDD-HHmmss-XXXX.
pub fn chrono_session_id() -> String {
    use time::format_description::FormatItem;
    use time::macros::format_description;
    static FMT: &[FormatItem<'_>] =
        format_description!("[year][month][day]-[hour][minute][second]");
    let base = time::OffsetDateTime::now_utc()
        .format(FMT)
        .unwrap_or_else(|_| String::new());
    // 完整 subsec_nanos（u32）：u16 截断使"随机"后缀每 65.5µs 重复一次，
    // 并行子代理会话 id 可碰撞导致第二个子代理启动失败。
    let rand_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{:08x}", base, rand_suffix)
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
