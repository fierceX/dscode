//! Session registry: scans the mink home, manages the active runtime map,
//! and owns the `session.lock` mutual-exclusion protocol.
//!
//! The lock is advisory: the TUI does not take locks, so a lock file only
//! proves the server itself (or a previous server process) holds the session.
//! Opening refuses a live lock held by a running process; stale locks are
//! reclaimed by dead-pid or timestamp detection.

use crate::session::runtime::SessionRuntime;
use std::sync::Arc;
use anyhow::{anyhow, Result};
use mink::runtime::{AgentOptions, SessionPolicy};
use mink::session::metadata::SessionMetadata;
use mink::session::usage::UsageRecord;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const LOCK_FILE: &str = "session.lock";
const LOCK_STALE_SECS: u64 = 300;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub alias: Option<String>,
    pub title: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub modified_secs: Option<u64>,
    /// Server-side runtime state: free (disk only) | active | running.
    pub status: &'static str,
    pub path: String,
    /// Usage 汇总（usage.jsonl）：会话累计 tokens 与费用，无记录时为 0。
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cost_nano_cny: u64,
    /// 最近一次请求的上下文估计（usage.jsonl 最后记录 input+cache），0 表示无记录
    pub last_context_tokens: u64,
}

/// 读取会话 usage.jsonl 并汇总 tokens/费用（失败静默返回零值）。
fn summarize_usage(dir: &Path) -> (u64, u64, u64, u64, u64) {
    let path = dir.join("usage.jsonl");
    let Ok(text) = fs::read_to_string(&path) else {
        return (0, 0, 0, 0, 0);
    };
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache = 0u64;
    let mut cost = 0u64;
    let mut last_context = 0u64;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<UsageRecord>(line) else {
            continue;
        };
        if let Some(tokens) = record.tokens {
            // 输入不含缓存（与实时 usage 事件语义一致；缓存单独统计）
            input = input.saturating_add(tokens.input_tokens);
            output = output.saturating_add(tokens.output_tokens);
            cache = cache.saturating_add(tokens.cache_read_tokens).saturating_add(tokens.cache_creation_tokens);
            // 最后一条记录 = 最近一次请求的上下文（当前上下文，含缓存）
            last_context = tokens.input_tokens.saturating_add(tokens.cache_read_tokens).saturating_add(tokens.cache_creation_tokens);
        }
        cost = cost.saturating_add(record.cost_nano_cny.unwrap_or(0));
    }
    (input, output, cache, cost, last_context)
}

struct ActiveSession {
    runtime: Arc<SessionRuntime>,
    lock_path: PathBuf,
}

pub struct Registry {
    home: PathBuf,
    model: String,
    active: Mutex<HashMap<String, ActiveSession>>,
    max_running: usize,
}

impl Registry {
    pub fn new(home: PathBuf, model: String, max_running: usize) -> Self {
        Self {
            home,
            model,
            active: Mutex::new(HashMap::new()),
            max_running,
        }
    }

