use crate::config::ResolvedConfig as Config;
use crate::llm::client::{LlmBackend, LlmModelTarget, LlmPurpose, LlmRequest, MeteredStream};
use crate::protocol::{ErrorEvent, Event, StopEvent, TextEvent, UsageEvent};
use crate::session::compaction_input;
use crate::session::event_log::EventLogWriter;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::session::usage::{UsageJournal, UsageKind};
use crate::ui::Display;
use anyhow::{Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CompactionState {
    active_start: usize,
    summary: String,
}

pub struct CompactionEngine {
    store: Arc<ConversationStore>,
    summary_path: PathBuf,
    state_path: PathBuf,
    api_url: String,
    api_key: String,
    llm_backend: Arc<dyn LlmBackend>,
    config: Config,
    stats: Arc<StatsTracker>,
    usage: Arc<UsageJournal>,
    session_id: String,
    display: Arc<dyn Display>,
    cancel: crate::cancel::CancellationToken,
    interrupt: Arc<AtomicBool>,
    state: RwLock<std::result::Result<CompactionState, String>>,
    compact_lock: tokio::sync::Mutex<()>,
    memo_epoch: Arc<AtomicU64>,
    projection_dirty: AtomicBool,
    event_log_writer: Option<EventLogWriter>,
}

impl CompactionEngine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<ConversationStore>,
        summary_path: PathBuf,
        api_url: String,
        config: &Config,
        stats: Arc<StatsTracker>,
        usage: Arc<UsageJournal>,
        session_id: String,
        display: Arc<dyn Display>,
        cancel: crate::cancel::CancellationToken,
        interrupt: Arc<AtomicBool>,
        llm_backend: Arc<dyn LlmBackend>,
        event_log_writer: Option<EventLogWriter>,
    ) -> Result<Self> {
        let state_path = summary_path.with_file_name("context-state.json");
        let state = load_state(&state_path)?;
        let expected_projection = format!("{}\n", state.summary);
        let projection_matches = std::fs::read(&summary_path)
            .is_ok_and(|content| content == expected_projection.as_bytes());
        if !projection_matches {
            crate::session::atomic_file::atomic_replace(
                &summary_path,
                expected_projection.as_bytes(),
            )?;
        }
        Ok(Self {
            store,
            summary_path,
            state_path,
            api_url,
            api_key: config.api_key.clone(),
            llm_backend,
            config: config.clone(),
            stats,
            usage,
            session_id,
            display,
            cancel,
            interrupt,
            state: RwLock::new(Ok(state)),
            compact_lock: tokio::sync::Mutex::new(()),
            memo_epoch: Arc::new(AtomicU64::new(0)),
            projection_dirty: AtomicBool::new(false),
            event_log_writer,
        })
    }

    /// Shared epoch counter for read memos: any committed compaction invalidates
    /// all in-session read memos (the model's context no longer holds the
    /// previously read content, so "reuse" responses would be misleading).
    pub fn memo_epoch(&self) -> Arc<AtomicU64> {
        self.memo_epoch.clone()
    }

    pub fn current_summary(&self) -> Result<Option<String>> {
        let state = self.current_state()?;
        Ok((!state.summary.trim().is_empty()).then_some(state.summary))
    }

    pub async fn validate_startup(&self) -> Result<()> {
        let state = self.current_state()?;
        if state.active_start == 0 {
            return Ok(());
        }
        let active = self.store.lines_from(state.active_start).await?;
        if active.is_empty() {
            return Ok(());
        }
        if active.first().is_some_and(is_safe_context_start) {
            return Ok(());
        }
        // Older builds could persist a cut on a runtime-injected user message.
        // Repair by moving the boundary forward to the next safe start (or to
        // an empty active tail) instead of refusing to open the session.
        let repaired_relative = (0..active.len())
            .find(|&index| is_safe_context_start(&active[index]))
            .unwrap_or(active.len());
        let repaired_start = state.active_start.saturating_add(repaired_relative);
        let next = CompactionState {
            active_start: repaired_start,
            summary: state.summary,
        };
        self.commit_state(next).await?;
        Ok(())
    }

    pub async fn read_summary(&self) -> Option<String> {
        self.current_summary()
            .ok()
            .flatten()
            .map(|summary| strip_dsml_tags(summary.trim()))
    }

    pub async fn active_messages(&self) -> Result<Vec<Value>> {
        let state = self.current_state()?;
        let mut messages = self.store.lines_from(state.active_start).await?;
        self.prepend_dynamic_summary(&mut messages, &state.summary);
        Ok(messages)
    }

    pub async fn evaluate_and_compact(
        &self,
        trigger: &str,
        context_tokens: usize,
        target: LlmModelTarget<'_>,
    ) -> Result<(bool, String)> {
        let _guard = self.compact_lock.lock().await;
        if self.config.max_context_tokens == 0 && matches!(trigger, "auto" | "preflight") {
            return Ok((false, "automatic compaction disabled".into()));
        }
        if !is_forced_trigger(trigger) && context_tokens < compaction_trigger_tokens(&self.config) {
            return Ok((false, "below threshold".into()));
        }

        let state = self.current_state()?;
        let active = self.store.lines_from(state.active_start).await?;
        if active.is_empty() {
            return Ok((false, "empty".into()));
        }

        let tail_target = self.config.context_compact_tail_tokens.max(1);
        let cut = find_compaction_cut_point(&active, tail_target);
        if cut == 0 {
            return Ok((false, "no safe boundary".into()));
        }

        let dropped = &active[..cut];
        let kept = &active[cut..];
        let total_tokens = estimate_messages_tokens(&active);
        let kept_tokens = estimate_messages_tokens(kept);
        let saved_tokens = total_tokens.saturating_sub(kept_tokens);
        if total_tokens == 0 || (saved_tokens as u128) * 10 < total_tokens as u128 {
            return Ok((false, "savings too small".into()));
        }

        let summary = self
            .run_summary_call(
                dropped,
                (!state.summary.is_empty()).then_some(state.summary.as_str()),
                target,
            )
            .await?;
        if self.interrupt.load(Ordering::SeqCst) {
            bail!("compaction interrupted");
        }

        self.validate_conversation_messages(kept, target.model)?;
        let next = CompactionState {
            active_start: state.active_start + cut,
            summary: summary.clone(),
        };
        self.commit_state(next).await?;

        Ok((
            true,
            format!(
                "compacted_at_trigger={trigger}_kept={}_input_reduction={}",
                kept.len(),
                self.config.context_compact_input_reduction
            ),
        ))
    }

    fn prepend_dynamic_summary(&self, messages: &mut Vec<Value>, summary: &str) {
        if !summary.trim().is_empty() {
            messages.insert(
                0,
                json!({
                    "role": "system",
                    "content": format!(
                        "<context-snapshot>\n{}\n</context-snapshot>",
                        summary.trim()
                    )
                }),
            );
        }
    }

    async fn commit_state(&self, state: CompactionState) -> Result<()> {
        let data = serde_json::to_vec_pretty(&state)?;
        let state_path = self.state_path.clone();
        tokio::task::spawn_blocking(move || {
            crate::session::atomic_file::atomic_replace(&state_path, &data)
        })
        .await??;
        *self
            .state
            .write()
            .map_err(|_| anyhow::anyhow!("context state lock poisoned"))? = Ok(state.clone());
        self.store.prune_cache_before(state.active_start).await;
        let summary_path = self.summary_path.clone();
        let summary = format!("{}\n", state.summary);
        let projection = tokio::task::spawn_blocking(move || {
            crate::session::atomic_file::atomic_replace(&summary_path, summary.as_bytes())
        })
        .await;
        self.projection_dirty
            .store(!matches!(projection, Ok(Ok(()))), Ordering::SeqCst);
        self.memo_epoch.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn flush_projection(&self) -> Result<()> {
        if !self.projection_dirty.load(Ordering::SeqCst) {
            return Ok(());
        }
        let state = self.current_state()?;
        let summary_path = self.summary_path.clone();
        let summary = format!("{}\n", state.summary);
        tokio::task::spawn_blocking(move || {
            crate::session::atomic_file::atomic_replace(&summary_path, summary.as_bytes())
        })
        .await??;
        self.projection_dirty.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn current_state(&self) -> Result<CompactionState> {
        self.state
            .read()
            .map_err(|_| anyhow::anyhow!("context state lock poisoned"))?
            .clone()
            .map_err(anyhow::Error::msg)
    }

    fn validate_conversation_messages(&self, messages: &[Value], model: &str) -> Result<()> {
        let _ = crate::llm::transport::build_openai_body(
            model,
            messages,
            &[],
            "",
            effective_max_tokens(&self.config),
        )?;
        Ok(())
    }

    async fn run_summary_call(
        &self,
        dropped: &[Value],
        previous_summary: Option<&str>,
        target: LlmModelTarget<'_>,
    ) -> Result<String> {
        let request_cancel = self.cancel.linked_child_token();
        let watcher_cancel = request_cancel.clone();
        let cleanup_cancel = request_cancel.clone();
        let interrupt = self.interrupt.clone();
        let watcher = tokio::spawn(async move {
            loop {
                if interrupt.load(Ordering::SeqCst) {
                    watcher_cancel.cancel();
                    return;
                }
                tokio::select! {
                    _ = watcher_cancel.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
                }
            }
        });
        let result = self
            .run_summary_call_with_cancel(dropped, previous_summary, target, request_cancel)
            .await;
        watcher.abort();
        cleanup_cancel.cancel();
        if self.interrupt.load(Ordering::SeqCst) {
            bail!("compaction interrupted");
        }
        result
    }

    async fn run_summary_call_with_cancel(
        &self,
        dropped: &[Value],
        previous_summary: Option<&str>,
        target: LlmModelTarget<'_>,
        request_cancel: crate::cancel::CancellationToken,
    ) -> Result<String> {
        let previous = serde_json::to_string(&previous_summary)?;
        let instruction = format!(
            "Merge the conversation turns above with the previous context snapshot below.\n\
             Preserve current facts, decisions, constraints, progress, and blockers.\n\
             Previous context snapshot: {previous}\n\n\
             Output these seven non-empty fields:\n\
             Task focus:\nLatest request:\nProgress:\nErrors:\nDecisions:\nTool evidence:\nReflections:\n\
             Write (none) for any field without content.\n\
             Start directly with Task focus: and do not use code fences."
        );
        let mut messages = if self.config.context_compact_input_reduction {
            vec![json!({
                "role": "user",
                "content": compaction_input::reduce_for_summary(dropped),
            })]
        } else {
            // Summary requests never carry pixels, even with input reduction
            // disabled: the compaction path bypasses stream_backend, so any
            // tool_attachment reference would be sent verbatim to the
            // provider (v7 §10.3). Degrade attachments to text markers.
            let mut messages = dropped.to_vec();
            degrade_images_for_summary(&mut messages);
            messages
        };
        messages.push(json!({"role":"user","content":instruction}));

        let system_prompt = "Summarize coding-agent history for a later model. Preserve user goals, constraints, decisions, progress, blockers, file changes, commands, errors, pending work, and exact identifiers. Do not continue the task.".to_string();

        if self.config.max_context_tokens > 0 {
            let input_tokens = crate::llm::transport::estimate_openai_context_tokens(
                &messages,
                &[],
                &system_prompt,
            )?;
            let input_limit = self.config.max_context_tokens.saturating_sub(
                usize::try_from(compaction_max_output_tokens(&self.config)).unwrap_or(0),
            );
            if input_tokens > input_limit {
                bail!(
                    "compaction summary input exceeds configured budget: {input_tokens} > {input_limit} tokens"
                );
            }
        }

        let capture = self.usage.capture(
            self.usage
                .scope(UsageKind::Compaction, self.session_id.clone()),
            target.model.to_string(),
        );
        let request = self.llm_backend.stream(LlmRequest {
            purpose: LlmPurpose::Compaction,
            model: target.model.to_string(),
            model_alias: target.alias.map(str::to_string),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            system_prompt,
            messages,
            tools: Vec::new(),
            max_tokens: compaction_max_output_tokens(&self.config),
            cancel: request_cancel.clone(),
            verbose: self.config.verbose,
            display: self.display.clone(),
        });
        tokio::pin!(request);
        let response = match tokio::select! {
            response = &mut request => response,
            _ = request_cancel.cancelled() => bail!("compaction interrupted"),
        } {
            Ok(response) => response,
            Err(error) => {
                let attempts = crate::llm::client::request_failure_attempt_count(&error);
                if let Err(record_error) =
                    capture.unreported(attempts, format!("request_failed: {error}"))
                {
                    self.display.render_error(&format!(
                        "Failed to record compaction usage failure: {record_error}"
                    ));
                }
                return Err(error);
            }
        };

        let mut stream = MeteredStream::new(response.events, capture, response.attempt_count);
        let mut output = String::new();
        let mut stop_reason = String::new();
        let mut last_error = String::new();
        loop {
            let event = tokio::select! {
                event = stream.next() => event,
                _ = request_cancel.cancelled() => bail!("compaction interrupted"),
            };
            let Some(event) = event else { break };
            match event? {
                Event::Text(TextEvent { content }) => output.push_str(&content),
                Event::Usage(usage) => {
                    self.log_compact_event(&usage);
                    self.stats.record_compact(&usage).await;
                }
                Event::UsageUnavailable => {}
                Event::Error(ErrorEvent { message }) => last_error = message,
                Event::Stop(StopEvent { reason }) => stop_reason = reason,
                _ => {}
            }
        }
        if !last_error.is_empty() {
            bail!("failed to generate context summary: {last_error}");
        }
        if !matches!(stop_reason.as_str(), "stop" | "end_turn") {
            bail!("failed to generate context summary: invalid stop reason {stop_reason:?}");
        }
        let summary = strip_dsml_tags(&output);
        if summary.trim().is_empty() {
            bail!("failed to generate context summary: empty response");
        }
        Ok(summary.trim().to_string())
    }

    fn log_compact_event(&self, usage: &UsageEvent) {
        if !self.config.log_events {
            return;
        }
        let event = crate::events::EventLog::usage(usage, "compact");
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        if let Some(writer) = &self.event_log_writer {
            writer.send(line);
        }
    }
}

pub fn effective_max_tokens(config: &Config) -> i32 {
    if config.max_context_tokens == 0 {
        return config.max_tokens;
    }
    let reserve = config.context_reserve_tokens.max(1);
    let requested = usize::try_from(config.max_tokens.max(1)).unwrap_or(1);
    i32::try_from(requested.min(reserve)).unwrap_or(i32::MAX)
}

pub fn compaction_max_output_tokens(config: &Config) -> i32 {
    config.context_compact_max_output_tokens.max(1)
}

pub fn request_input_limit(config: &Config) -> usize {
    if config.max_context_tokens == 0 {
        return usize::MAX;
    }
    config
        .max_context_tokens
        .saturating_sub(usize::try_from(effective_max_tokens(config)).unwrap_or(0))
}

fn load_state(path: &Path) -> Result<CompactionState> {
    if !path.exists() {
        return Ok(CompactionState::default());
    }
    let data = std::fs::read(path)?;
    if data.iter().all(u8::is_ascii_whitespace) {
        return Ok(CompactionState::default());
    }
    Ok(serde_json::from_slice(&data)?)
}

fn is_forced_trigger(trigger: &str) -> bool {
    matches!(
        trigger,
        "manual" | "preflight" | "overflow" | "plan_clear" | "plan_confirm"
    )
}

fn compaction_trigger_tokens(config: &Config) -> usize {
    let percentage = ((config.max_context_tokens as u128
        * u128::from(config.context_compact_pct.clamp(1, 100)))
        / 100)
        .min(usize::MAX as u128) as usize;
    percentage.min(
        config
            .max_context_tokens
            .saturating_sub(config.context_reserve_tokens),
    )
}

fn estimate_messages_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|message| {
            serde_json::to_vec(message)
                .map_or(0, |value| value.len())
                .div_ceil(3)
                .max(1)
        })
        .fold(0, |total, tokens| total.saturating_add(tokens))
}

