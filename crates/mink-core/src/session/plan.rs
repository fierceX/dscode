use crate::session::atomic_file::atomic_replace;
use crate::session::store::ConversationStore;
use crate::tools::plan::PlanCommand;
use crate::tools::runner::ToolExecution;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const JOURNAL_VERSION: u32 = 1;
const PLAN_TRANSACTION_KEY: &str = "plan_transaction_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanOperation {
    Confirm,
    Clear,
}

impl PlanOperation {
    fn from_command(command: PlanCommand) -> Option<Self> {
        match command {
            PlanCommand::SetDraft => None,
            PlanCommand::Confirm => Some(Self::Confirm),
            PlanCommand::Clear => Some(Self::Clear),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransactionPhase {
    Started,
    Bound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanTransaction {
    id: String,
    operation: PlanOperation,
    phase: TransactionPhase,
    #[serde(with = "base64_bytes")]
    prior_plan: Option<Vec<u8>>,
    #[serde(with = "base64_bytes")]
    prior_draft: Option<Vec<u8>>,
    #[serde(with = "base64_bytes")]
    plan_content: Option<Vec<u8>>,
    tool_use_id: Option<String>,
    tool_result: Option<Value>,
    transition: Option<Value>,
}

/// Serialize plan snapshots as base64 strings instead of serde's default
/// per-byte JSON arrays: markdown plan content routinely reaches kilobytes,
/// and a numeric array inflates the journal severalfold.
mod base64_bytes {
    use base64::Engine as _;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value.as_deref() {
            Some(bytes) => {
                serializer.serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(D::Error::custom)
            })
            .transpose()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanTransactionJournal {
    version: u32,
    transactions: Vec<PlanTransaction>,
}

/// Full request projection, shared by the turn executor and the compactor:
/// single-consumption image lifecycle (§7.3). Plan state is append-only in
/// conversation history, with an active-plan checkpoint supplied by the
/// compaction projection only after history has been compacted.
/// Any token estimate fed to compaction must use this exactly
/// like the real request, or history pictures would be counted as visual
/// tokens after they were already consumed.
pub fn project_full_request(messages: &[serde_json::Value]) -> Result<Vec<serde_json::Value>> {
    Ok(crate::llm::image_projection::project_consumed_attachments(
        messages,
    ))
}

pub struct PlanStore {
    plan_path: PathBuf,
    draft_path: PathBuf,
    journal_path: PathBuf,
    transition_lock: Mutex<()>,
}

impl PlanStore {
    pub fn new(plan_path: PathBuf, draft_path: PathBuf) -> Self {
        let journal_path = plan_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plan-transaction.json");
        Self {
            plan_path,
            draft_path,
            journal_path,
            transition_lock: Mutex::new(()),
        }
    }

    pub fn set_draft(&self, content: &str, max_bytes: usize) -> Result<()> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_no_pending_transaction()?;
        if content.len() > max_bytes {
            bail!(
                "plan draft is too large: {} bytes exceeds limit of {max_bytes} bytes",
                content.len()
            );
        }
        if content.is_empty() {
            return match std::fs::remove_file(&self.draft_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        match self.plan_path.try_exists() {
            Ok(true) => bail!("cannot create a plan draft while a confirmed plan exists"),
            Ok(false) => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot inspect current plan before saving draft: {error}"
                ));
            }
        }
        atomic_replace(&self.draft_path, content.as_bytes())
    }

    pub fn confirm(&self) -> Result<String> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_no_pending_transaction()?;
        let draft = match std::fs::read(&self.draft_path) {
            Ok(draft) => draft,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("no plan draft found to confirm")
            }
            Err(error) => return Err(anyhow::anyhow!("cannot read plan draft: {error}")),
        };
        if draft.iter().all(u8::is_ascii_whitespace) {
            bail!("no plan draft found to confirm");
        }
        let content = String::from_utf8(draft)
            .map_err(|error| anyhow::anyhow!("plan draft is not valid UTF-8: {error}"))?;
        match self.plan_path.try_exists() {
            Ok(true) => bail!("a confirmed plan already exists"),
            Ok(false) => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot inspect current plan before confirmation: {error}"
                ));
            }
        }
        let transaction = PlanTransaction {
            id: next_transaction_id(),
            operation: PlanOperation::Confirm,
            phase: TransactionPhase::Started,
            prior_plan: None,
            prior_draft: Some(content.as_bytes().to_vec()),
            plan_content: Some(content.as_bytes().to_vec()),
            tool_use_id: None,
            tool_result: None,
            transition: None,
        };
        self.push_transaction(&transaction)?;
        ensure_parent(&self.plan_path)?;
        if let Err(error) = std::fs::rename(&self.draft_path, &self.plan_path) {
            let rollback = self.rollback_and_remove(&transaction);
            return match rollback {
                Ok(()) => Err(anyhow::anyhow!(
                    "cannot commit plan draft atomically: {error}"
                )),
                Err(rollback) => Err(anyhow::anyhow!(
                    "cannot commit plan draft atomically: {error}; transaction rollback failed: {rollback:#}"
                )),
            };
        }
        Ok(content)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_no_pending_transaction()?;
        let current = match std::fs::read(&self.plan_path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("no confirmed plan found to clear")
            }
            Err(error) => return Err(anyhow::anyhow!("cannot read current plan: {error}")),
        };
        if current.iter().all(u8::is_ascii_whitespace) {
            bail!("no confirmed plan found to clear");
        }
        let prior_draft = match std::fs::read(&self.draft_path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot read stale plan draft before clearing confirmed plan: {error}"
                ));
            }
        };
        let transaction = PlanTransaction {
            id: next_transaction_id(),
            operation: PlanOperation::Clear,
            phase: TransactionPhase::Started,
            prior_plan: Some(current),
            prior_draft,
            plan_content: None,
            tool_use_id: None,
            tool_result: None,
            transition: None,
        };
        self.push_transaction(&transaction)?;
        if let Err(error) = std::fs::remove_file(&self.draft_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            let rollback = self.rollback_and_remove(&transaction);
            return match rollback {
                Ok(()) => Err(anyhow::anyhow!(
                    "cannot remove stale plan draft before clearing confirmed plan: {error}"
                )),
                Err(rollback) => Err(anyhow::anyhow!(
                    "cannot remove stale plan draft before clearing confirmed plan: {error}; transaction rollback failed: {rollback:#}"
                )),
            };
        }
        if let Err(error) = std::fs::remove_file(&self.plan_path) {
            let rollback = self.rollback_and_remove(&transaction);
            return match rollback {
                Ok(()) => Err(anyhow::anyhow!("cannot clear confirmed plan: {error}")),
                Err(rollback) => Err(anyhow::anyhow!(
                    "cannot clear confirmed plan: {error}; transaction rollback failed: {rollback:#}"
                )),
            };
        }
        Ok(())
    }

    /// Attach the durable conversation records to the most recent filesystem
    /// mutation. Before this point recovery rolls the mutation back; after it,
    /// recovery completes the result + transition append idempotently.
    pub(crate) fn bind_transition(&self, result: &mut ToolExecution) -> Result<()> {
        let Some(operation) = result.plan_command.and_then(PlanOperation::from_command) else {
            return Ok(());
        };
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut journal = self.load_journal()?;
        let transaction = journal
            .transactions
            .iter_mut()
            .rev()
            .find(|transaction| {
                transaction.operation == operation && transaction.phase == TransactionPhase::Started
            })
            .ok_or_else(|| anyhow::anyhow!("missing started plan transaction for {operation:?}"))?;

        let metadata = result.state_metadata.get_or_insert_with(|| json!({}));
        let object = metadata
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("plan tool state metadata must be a JSON object"))?;
        object.insert(
            PLAN_TRANSACTION_KEY.to_string(),
            Value::String(transaction.id.clone()),
        );

        transaction.phase = TransactionPhase::Bound;
        transaction.tool_use_id = Some(result.tool_use_id.clone());
        transaction.tool_result = Some(ConversationStore::tool_results_message(
            std::slice::from_ref(result),
        ));
        transaction.transition = Some(transition_message(
            result
                .plan_command
                .expect("operation came from a plan command"),
            &transaction.id,
        )?);
        self.save_journal(&journal)
    }

    pub(crate) async fn finish_transition(
        &self,
        store: &ConversationStore,
        tool_use_id: &str,
        command: PlanCommand,
    ) -> Result<()> {
        let operation = PlanOperation::from_command(command)
            .ok_or_else(|| anyhow::anyhow!("draft updates do not have plan transitions"))?;
        let transaction = {
            let _guard = self
                .transition_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.load_journal()?
                .transactions
                .into_iter()
                .find(|transaction| {
                    transaction.operation == operation
                        && transaction.phase == TransactionPhase::Bound
                        && transaction.tool_use_id.as_deref() == Some(tool_use_id)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("missing bound plan transaction for tool use {tool_use_id}")
                })?
        };
        self.complete_bound_transaction(store, &transaction).await?;
        self.remove_transaction(&transaction.id)
    }

    /// Recover unfinished cross-file transitions before any model request.
    /// Started records have no durable successful tool result and are rolled
    /// back. Bound records are replayed forward with transaction markers so
    /// repeated recovery never duplicates conversation messages.
    pub async fn recover_pending(&self, store: &ConversationStore) -> Result<()> {
        loop {
            let transaction = {
                let _guard = self
                    .transition_lock
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                self.load_journal()?.transactions.into_iter().next()
            };
            let Some(transaction) = transaction else {
                return Ok(());
            };
            match transaction.phase {
                TransactionPhase::Started => {
                    let _guard = self
                        .transition_lock
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    self.rollback_and_remove(&transaction)?;
                }
                TransactionPhase::Bound => {
                    {
                        let _guard = self
                            .transition_lock
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        self.apply_transaction(&transaction)?;
                    }
                    self.complete_bound_transaction(store, &transaction).await?;
                    self.remove_transaction(&transaction.id)?;
                }
            }
        }
    }

    async fn complete_bound_transaction(
        &self,
        store: &ConversationStore,
        transaction: &PlanTransaction,
    ) -> Result<()> {
        let tool_result = transaction.tool_result.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "bound plan transaction {} lacks tool result",
                transaction.id
            )
        })?;
        let transition = transaction.transition.as_ref().ok_or_else(|| {
            anyhow::anyhow!("bound plan transaction {} lacks transition", transaction.id)
        })?;
        let lines = store.lines_lossy_with_warnings(|_| {}).await?;
        if !has_tool_result_marker(&lines, &transaction.id) {
            store.append_runtime_message(tool_result.clone()).await?;
        }
        // A crashed assistant response may have contained other tool calls.
        // Pair all of them before inserting the user-role plan transition;
        // otherwise the transition could split an OpenAI tool-call/result
        // exchange and make the recovered history invalid.
        store.repair_dangling_tool_uses().await?;
        let lines = store.lines_lossy_with_warnings(|_| {}).await?;
        if !has_transition_marker(&lines, &transaction.id) {
            // The fsync also makes any preceding batched tool result durable
            // before the journal record is removed.
            store
                .append_runtime_message_durable(transition.clone())
                .await?;
        }
        Ok(())
    }

    fn push_transaction(&self, transaction: &PlanTransaction) -> Result<()> {
        let mut journal = self.load_journal()?;
        journal.transactions.push(transaction.clone());
        self.save_journal(&journal)
    }

    fn ensure_no_pending_transaction(&self) -> Result<()> {
        if self.load_journal()?.transactions.is_empty() {
            Ok(())
        } else {
            bail!("an unfinished plan transition must be recovered before another plan mutation")
        }
    }

    fn rollback_and_remove(&self, transaction: &PlanTransaction) -> Result<()> {
        self.rollback_transaction(transaction)?;
        self.remove_transaction_locked(&transaction.id)
    }

    fn remove_transaction(&self, id: &str) -> Result<()> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.remove_transaction_locked(id)
    }

    fn remove_transaction_locked(&self, id: &str) -> Result<()> {
        let mut journal = self.load_journal()?;
        journal
            .transactions
            .retain(|transaction| transaction.id != id);
        self.save_journal(&journal)
    }

    fn load_journal(&self) -> Result<PlanTransactionJournal> {
        let bytes = match std::fs::read(&self.journal_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PlanTransactionJournal {
                    version: JOURNAL_VERSION,
                    transactions: Vec::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let journal: PlanTransactionJournal = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "invalid plan transaction journal {}: {error}",
                self.journal_path.display()
            )
        })?;
        if journal.version != JOURNAL_VERSION {
            bail!(
                "unsupported plan transaction journal version {} in {}",
                journal.version,
                self.journal_path.display()
            );
        }
        Ok(journal)
    }

    fn save_journal(&self, journal: &PlanTransactionJournal) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(journal)?;
        bytes.push(b'\n');
        atomic_replace(&self.journal_path, &bytes)
    }

    fn apply_transaction(&self, transaction: &PlanTransaction) -> Result<()> {
        match transaction.operation {
            PlanOperation::Confirm => {
                let content = transaction.plan_content.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("confirm transaction {} lacks plan content", transaction.id)
                })?;
                ensure_matches_or_missing(&self.plan_path, content, "confirmed plan")?;
                if !self.plan_path.exists() {
                    atomic_replace(&self.plan_path, content)?;
                }
                remove_if_matches(&self.draft_path, Some(content), "plan draft")?;
            }
            PlanOperation::Clear => {
                remove_if_matches(
                    &self.plan_path,
                    transaction.prior_plan.as_deref(),
                    "confirmed plan",
                )?;
                remove_if_matches(
                    &self.draft_path,
                    transaction.prior_draft.as_deref(),
                    "plan draft",
                )?;
            }
        }
        Ok(())
    }

    fn rollback_transaction(&self, transaction: &PlanTransaction) -> Result<()> {
        restore_snapshot(
            &self.plan_path,
            transaction.prior_plan.as_deref(),
            transaction.plan_content.as_deref(),
            "confirmed plan",
        )?;
        restore_snapshot(
            &self.draft_path,
            transaction.prior_draft.as_deref(),
            transaction.plan_content.as_deref(),
            "plan draft",
        )
    }
}