    /// Scan all sessions under `~/.mink/projects/<project_key>/<id>` across
    /// every project (workspace), not just the current cwd's project — the
    /// three-pane UI groups sessions by workspace directory.
    pub async fn list(&self) -> Result<Vec<SessionSummary>> {
        let active = self.active.lock().unwrap();
        let mut out = Vec::new();
        for (dir, metadata, modified) in scan_all_sessions(&self.home) {
            // 过滤子代理会话：parent 非空的 session 不单独展示
            if metadata.as_ref().and_then(|m| m.parent.as_ref()).is_some() {
                continue;
            }
            let id = metadata.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| {
                dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            });
            // 防御性过滤：子代理会话不单独展示（sub_ 前缀 / parent 字段）。
            // 实际子代理在 <session>/subagents/sub_xxx/ 深层目录，一层扫描本就不含。
            if id.starts_with("sub_")
                || metadata.as_ref().and_then(|m| m.parent.as_ref()).is_some()
            {
                continue;
            }
            let status = active.get(&id).map(|a| {
                if a.runtime.running() {
                    "running"
                } else {
                    "active"
                }
            });
            let mut summary = summary_from_metadata(metadata, modified, &dir);
            summary.status = status.unwrap_or("free");
            out.push(summary);
        }
        out.sort_by(|a, b| b.modified_secs.unwrap_or(0).cmp(&a.modified_secs.unwrap_or(0)));
        Ok(out)
    }

    /// Create a session on disk. `name` becomes the session alias (mink's
    /// `UseOrCreate` semantics): the runtime builds the session directory +
    /// metadata, then we shut it down, leaving a disk session visible to both
    /// `list()` and the TUI.
    pub async fn create(&self, name: &str, cwd: &Path) -> Result<SessionSummary> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let runtime = SessionRuntime::open(self.build_options(name, Some(name), cwd)).await?;
        let session_id = runtime.session_id();
        runtime.shutdown().await?;
        let (dir, metadata, modified) = scan_all_sessions(&self.home)
            .into_iter()
            .find(|(_, m, _)| m.as_ref().map(|m| m.id == session_id).unwrap_or(false))
            .ok_or_else(|| anyhow!("session {session_id} was not created"))?;
        let mut summary = summary_from_metadata(metadata, modified, &dir);
        summary.status = "free";
        Ok(summary)
    }

    /// Open a session: refuse a live lock, build the runtime, then take the
    /// lock. Idempotent for sessions this server already holds.
    pub async fn open(&self, id: &str) -> Result<()> {
        // 锁内只做“决定”，所有 .await 都在锁外（std MutexGuard 是 !Send，
        // 且词法层面“可能被后续分支使用”会让 future 非 Send）。
        {
            let active = self.active.lock().unwrap();
            if active.contains_key(id) {
                return Ok(());
            }
        }
        let dir = self.session_dir(id).await?;
        let lock_path = dir.join(LOCK_FILE);
        if lock_path.exists() && !lock_is_stale(&lock_path) {
            anyhow::bail!("session {id} is locked by another process");
        }
        // 使用 session 自身的 cwd（跨工作区打开的关键：UseOrCreate 需要在
        // 正确的 project 布局下解析，而不是服务启动目录）。
        let session_cwd = session_cwd_from_dir(&dir);
        let runtime = SessionRuntime::open(self.build_options(id, None, &session_cwd)).await?;
        let actual_id = runtime.session_id();
        // 校验与插入顺序：先验证实际解析出的 id，再入 active——
        // 校验失败时不得留下不一致状态。
        if actual_id != id {
            let _ = runtime.shutdown().await;
            anyhow::bail!("opened session {actual_id} instead of {id}");
        }


        // 并发 open 防重入：构建期间另一请求可能已插入同一 id（锁内决定）。
        let already_open = { self.active.lock().unwrap().contains_key(id) };
        if already_open {
            let _ = runtime.shutdown().await;
            return Ok(());
        }
        // 锁内插入（无 await）。
        {
            let mut active = self.active.lock().unwrap();
            write_lock(&lock_path)?;
            active.insert(
                id.to_string(),
                ActiveSession {
                    runtime: Arc::new(runtime),
                    lock_path,
                },
            );
        }
        Ok(())
    }

    /// Submit a user input on an open session. The turn runs on its own task;
    /// events land in `events.jsonl` via the core (same file as the TUI).
    /// 获取活动 runtime 的事件 receiver（SSE 订阅；Arc 在取 receiver 后释放）。
    pub fn active_runtime(&self, id: &str) -> Option<Arc<SessionRuntime>> {
        self.active.lock().unwrap().get(id).map(|a| a.runtime.clone())
    }

    pub fn start_turn(&self, id: &str, input: String) -> Result<()> {
        let active = self.active.lock().unwrap();
        let session = active
            .get(id)
            .ok_or_else(|| anyhow!("session {id} is not open"))?;
        if session.runtime.running() {
            anyhow::bail!("session {id} already has a turn in progress");
        }
        let running = active.values().filter(|a| a.runtime.running()).count();
        if running >= self.max_running {
            anyhow::bail!("too many running sessions (limit {})", self.max_running);
        }
        session.runtime.start_turn(input)
    }

    pub fn interrupt(&self, id: &str) -> Result<()> {
        let active = self.active.lock().unwrap();
        let session = active
            .get(id)
            .ok_or_else(|| anyhow!("session {id} is not open"))?;
        session.runtime.interrupt();
        Ok(())
    }

    /// Close an open session: shutdown the runtime and release the lock.
    pub async fn close(&self, id: &str) -> Result<()> {
        let session = self
            .active
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| anyhow!("session {id} is not open"))?;
        let _ = std::fs::remove_file(&session.lock_path);
        // Arc 独占时 shutdown（SSE 订阅不持有 Arc——receiver 独立）；否则 drop 释放
        match Arc::try_unwrap(session.runtime) {
            Ok(rt) => {
                let _ = rt.shutdown().await;
            }
            Err(arc) => drop(arc),
        }
        Ok(())
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.active.lock().unwrap().contains_key(id)
    }

    pub fn running(&self, id: &str) -> bool {
        self.active
            .lock()
            .unwrap()
            .get(id)
            .map(|a| a.runtime.running())
            .unwrap_or(false)
    }

    /// The session's working directory (SessionMetadata.cwd).
    pub async fn session_metadata_cwd(&self, id: &str) -> Result<Option<String>> {
        Ok(scan_all_sessions(&self.home)
            .into_iter()
            .find(|(_, m, _)| m.as_ref().map(|m| m.id == id).unwrap_or(false))
            .and_then(|(_, m, _)| m.map(|m| m.cwd)))
    }

    /// Find the disk path of a session (full-home scan).
    pub async fn session_dir(&self, id: &str) -> Result<PathBuf> {
        scan_all_sessions(&self.home)
            .into_iter()
            .find(|(_, m, _)| m.as_ref().map(|m| m.id == id).unwrap_or(false))
            .map(|(dir, _, _)| dir)
            .ok_or_else(|| anyhow!("session {id} not found"))
    }

    fn build_options(
        &self,
        session_ref: &str,
        first_prompt: Option<&str>,
        cwd: &Path,
    ) -> AgentOptions {
        let mut cfg = mink::config::Config::default();
        if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
            cfg.api_key = k;
        }
        if let Ok(u) = std::env::var("DEEPSEEK_BASE_URL") {
            cfg.base_url = u;
        }
        if let Ok(m) = std::env::var("MODEL") {
            cfg.model = m;
        }
        cfg.log_events = true;
        cfg.interactive = false;
        let mut options = AgentOptions::from_config(cfg, &self.home, cwd)
            .with_model(&self.model)
            .with_session(SessionPolicy::UseOrCreate(session_ref.to_string()));
        if let Some(prompt) = first_prompt {
            options = options.with_first_prompt(prompt);
        }
        options
            .with_project_scoped_sessions()
            .with_log_events(true)
    }
}