/// Minimum number of real user messages that must survive a compaction in the
/// active tail (preferred over the pure token budget, bounded by history size).
const COMPACTION_MIN_TAIL_USER_MESSAGES: usize = 2;

fn find_compaction_cut_point(messages: &[Value], tail_target: usize) -> usize {
    if messages.len() < 2 {
        return 0;
    }
    let mut tokens = 0usize;
    let mut candidate = messages.len() - 1;
    for index in (0..messages.len()).rev() {
        tokens = tokens.saturating_add(estimate_messages_tokens(&messages[index..=index]));
        candidate = index;
        if tokens >= tail_target {
            break;
        }
    }
    let safe = (1..=candidate)
        .rev()
        .find(|&index| is_safe_context_start(&messages[index]))
        .unwrap_or(0);

    // Keep at least COMPACTION_MIN_TAIL_USER_MESSAGES real user messages in the
    // tail so user constraints do not silently fall behind the cut point.
    let total_users = messages.iter().filter(|m| is_real_user_message(m)).count();
    if total_users < COMPACTION_MIN_TAIL_USER_MESSAGES {
        return safe;
    }
    let mut cut = safe;
    let mut users_seen = messages[cut..]
        .iter()
        .filter(|m| is_real_user_message(m))
        .count();
    while users_seen < COMPACTION_MIN_TAIL_USER_MESSAGES && cut > 0 {
        cut -= 1;
        if is_real_user_message(&messages[cut]) {
            users_seen += 1;
        }
    }
    if cut == 0 {
        // Not enough history to satisfy the guard; keep the token-based cut.
        return safe;
    }
    // `cut` 由 safe start 或真实 user 消息得出，必然是安全边界；
    // 前向对齐循环不可达，以断言钉住该不变式。
    debug_assert!(
        cut >= messages.len() || is_safe_context_start(&messages[cut]),
        "compaction cut point must land on a safe context start"
    );
    cut
}

