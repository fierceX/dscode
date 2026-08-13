use crate::config::ModelTier;
use crate::protocol::UsageEvent;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
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
    pub cost_nano_cny: u64,
}

impl UsageSummary {
    pub fn from_records(records: &[UsageRecord]) -> Self {
        let mut summary = Self::default();
        for record in records {
            summary.request_count = summary.request_count.saturating_add(1);
            summary.attempt_count = summary
                .attempt_count
                .saturating_add(u64::from(record.attempt_count));
            match record.status {
                UsageStatus::Reported => {
                    summary.reported_request_count =
                        summary.reported_request_count.saturating_add(1);
                }
                UsageStatus::Unreported => {
                    summary.unreported_request_count =
                        summary.unreported_request_count.saturating_add(1);
                }
            }
            if let Some(tokens) = &record.tokens {
                summary.tokens.add_assign(tokens);
            }
            summary.cost_nano_cny = summary
                .cost_nano_cny
                .saturating_add(record.cost_nano_cny.unwrap_or_default());
        }
        summary
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
}

impl UsageJournal {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            write_lock: Mutex::new(()),
            active_turn: Mutex::new(None),
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
        read_records(&self.path).map(|records| {
            records
                .into_iter()
                .filter(|record| record.billing_turn_id == billing_turn_id)
                .collect()
        })
    }

    pub fn all_records(&self) -> Result<Vec<UsageRecord>> {
        read_records(&self.path)
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
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, record)?;
        writeln!(file)?;
        file.flush()?;
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
            Some(tokens.clone()),
            Some(price_usage(&self.model, &tokens)?),
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

fn read_records(path: &Path) -> Result<Vec<UsageRecord>> {
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

fn price_usage(model: &str, tokens: &TokenUsage) -> Result<u64> {
    let Ok(tier) = ModelTier::parse(model) else {
        return Ok(0);
    };
    let input_nano = (tier.price_input_per_m() * 1000.0).round() as u64;
    let output_nano = (tier.price_output_per_m() * 1000.0).round() as u64;
    let cache_read_nano = (tier.price_cache_read_per_m() * 1000.0).round() as u64;
    tokens
        .input_tokens
        .checked_mul(input_nano)
        .and_then(|value| {
            tokens
                .cache_creation_tokens
                .checked_mul(input_nano)
                .and_then(|cache| value.checked_add(cache))
        })
        .and_then(|value| {
            tokens
                .cache_read_tokens
                .checked_mul(cache_read_nano)
                .and_then(|cache| value.checked_add(cache))
        })
        .and_then(|value| {
            tokens
                .output_tokens
                .checked_mul(output_nano)
                .and_then(|output| value.checked_add(output))
        })
        .ok_or_else(|| anyhow!("usage cost overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mink-usage-{name}-{}-{}.jsonl",
            std::process::id(),
            next_id("test")
        ))
    }

    #[test]
    fn journal_filters_and_summarizes_turn_records() {
        let path = temp_path("summary");
        let journal = UsageJournal::new(path.clone());
        let turn = journal.begin_turn();
        let capture = journal.capture(
            journal.scope(UsageKind::Agent, "session-1"),
            "deepseek-v4-flash",
        );
        capture
            .reported(
                &UsageEvent {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_read_input_tokens: 40,
                    cache_creation_input_tokens: 0,
                },
                2,
            )
            .unwrap();

        let records = journal.records_for(&turn).unwrap();
        let summary = UsageSummary::from_records(&records);
        assert_eq!(records.len(), 1);
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.attempt_count, 2);
        assert_eq!(summary.tokens.input_tokens, 100);
        assert_eq!(summary.cost_nano_cny, 140_800);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unreported_record_does_not_fabricate_zero_tokens() {
        let path = temp_path("unreported");
        let journal = UsageJournal::new(path.clone());
        journal.begin_turn();
        journal
            .capture(
                journal.scope(UsageKind::Compaction, "session-1"),
                "deepseek-v4-flash",
            )
            .unreported(1, "provider_usage_missing")
            .unwrap();

        let records = journal.all_records().unwrap();
        assert_eq!(records[0].status, UsageStatus::Unreported);
        assert!(records[0].tokens.is_none());
        assert!(records[0].cost_nano_cny.is_none());
        let _ = std::fs::remove_file(path);
    }
}