fn summary_from_metadata(
    metadata: Option<SessionMetadata>,
    modified: Option<SystemTime>,
    dir: &Path,
) -> SessionSummary {
    let fallback_id = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let m = metadata.unwrap_or_else(|| SessionMetadata {
        id: fallback_id.clone(),
        alias: None,
        title: None,
        created_at: String::new(),
        updated_at: String::new(),
        cwd: String::new(),
        parent: None,
        first_prompt: None,
        summary: None,
    });
    let (tokens_in, tokens_out, cache_read_tokens, cost_nano_cny, last_context_tokens) = summarize_usage(dir);
    SessionSummary {
        id: m.id.clone(),
        alias: m.alias.clone(),
        title: m.title.clone(),
        cwd: m.cwd.clone(),
        created_at: m.created_at.clone(),
        updated_at: m.updated_at.clone(),
        modified_secs: modified
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        status: "free",
        path: dir.display().to_string(),
        tokens_in,
        tokens_out,
        cache_read_tokens,
        cost_nano_cny,
        last_context_tokens,
    }
}

/// Session 自身的工作目录（metadata.cwd），缺失时回退到 session 目录本身。
fn session_cwd_from_dir(dir: &Path) -> PathBuf {
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<SessionMetadata>(&text).ok())
        .map(|m| PathBuf::from(m.cwd))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Scan every project under `~/.mink/projects/` for session directories.
/// Returns (session dir, parsed metadata, mtime of events.jsonl).
fn scan_all_sessions(home: &Path) -> Vec<(PathBuf, Option<SessionMetadata>, Option<SystemTime>)> {
    let projects = home.join(".mink").join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project in entries.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for session in sessions.flatten() {
            let dir = session.path();
            if !dir.is_dir() {
                continue;
            }
            let metadata = std::fs::read_to_string(dir.join("session.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<SessionMetadata>(&text).ok());
            // 活动时间：events.jsonl mtime（更精确），回退到目录 mtime
            let modified = std::fs::metadata(dir.join("events.jsonl"))
                .ok()
                .and_then(|m| m.modified().ok())
                .or_else(|| dir.metadata().ok().and_then(|m| m.modified().ok()));
            out.push((dir, metadata, modified));
        }
    }
    out
}

fn write_lock(path: &Path) -> Result<()> {
    let text = format!(
        "{{\"pid\":{},\"taken_at\":{}}}",
        std::process::id(),
        now_secs()
    );
    std::fs::write(path, text)
        .map_err(|e| anyhow!("failed to write lock {}: {e}", path.display()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Check whether a lock file is stale and reclaimable.
/// A lock is stale when its process is gone, or when it is older than
/// LOCK_STALE_SECS (covers crashes that leave the file behind).
pub fn lock_is_stale(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    if let Some(pid) = parse_pid(&text) {
        if pid != 0 && pid_alive(pid) {
            return false;
        }
        return true;
    }
    // Unparseable: fall back to the file timestamp.
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    modified
        .elapsed()
        .map(|d| d > Duration::from_secs(LOCK_STALE_SECS))
        .unwrap_or(true)
}

fn parse_pid(text: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
        .map(|p| p as u32)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // u32 values above i32::MAX wrap to negative pids (kill(-1) means
        // "all processes"); treat them as non-existent.
        let pid_i = pid as i32;
        if pid_i < 0 {
            return false;
        }
        unsafe { libc::kill(pid_i, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_parse_and_stale_detection() {
        let dir = std::env::temp_dir().join(format!(
            "mink-server-lock-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOCK_FILE);
        // Dead pid is never alive in practice.
        std::fs::write(&path, format!("{{\"pid\":{}, \"taken_at\":0}}", u32::MAX)).unwrap();
        assert!(lock_is_stale(&path), "dead pid should be stale");
        std::fs::write(&path, format!("{{\"pid\":{}}}", std::process::id())).unwrap();
        assert!(!lock_is_stale(&path), "own live pid should not be stale");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