/// Engine-injected user-role messages that must not count as real user
/// constraints for the compaction guard. The primary signal is the `internal`
/// (or `_mink`) metadata flag set by `add_runtime_user` / todo sync; the
/// string markers below remain as a defensive fallback for historical or
/// third-party messages that predate the flag.
const RUNTIME_INJECTED_MARKERS: &[&str] = &[
    "<todo-progress-reminder>",
    "<todo-final-reminder>",
    "<todo-sync",
    "[System note:",
];

fn is_real_user_message(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if message.get("internal").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if message.get("_mink").is_some() {
        return false;
    }
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        // Array content (tool results / attachments) is never a real user
        // message and never a safe compaction start (v7 §10.1).
        return false;
    };
    !RUNTIME_INJECTED_MARKERS
        .iter()
        .any(|marker| content.starts_with(marker))
}

/// Replace every `tool_attachment` block with a text marker so a summary
/// request never carries image references or pixels (v7 §10.3).
fn degrade_images_for_summary(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks.iter_mut() {
            if block.get("type").and_then(Value::as_str) != Some("tool_attachment") {
                continue;
            }
            let url = block.get("url").and_then(Value::as_str).unwrap_or("?");
            let format = block.get("format").and_then(Value::as_str).unwrap_or("?");
            let width = block.get("width").and_then(Value::as_u64).unwrap_or(0);
            let height = block.get("height").and_then(Value::as_u64).unwrap_or(0);
            *block = serde_json::json!({
                "type": "text",
                "text": format!("[image {format} {width}x{height}: {url}]")
            });
        }
    }
}

fn is_safe_context_start(message: &Value) -> bool {
    // Runtime-injected user messages are not safe boundaries: a cut may never
    // land on an internal prompt that the caller did not author.
    message.get("role").and_then(Value::as_str) == Some("assistant")
        || is_real_user_message(message)
}

fn strip_dsml_tags(text: &str) -> String {
    let regex = regex::Regex::new(r"</?ds_\w+[^>]*>").unwrap();
    regex.replace_all(text, "").into_owned()
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
