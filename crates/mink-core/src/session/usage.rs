use crate::protocol::UsageEvent;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const USAGE_RECORD_VERSION: u32 = 1;
static ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str) -> String {
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{prefix}-{nanos:x}-{sequence:x}")
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    Agent,
    Compaction,
    SubAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Reported,
    Unreported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    pub fn from_provider(value: &UsageEvent) -> Result<Self> {
        fn checked(value: i64, field: &str) -> Result<u64> {
            u64::try_from(value).map_err(|_| anyhow!("negative provider {field}: {value}"))
        }

        Ok(Self {
            input_tokens: checked(value.input_tokens, "input_tokens")?,
            cache_read_tokens: checked(value.cache_read_input_tokens, "cache_read_input_tokens")?,
            cache_creation_tokens: checked(
                value.cache_creation_input_tokens,
                "cache_creation_input_tokens",
            )?,
            output_tokens: checked(value.output_tokens, "output_tokens")?,
        })
    }

    fn add_assign(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub version: u32,
    pub billing_turn_id: String,
    pub request_id: String,
    pub kind: UsageKind,
    pub origin_session_id: String,
    pub model: String,
    pub attempt_count: u32,
    pub status: UsageStatus,
    pub tokens: Option<TokenUsage>,
    /// 兼容字段：费用统计已移除。已上报记录恒写 `0`；未上报记录保持 `None`（历史文件同样可能为 `None` 或旧数值）。
    pub cost_nano_cny: Option<u64>,
    pub reason: Option<String>,
    pub completed_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub request_count: u64,
    pub reported_request_count: u64,
    pub unreported_request_count: u64,
    pub attempt_count: u64,
    pub tokens: TokenUsage,
}

impl UsageSummary {
    pub fn from_records(records: &[UsageRecord]) -> Self {
        let mut summary = Self::default();
        for record in records {
            summary.add_record(record);
        }
        summary
    }

    fn add_record(&mut self, record: &UsageRecord) {
        self.request_count = self.request_count.saturating_add(1);
        self.attempt_count = self
            .attempt_count
            .saturating_add(u64::from(record.attempt_count));
        match record.status {
            UsageStatus::Reported => {
                self.reported_request_count = self.reported_request_count.saturating_add(1);
            }
            UsageStatus::Unreported => {
                self.unreported_request_count = self.unreported_request_count.saturating_add(1);
            }
        }
        if let Some(tokens) = &record.tokens {
            self.tokens.add_assign(tokens);
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageScope {
    pub billing_turn_id: String,
    pub kind: UsageKind,
    pub origin_session_id: String,
}

struct ActiveTurn {
    billing_turn_id: String,
}

pub struct UsageJournal {
    path: PathBuf,
    write_lock: Mutex<()>,
    active_turn: Mutex<Option<ActiveTurn>>,
    summary: Mutex<UsageSummary>,
}

impl UsageJournal {
    pub fn new(path: PathBuf) -> Arc<Self> {
        // 损坏的 usage.jsonl 不再静默把汇总归零：跳过坏行尽力恢复并告警
        //（磁盘记录仍保留；all_records()/records_for() 同样使用 resilient 读取）。
        let summary = match read_records(&path) {
            Ok(records) => UsageSummary::from_records(&records),
            Err(error) => {
                eprintln!(
                    "[mink] Warning: usage journal {} unreadable ({error}); \
recovering salvageable records",
                    path.display()
                );
                let records = read_records_lossy(&path);
                UsageSummary::from_records(&records)
            }
        };
        Arc::new(Self {
            path,
            write_lock: Mutex::new(()),
            active_turn: Mutex::new(None),
            summary: Mutex::new(summary),
        })
    }

    pub fn begin_turn(&self) -> String {
        let billing_turn_id = next_id("turn");
        *self.active_turn.lock().unwrap_or_else(|e| e.into_inner()) = Some(ActiveTurn {
            billing_turn_id: billing_turn_id.clone(),
        });
        billing_turn_id
    }

    pub fn end_turn(&self, billing_turn_id: &str) {
        let mut active = self.active_turn.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|turn| turn.billing_turn_id == billing_turn_id)
        {
            *active = None;
        }
    }

    pub fn scope(&self, kind: UsageKind, origin_session_id: impl Into<String>) -> UsageScope {
        let active = self.active_turn.lock().unwrap_or_else(|e| e.into_inner());
        let billing_turn_id = active
            .as_ref()
            .map(|turn| turn.billing_turn_id.clone())
            .unwrap_or_else(|| next_id("operation"));
        UsageScope {
            billing_turn_id,
            kind,
            origin_session_id: origin_session_id.into(),
        }
    }

    pub fn capture(self: &Arc<Self>, scope: UsageScope, model: impl Into<String>) -> UsageCapture {
        UsageCapture {
            journal: self.clone(),
            scope,
            request_id: next_id("request"),
            model: model.into(),
        }
    }

    pub fn records_for(&self, billing_turn_id: &str) -> Result<Vec<UsageRecord>> {
        read_records_resilient(&self.path).map(|records| {
            records
                .into_iter()
                .filter(|record| record.billing_turn_id == billing_turn_id)
                .collect()
        })
    }

    pub fn all_records(&self) -> Result<Vec<UsageRecord>> {
        read_records_resilient(&self.path)
    }

    pub fn summary(&self) -> UsageSummary {
        self.summary
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn flush(&self) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        match OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file.sync_all().map_err(Into::into),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn append(&self, record: &UsageRecord) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        // 单缓冲区追加（含换行）：序列化/写文件半途失败留下的无换行尾行
        // 由下一次 append 前的修复处理，不会粘坏后续记录。
        crate::session::jsonl::append_line(&self.path, &line, false)?;
        let mut summary = self
            .summary
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        summary.add_record(record);
        Ok(())
    }
}

#[derive(Clone)]
pub struct UsageCapture {
    journal: Arc<UsageJournal>,
    scope: UsageScope,
    request_id: String,
    model: String,
}

impl UsageCapture {
    pub fn reported(&self, usage: &UsageEvent, attempt_count: u32) -> Result<UsageRecord> {
        let tokens = TokenUsage::from_provider(usage)?;
        let record = self.record(
            attempt_count,
            UsageStatus::Reported,
            Some(tokens),
            // 费用统计已移除（上游定价随时间/时段变动，无法本地准确计价）；
            // 为兼容历史 session 文件语义，记录中费用恒写 0。
            Some(0),
            None,
        );
        self.journal.append(&record)?;
        Ok(record)
    }

    pub fn unreported(&self, attempt_count: u32, reason: impl Into<String>) -> Result<UsageRecord> {
        let record = self.record(
            attempt_count,
            UsageStatus::Unreported,
            None,
            // 未上报 usage 的请求不伪造费用：保持 None（与“无 Token”语义一致）。
            None,
            Some(reason.into()),
        );
        self.journal.append(&record)?;
        Ok(record)
    }

    fn record(
        &self,
        attempt_count: u32,
        status: UsageStatus,
        tokens: Option<TokenUsage>,
        cost_nano_cny: Option<u64>,
        reason: Option<String>,
    ) -> UsageRecord {
        UsageRecord {
            version: USAGE_RECORD_VERSION,
            billing_turn_id: self.scope.billing_turn_id.clone(),
            request_id: self.request_id.clone(),
            kind: self.scope.kind,
            origin_session_id: self.scope.origin_session_id.clone(),
            model: self.model.clone(),
            attempt_count,
            status,
            tokens,
            cost_nano_cny,
            reason,
            completed_at: now_rfc3339(),
        }
    }
}

/// 逐行恢复可解析记录，跳过损坏行（供启动期汇总兜底）。
fn read_records_lossy(path: &Path) -> Vec<UsageRecord> {
    let Ok(data) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    crate::session::jsonl::parse_lossy_lines(path, &data, &mut |_| {})
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect()
}

/// 读取 usage 记录并跳过损坏行，但保留 I/O 错误。
///
/// 用于只读会话发现/列表等“单个会话损坏不应拖垮整体”的场景；需要严格
/// 失败语义的写入路径仍应使用 [`read_records`]。
pub(crate) fn read_records_resilient(path: &Path) -> Result<Vec<UsageRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(path)?;
    let mut records = Vec::new();
    for line in data.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str(line) {
            records.push(record);
        }
    }
    Ok(records)
}

pub(crate) fn read_records(path: &Path) -> Result<Vec<UsageRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(path)?;
    let mut records = Vec::new();
    let lines: Vec<&str> = data.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) if index + 1 == lines.len() && !data.ends_with('\n') => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(records)
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
