//! Session registry: scans the mink home, manages the active runtime map,
//! and owns the `session.lock` mutual-exclusion protocol.
//!
//! The lock is advisory: the TUI does not take locks, so a lock file only
//! proves the server itself (or a previous server process) holds the session.
//! Opening refuses a live lock held by a running process; stale locks are
//! reclaimed by dead-pid or timestamp detection.

use crate::session::runtime::SessionRuntime;
use anyhow::{Result, anyhow};
use mink::runtime::{AgentOptions, SessionPolicy};
use mink::session::metadata::SessionMetadata;
use mink::session::usage::UsageRecord;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const LOCK_FILE: &str = "session.lock";
const LOCK_STALE_SECS: u64 = 300;

#[derive(Debug)]
pub enum RegistryError {
    NotFound(String),
    Ambiguous(String),
    Locked(String),
    Busy(String),
    Capacity(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Ambiguous(message)
            | Self::Locked(message)
            | Self::Busy(message)
            | Self::Capacity(message) => f.write_str(message),
            Self::Internal(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<anyhow::Error> for RegistryError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl RegistryError {
    fn from_resolution(error: anyhow::Error) -> Self {
        let message = error.to_string();
        if message.contains("ambiguous") {
            Self::Ambiguous(message)
        } else {
            Self::Internal(error)
        }
    }

    fn from_lease(error: anyhow::Error) -> Self {
        let message = error.to_string();
        if message.contains("locked by another process") {
            Self::Locked(message)
        } else {
            Self::Internal(error)
        }
    }
}

pub type RegistryResult<T> = std::result::Result<T, RegistryError>;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub project_key: String,
    pub corrupt: bool,
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

/// 逐行读取 usage.jsonl 并汇总 tokens/费用。缺失表示尚无用量，其他 I/O 错误传播。
fn summarize_usage(dir: &Path) -> Result<(u64, u64, u64, u64, u64)> {
    let path = dir.join("usage.jsonl");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, 0, 0, 0, 0));
        }
        Err(error) => return Err(error.into()),
    };
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache = 0u64;
    let mut cost = 0u64;
    let mut last_context = 0u64;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<UsageRecord>(&line) else {
            continue;
        };
        if let Some(tokens) = record.tokens {
            // 输入不含缓存（与实时 usage 事件语义一致；缓存单独统计）
            input = input.saturating_add(tokens.input_tokens);
            output = output.saturating_add(tokens.output_tokens);
            cache = cache
                .saturating_add(tokens.cache_read_tokens)
                .saturating_add(tokens.cache_creation_tokens);
            // 最后一条记录 = 最近一次请求的上下文（当前上下文，含缓存）
            last_context = tokens
                .input_tokens
                .saturating_add(tokens.cache_read_tokens)
                .saturating_add(tokens.cache_creation_tokens);
        }
        cost = cost.saturating_add(record.cost_nano_cny.unwrap_or(0));
    }
    Ok((input, output, cache, cost, last_context))
}