fn transition_message(command: PlanCommand, transaction_id: &str) -> Result<Value> {
    let content = command
        .transition_message()
        .ok_or_else(|| anyhow::anyhow!("draft updates do not have plan transitions"))?;
    Ok(json!({
        "role": "user",
        "content": content,
        "internal": true,
        "_mink": { PLAN_TRANSACTION_KEY: transaction_id },
    }))
}

fn has_tool_result_marker(lines: &[Value], transaction_id: &str) -> bool {
    lines.iter().any(|line| {
        line.get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block
                        .get("_mink")
                        .and_then(|metadata| metadata.get(PLAN_TRANSACTION_KEY))
                        .and_then(Value::as_str)
                        == Some(transaction_id)
            })
    })
}

fn has_transition_marker(lines: &[Value], transaction_id: &str) -> bool {
    lines.iter().any(|line| {
        line.get("_mink")
            .and_then(|metadata| metadata.get(PLAN_TRANSACTION_KEY))
            .and_then(Value::as_str)
            == Some(transaction_id)
    })
}

fn next_transaction_id() -> String {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{}-{sequence}", std::process::id())
}

fn ensure_matches_or_missing(path: &Path, expected: &[u8], label: &str) -> Result<()> {
    match std::fs::read(path) {
        Ok(actual) if actual == expected => Ok(()),
        Ok(_) => bail!(
            "cannot recover {label}: {} differs from the transaction journal",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_if_matches(path: &Path, expected: Option<&[u8]>, label: &str) -> Result<()> {
    match std::fs::read(path) {
        Ok(actual) => {
            let Some(expected) = expected else {
                bail!(
                    "cannot recover {label}: unexpected file exists at {}",
                    path.display()
                );
            };
            if actual != expected {
                bail!(
                    "cannot recover {label}: {} differs from the transaction journal",
                    path.display()
                );
            }
            std::fs::remove_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn restore_snapshot(
    path: &Path,
    previous: Option<&[u8]>,
    transaction_content: Option<&[u8]>,
    label: &str,
) -> Result<()> {
    match previous {
        Some(previous) => {
            if let Ok(actual) = std::fs::read(path)
                && actual != previous
                && transaction_content.is_none_or(|content| actual != content)
            {
                bail!(
                    "cannot roll back {label}: {} differs from the transaction journal",
                    path.display()
                );
            }
            atomic_replace(path, previous)
        }
        None => remove_if_matches(path, transaction_content, label),
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plan path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    Ok(())
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