struct ActiveSession {
    runtime: Arc<SessionRuntime>,
    lock_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionLocator {
    project_key: String,
    id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CreateLocator {
    project_key: String,
    alias: String,
}

type ScannedSession = (PathBuf, Option<SessionMetadata>, Option<SystemTime>);

pub struct Registry {
    home: PathBuf,
    model: String,
    active: Mutex<HashMap<SessionLocator, ActiveSession>>,
    operation_locks: Mutex<HashMap<SessionLocator, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    create_locks: Mutex<HashMap<CreateLocator, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    max_running: usize,
    llm_backend: Option<Arc<dyn mink::runtime::LlmBackend>>,
}

impl Registry {
    pub fn new(home: PathBuf, model: String, max_running: usize) -> Self {
        Self {
            home,
            model,
            active: Mutex::new(HashMap::new()),
            operation_locks: Mutex::new(HashMap::new()),
            create_locks: Mutex::new(HashMap::new()),
            max_running,
            llm_backend: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_llm_backend(
        home: PathBuf,
        model: String,
        max_running: usize,
        llm_backend: Arc<dyn mink::runtime::LlmBackend>,
    ) -> Self {
        let mut registry = Self::new(home, model, max_running);
        registry.llm_backend = Some(llm_backend);
        registry
    }

    /// Scan all sessions under `~/.mink/projects/<project_key>/<id>` across
    /// every project (workspace), not just the current cwd's project — the
    /// three-pane UI groups sessions by workspace directory.
    pub async fn list(&self) -> Result<Vec<SessionSummary>> {
        let active_status = self
            .active
            .lock()
            .unwrap()
            .iter()
            .map(|(locator, session)| {
                (
                    locator.clone(),
                    if session.runtime.running() {
                        "running"
                    } else {
                        "active"
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut out = Vec::new();
        let home = self.home.clone();
        let scanned = tokio::task::spawn_blocking(move || -> Result<Vec<_>> {
            scan_all_sessions(&home)?
                .into_iter()
                .map(|(dir, metadata, modified)| {
                    let usage = summarize_usage(&dir)?;
                    Ok((dir, metadata, modified, usage))
                })
                .collect()
        })
        .await??;
        for (dir, metadata, modified, usage) in scanned {
            // 过滤子代理会话：parent 非空的 session 不单独展示
            if metadata.as_ref().and_then(|m| m.parent.as_ref()).is_some() {
                continue;
            }
            let id = metadata.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            // 防御性过滤：子代理会话不单独展示（sub_ 前缀 / parent 字段）。
            // 实际子代理在 <session>/subagents/sub_xxx/ 深层目录，一层扫描本就不含。
            if id.starts_with("sub_") || metadata.as_ref().and_then(|m| m.parent.as_ref()).is_some()
            {
                continue;
            }
            let locator = locator_from_dir(&dir, &id);
            let status = active_status.get(&locator).copied();
            let mut summary = summary_from_metadata(metadata, modified, &dir, usage);
            summary.status = status.unwrap_or("free");
            out.push(summary);
        }
        out.sort_by(|a, b| {
            b.modified_secs
                .unwrap_or(0)
                .cmp(&a.modified_secs.unwrap_or(0))
        });
        Ok(out)
    }

    /// Create a session on disk. `name` becomes the session alias (mink's
    /// `UseOrCreate` semantics): the runtime builds the session directory +
    /// metadata, then we shut it down, leaving a disk session visible to both
    /// `list()` and the TUI.
    pub async fn create(&self, name: &str, cwd: &Path) -> RegistryResult<SessionSummary> {
        self.create_inner(name, cwd)
            .await
            .map_err(RegistryError::from_resolution)
    }

    async fn create_inner(&self, name: &str, cwd: &Path) -> Result<SessionSummary> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let alias = mink::session::metadata::sanitize_alias(name)
            .ok_or_else(|| anyhow!("invalid session name: {name}"))?;
        let locator = CreateLocator {
            project_key: mink::session::paths::project_key(cwd),
            alias,
        };
        let create_lock = self.create_lock(&locator);
        let _create = create_lock.lock().await;

        if let Some(record) = mink::session::metadata::resolve_session_record_with_layout(
            &self.home,
            cwd,
            &locator.alias,
            mink::session::paths::SessionLayout::ProjectScoped,
        )
        .await?
        {
            let session_locator = locator_from_dir(&record.path, &record.id);
            let operation_lock = self.operation_lock(&session_locator);
            let _operation = operation_lock.lock().await;
            let mut summary = summary_from_metadata(
                Some(record.metadata),
                Some(record.modified),
                &record.path,
                summarize_usage(&record.path)?,
            );
            if let Some(active) = self.active.lock().unwrap().get(&session_locator) {
                summary.status = if active.runtime.running() {
                    "running"
                } else {
                    "active"
                };
            }
            return Ok(summary);
        }

        let session_id = mink::session::paths::chrono_session_id();
        let paths = mink::session::paths::paths_for(&self.home, cwd, &session_id);
        std::fs::create_dir_all(&paths.base_dir)?;
        std::fs::create_dir(&paths.session_dir).map_err(|error| {
            anyhow!(
                "failed to reserve session {}: {error}",
                paths.session_dir.display()
            )
        })?;
        let session_locator = locator_from_dir(&paths.session_dir, &session_id);
        let operation_lock = self.operation_lock(&session_locator);
        let _operation = operation_lock.lock().await;
        let result = async {
            let _lease = SessionLease::acquire(paths.session_dir.join(LOCK_FILE))?;
            mink::session::metadata::ensure_metadata(
                &paths,
                cwd,
                mink::session::metadata::SessionSeed {
                    alias: Some(locator.alias.clone()),
                    title: Some(name.to_string()),
                    first_prompt: Some(name.to_string()),
                },
            )
            .await?;
            let runtime = SessionRuntime::open(
                self.build_options(&session_id, None, cwd)
                    .with_session(SessionPolicy::Resume(session_id.clone())),
            )
            .await?;
            runtime.shutdown().await?;
            let metadata = read_session_metadata(&paths.session_dir)?;
            Ok(summary_from_metadata(
                Some(metadata),
                std::fs::metadata(&paths.events)
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                &paths.session_dir,
                summarize_usage(&paths.session_dir)?,
            ))
        }
        .await;
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&paths.session_dir);
        }
        result
    }

    /// Open a session: take an exclusive lease, build the runtime, then publish
    /// it in the active map. Idempotent for sessions this server already holds.
    pub async fn open(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        self.open_inner(id, project).await
    }

    async fn open_inner(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        let dir = self.session_dir(id, project).await?;
        let locator = locator_from_dir(&dir, id);
        let operation_lock = self.operation_lock(&locator);
        let _operation = operation_lock.lock().await;
        // 锁内只做“决定”，所有 .await 都在锁外（std MutexGuard 是 !Send，
        // 且词法层面“可能被后续分支使用”会让 future 非 Send）。
        let closed_lock = {
            let mut active = self.active.lock().unwrap();
            match active.get(&locator).map(|session| session.runtime.phase()) {
                Some(crate::session::runtime::RuntimePhase::Idle)
                | Some(crate::session::runtime::RuntimePhase::Running)
                | Some(crate::session::runtime::RuntimePhase::Cancelling) => return Ok(()),
                Some(crate::session::runtime::RuntimePhase::Closing) => {
                    return Err(RegistryError::Busy(format!(
                        "session {id} runtime is closing"
                    )));
                }
                Some(crate::session::runtime::RuntimePhase::Closed) => {
                    active.remove(&locator).map(|session| session.lock_path)
                }
                None => None,
            }
        };
        if let Some(lock_path) = closed_lock {
            fs::remove_file(&lock_path).map_err(|error| {
                RegistryError::Internal(anyhow!(
                    "failed to release closed runtime lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        }
        if !dir.is_dir() {
            return Err(RegistryError::NotFound(format!("session {id} not found")));
        }
        let lock_path = dir.join(LOCK_FILE);
        let mut lease =
            SessionLease::acquire(lock_path.clone()).map_err(RegistryError::from_lease)?;
        // 使用 session 自身的 cwd（跨工作区打开的关键：UseOrCreate 需要在
        // 正确的 project 布局下解析，而不是服务启动目录）。
        let session_cwd = session_cwd_from_dir(&dir).map_err(RegistryError::Internal)?;
        let runtime = SessionRuntime::open(
            self.build_options(id, None, &session_cwd)
                .with_session(SessionPolicy::Resume(id.to_string())),
        )
        .await
        .map_err(RegistryError::Internal)?;
        let actual_id = runtime.session_id();
        // 校验与插入顺序：先验证实际解析出的 id，再入 active——
        // 校验失败时不得留下不一致状态。
        if actual_id != id {
            let _ = runtime.shutdown().await;
            return Err(RegistryError::Internal(anyhow!(
                "opened session {actual_id} instead of {id}"
            )));
        }

        // 同一 locator 的 operation lock 覆盖最终 recheck、构造与 lease 转移。
        {
            let mut active = self.active.lock().unwrap();
            active.insert(
                locator,
                ActiveSession {
                    runtime: Arc::new(runtime),
                    lock_path,
                },
            );
        }
        lease.disarm();
        Ok(())
    }

    /// Submit a user input on an open session. The turn runs on its own task;
    /// events land in `events.jsonl` via the core (same file as the TUI).
    /// 获取活动 runtime 的事件 receiver（SSE 订阅；Arc 在取 receiver 后释放）。
    pub fn active_runtime(
        &self,
        id: &str,
        project: Option<&str>,
    ) -> RegistryResult<Option<Arc<SessionRuntime>>> {
        let locator = self.active_locator(id, project)?;
        Ok(locator.and_then(|locator| {
            self.active
                .lock()
                .unwrap()
                .get(&locator)
                .map(|a| a.runtime.clone())
        }))
    }

    fn active_locator(
        &self,
        id: &str,
        project: Option<&str>,
    ) -> RegistryResult<Option<SessionLocator>> {
        let active = self.active.lock().unwrap();
        let candidates = active
            .keys()
            .filter(|locator| {
                locator.id == id && project.is_none_or(|project| locator.project_key == project)
            })
            .cloned()
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => Ok(None),
            [locator] => Ok(Some(locator.clone())),
            many => Err(RegistryError::Ambiguous(format!(
                "session {id} is ambiguous across projects: {}",
                many.iter()
                    .map(|locator| locator.project_key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn required_active_locator(
        &self,
        id: &str,
        project: Option<&str>,
    ) -> RegistryResult<SessionLocator> {
        self.active_locator(id, project)?
            .ok_or_else(|| RegistryError::NotFound(format!("session {id} is not open")))
    }

    pub fn start_turn(&self, id: &str, project: Option<&str>, input: String) -> RegistryResult<()> {
        self.start_turn_inner(id, project, input)
    }

    fn start_turn_inner(
        &self,
        id: &str,
        project: Option<&str>,
        input: String,
    ) -> RegistryResult<()> {
        let locator = self.required_active_locator(id, project)?;
        let active = self.active.lock().unwrap();
        let session = active
            .get(&locator)
            .ok_or_else(|| RegistryError::NotFound(format!("session {id} is not open")))?;
        if session.runtime.closed() {
            return Err(RegistryError::NotFound(format!(
                "session {id} runtime is closed; reopen it"
            )));
        }
        if session.runtime.running() {
            return Err(RegistryError::Busy(format!(
                "session {id} already has a turn in progress"
            )));
        }
        let running = active.values().filter(|a| a.runtime.running()).count();
        if running >= self.max_running {
            return Err(RegistryError::Capacity(format!(
                "too many running sessions (limit {})",
                self.max_running
            )));
        }
        session
            .runtime
            .start_turn(input)
            .map_err(|error| RegistryError::Busy(error.to_string()))
    }

    pub fn interrupt(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        self.interrupt_inner(id, project)
    }

    fn interrupt_inner(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        let locator = self.required_active_locator(id, project)?;
        let active = self.active.lock().unwrap();
        let session = active
            .get(&locator)
            .ok_or_else(|| RegistryError::NotFound(format!("session {id} is not open")))?;
        session.runtime.interrupt();
        Ok(())
    }

    /// Close an open session: shutdown the runtime and release the lock.
    pub async fn close(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        self.close_inner(id, project).await
    }

    async fn close_inner(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        // Web clients always provide the project key. Constructing the locator
        // before consulting `active` lets close queue behind an in-flight open
        // on the same per-session operation lock.
        let locator = match project {
            Some(project_key) => SessionLocator {
                project_key: project_key.to_string(),
                id: id.to_string(),
            },
            None => self.required_active_locator(id, None)?,
        };
        let operation_lock = self.operation_lock(&locator);
        let _operation = operation_lock.lock().await;
        let (runtime, lock_path) = {
            let active = self.active.lock().unwrap();
            let session = active
                .get(&locator)
                .ok_or_else(|| RegistryError::NotFound(format!("session {id} is not open")))?;
            (session.runtime.clone(), session.lock_path.clone())
        };
        let shutdown_result = runtime.shutdown().await;
        self.active.lock().unwrap().remove(&locator);
        let release_result = std::fs::remove_file(&lock_path)
            .map_err(|error| anyhow!("failed to release lock {}: {error}", lock_path.display()));
        match (shutdown_result, release_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(RegistryError::Internal(error)),
            (Ok(()), Err(error)) => Err(RegistryError::Internal(error)),
            (Err(shutdown), Err(release)) => Err(RegistryError::Internal(anyhow!(
                "runtime shutdown failed: {shutdown:#}; lock release also failed: {release:#}"
            ))),
        }
    }

    fn operation_lock(&self, locator: &SessionLocator) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.operation_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(locator).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(locator.clone(), Arc::downgrade(&lock));
        lock
    }

    fn create_lock(&self, locator: &CreateLocator) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.create_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(locator).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(locator.clone(), Arc::downgrade(&lock));
        lock
    }

    pub async fn delete(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        self.delete_inner(id, project).await
    }

    async fn delete_inner(&self, id: &str, project: Option<&str>) -> RegistryResult<()> {
        let dir = self.session_dir(id, project).await?;
        let locator = locator_from_dir(&dir, id);
        let operation_lock = self.operation_lock(&locator);
        let _operation = operation_lock.lock().await;
        let active_session = {
            self.active
                .lock()
                .unwrap()
                .get(&locator)
                .map(|session| (session.runtime.clone(), session.lock_path.clone()))
        };
        if let Some((runtime, lock_path)) = active_session {
            runtime.shutdown().await.map_err(RegistryError::Internal)?;
            self.active.lock().unwrap().remove(&locator);
            fs::remove_file(&lock_path).map_err(|error| {
                RegistryError::Internal(anyhow!(
                    "failed to release lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        }
        if !dir.is_dir() {
            return Err(RegistryError::NotFound(format!("session {id} not found")));
        }
        tokio::task::spawn_blocking(move || fs::remove_dir_all(dir))
            .await
            .map_err(|error| RegistryError::Internal(error.into()))?
            .map_err(|error| RegistryError::Internal(error.into()))?;
        Ok(())
    }

    pub fn is_open(&self, id: &str, project: Option<&str>) -> RegistryResult<bool> {
        Ok(self.active_locator(id, project)?.is_some())
    }

    pub fn running(&self, id: &str, project: Option<&str>) -> RegistryResult<bool> {
        let locator = self.active_locator(id, project)?;
        Ok(locator
            .and_then(|locator| {
                self.active
                    .lock()
                    .unwrap()
                    .get(&locator)
                    .map(|a| a.runtime.running())
            })
            .unwrap_or(false))
    }

    pub fn idle_session_ids(&self, minimum_idle: Duration) -> Vec<(String, String)> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, session)| {
                session
                    .runtime
                    .idle_for()
                    .is_some_and(|idle| idle >= minimum_idle)
            })
            .map(|(locator, _)| (locator.id.clone(), locator.project_key.clone()))
            .collect()
    }

    pub async fn shutdown_all(&self) -> Result<()> {
        let ids = self
            .active
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for locator in ids {
            if let Err(error) = self.close(&locator.id, Some(&locator.project_key)).await {
                errors.push(format!("{}: {error:#}", locator.id));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("failed to close sessions: {}", errors.join("; "))
        }
    }

    /// The session's working directory (SessionMetadata.cwd).
    pub async fn session_metadata_cwd(
        &self,
        id: &str,
        project: Option<&str>,
    ) -> Result<Option<String>> {
        let dir = self.session_dir(id, project).await?;
        tokio::task::spawn_blocking(move || {
            let metadata = read_session_metadata(&dir)?;
            if metadata.cwd.trim().is_empty() {
                anyhow::bail!("session metadata cwd is empty in {}", dir.display());
            }
            Ok(Some(metadata.cwd))
        })
        .await?
    }

    /// Find the disk path of a session (full-home scan).
    pub async fn session_dir(&self, id: &str, project: Option<&str>) -> RegistryResult<PathBuf> {
        let home = self.home.clone();
        let scanned = tokio::task::spawn_blocking(move || scan_all_sessions(&home))
            .await
            .map_err(|error| RegistryError::Internal(error.into()))?
            .map_err(RegistryError::Internal)?;
        let candidates = scanned
            .into_iter()
            .filter(|(dir, metadata, _)| {
                metadata.as_ref().is_some_and(|metadata| metadata.id == id)
                    && project.is_none_or(|project| project_key_from_dir(dir) == project)
            })
            .map(|(dir, _, _)| dir)
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => Err(RegistryError::NotFound(format!("session {id} not found"))),
            [dir] => Ok(dir.clone()),
            many => Err(RegistryError::Ambiguous(format!(
                "session {id} is ambiguous; candidates: {}",
                many.iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
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
        options = options.with_project_scoped_sessions().with_log_events(true);
        if let Some(backend) = &self.llm_backend {
            options = options.with_llm_backend(backend.clone());
        }
        options
    }
}

fn summary_from_metadata(
    metadata: Option<SessionMetadata>,
    modified: Option<SystemTime>,
    dir: &Path,
    usage: (u64, u64, u64, u64, u64),
) -> SessionSummary {
    let corrupt = dir.join("session.json").exists() && metadata.is_none();
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
    let (tokens_in, tokens_out, cache_read_tokens, cost_nano_cny, last_context_tokens) = usage;
    SessionSummary {
        project_key: project_key_from_dir(dir),
        corrupt,
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

fn project_key_from_dir(dir: &Path) -> String {
    dir.parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn locator_from_dir(dir: &Path, id: &str) -> SessionLocator {
    SessionLocator {
        project_key: project_key_from_dir(dir),
        id: id.to_string(),
    }
}

fn session_cwd_from_dir(dir: &Path) -> Result<PathBuf> {
    let metadata = read_session_metadata(dir)?;
    let cwd = PathBuf::from(metadata.cwd);
    if cwd.as_os_str().is_empty() {
        anyhow::bail!("session metadata cwd is empty in {}", dir.display());
    }
    Ok(cwd)
}

fn read_session_metadata(dir: &Path) -> Result<SessionMetadata> {
    let path = dir.join("session.json");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        anyhow!(
            "failed to read session metadata {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_str(&text)
        .map_err(|error| anyhow!("corrupt session metadata {}: {error}", path.display()))
}

/// Scan every project under `~/.mink/projects/` for session directories.
/// Returns (session dir, parsed metadata, mtime of events.jsonl).
fn scan_all_sessions(home: &Path) -> Result<Vec<ScannedSession>> {
    let projects = home.join(".mink").join("projects");
    let entries = match std::fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for project in entries {
        let project = project?;
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let sessions = std::fs::read_dir(&project_dir)?;
        for session in sessions {
            let session = session?;
            let dir = session.path();
            if !dir.is_dir() {
                continue;
            }
            let metadata = match std::fs::read_to_string(dir.join("session.json")) {
                Ok(text) => serde_json::from_str::<SessionMetadata>(&text).ok(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            // 活动时间：events.jsonl mtime（更精确），回退到目录 mtime
            let modified = std::fs::metadata(dir.join("events.jsonl"))
                .ok()
                .and_then(|m| m.modified().ok())
                .or_else(|| dir.metadata().ok().and_then(|m| m.modified().ok()));
            out.push((dir, metadata, modified));
        }
    }
    Ok(out)
}

fn write_lock(path: &Path) -> Result<()> {
    let text = format!(
        "{{\"pid\":{},\"taken_at\":{}}}",
        std::process::id(),
        now_secs()
    );
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| anyhow!("failed to acquire lock {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

struct SessionLease {
    path: PathBuf,
    armed: bool,
}

impl SessionLease {
    fn acquire(path: PathBuf) -> Result<Self> {
        if path.exists() {
            if !lock_is_stale(&path) {
                anyhow::bail!("session is locked by another process");
            }
            std::fs::remove_file(&path).map_err(|error| {
                anyhow!("failed to reclaim stale lock {}: {error}", path.display())
            })?;
        }
        write_lock(&path)?;
        Ok(Self { path, armed: true })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
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

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mink-server-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn lock_parse_and_stale_detection() {
        let dir =
            std::env::temp_dir().join(format!("mink-server-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOCK_FILE);
        // Dead pid is never alive in practice.
        std::fs::write(&path, format!("{{\"pid\":{}, \"taken_at\":0}}", u32::MAX)).unwrap();
        assert!(lock_is_stale(&path), "dead pid should be stale");
        std::fs::write(&path, format!("{{\"pid\":{}}}", std::process::id())).unwrap();
        assert!(!lock_is_stale(&path), "own live pid should not be stale");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_open_publishes_one_runtime_and_one_lease() {
        let root = unique_temp_dir("concurrent-open");
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let registry = Arc::new(Registry::new(home, "flash".to_string(), 4));
        let created = registry.create("race", &cwd).await.unwrap();

        let mut tasks = Vec::new();
        for _ in 0..32 {
            let registry = registry.clone();
            let id = created.id.clone();
            let project = created.project_key.clone();
            tasks.push(tokio::spawn(async move {
                registry.open(&id, Some(&project)).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        assert_eq!(registry.active.lock().unwrap().len(), 1);
        let session_dir = registry
            .session_dir(&created.id, Some(&created.project_key))
            .await
            .unwrap();
        assert!(session_dir.join(LOCK_FILE).is_file());

        registry
            .close(&created.id, Some(&created.project_key))
            .await
            .unwrap();
        assert!(!session_dir.join(LOCK_FILE).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_create_reuses_one_project_alias() {
        let root = unique_temp_dir("concurrent-create");
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let registry = Arc::new(Registry::new(home, "flash".to_string(), 4));

        let mut tasks = Vec::new();
        for _ in 0..32 {
            let registry = registry.clone();
            let cwd = cwd.clone();
            tasks.push(tokio::spawn(async move {
                registry.create("same alias", &cwd).await
            }));
        }
        let mut ids = std::collections::BTreeSet::new();
        for task in tasks {
            ids.insert(task.await.unwrap().unwrap().id);
        }

        assert_eq!(ids.len(), 1);
        assert_eq!(registry.list().await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn list_marks_malformed_metadata_without_overwriting_it() {
        let root = unique_temp_dir("corrupt-list");
        let home = root.join("home");
        let session_dir = home
            .join(".mink")
            .join("projects")
            .join("project-key")
            .join("broken");
        std::fs::create_dir_all(&session_dir).unwrap();
        let metadata_path = session_dir.join("session.json");
        std::fs::write(&metadata_path, "{not-json").unwrap();

        let registry = Registry::new(home, "flash".to_string(), 1);
        let sessions = registry.list().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "broken");
        assert!(sessions[0].corrupt);
        assert_eq!(std::fs::read_to_string(metadata_path).unwrap(), "{not-json");

        let _ = std::fs::remove_dir_all(root);
    }
}
