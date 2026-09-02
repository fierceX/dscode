use crate::config::ResolvedConfig as Config;
use crate::llm::client::{
    LlmBackend, LlmCacheProjection, LlmModelTarget, LlmPurpose, LlmRequest, MeteredStream,
};
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
use std::sync::{Arc, Mutex, RwLock};

const COMPACTION_INSTRUCTION: &str = "Merge the conversation history above into one checkpoint. Preserve current user goals, constraints, decisions, progress, blockers, file changes, commands, errors, pending work, and exact identifiers. An earlier <compacted-summary>, if present, is established background and must be merged with the newer history. Output these seven non-empty fields: Task focus:, Latest request:, Progress:, Errors:, Decisions:, Tool evidence:, Reflections:. Write (none) for any field without content. Start directly with Task focus:, do not use code fences, do not continue the task, and do not call tools.";

const FALLBACK_SYSTEM_PROMPT: &str = "Summarize coding-agent history for a later model. Preserve user goals, constraints, decisions, progress, blockers, file changes, commands, errors, pending work, and exact identifiers. Do not continue the task.";

#[derive(Debug, Clone)]
struct LatestAgentRequest {
    model: String,
    source_fingerprint: String,
    local_tokens: usize,
    projection_generation: u64,
    backend_name: String,
    source_system_prompt: String,
    source_tools: Vec<Value>,
    projection: Option<CacheProjectionSnapshot>,
}

#[derive(Debug, Clone)]
struct PromptUsageBaseline {
    request: Arc<LatestAgentRequest>,
    provider_prompt_tokens: usize,
}

#[derive(Debug, Default)]
struct PromptUsageState {
    latest_request: Option<Arc<LatestAgentRequest>>,
    baseline: Option<PromptUsageBaseline>,
}

/// Long-lived provider projection metadata. Message bodies are represented by
/// hashes so request-time image data URLs can never survive across requests.
#[derive(Debug, Clone)]
struct CacheProjectionSnapshot {
    model: String,
    system_prompt: String,
    tools: Vec<Value>,
    message_hashes: Vec<[u8; 32]>,
}

#[derive(Debug)]
struct PressureDecision {
    source: &'static str,
    effective_tokens: usize,
    provider_baseline_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
struct SummaryInputMeta {
    input_mode: &'static str,
    aligned_messages: usize,
    aligned_estimated_tokens: usize,
    reduced_suffix_messages: usize,
    fallback_reason: Option<String>,
}

struct SummaryRequestInput {
    system_prompt: String,
    tools: Vec<Value>,
    messages: Vec<Value>,
    meta: SummaryInputMeta,
}

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
    projection_generation: AtomicU64,
    prompt_usage: Mutex<PromptUsageState>,
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
            projection_generation: AtomicU64::new(0),
            prompt_usage: Mutex::new(PromptUsageState::default()),
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_agent_request(
        &self,
        model: &str,
        source_fingerprint: &str,
        local_tokens: usize,
        backend_name: String,
        source_system_prompt: String,
        source_tools: Vec<Value>,
        projection: Option<LlmCacheProjection>,
    ) {
        let request = Arc::new(LatestAgentRequest {
            model: model.to_string(),
            source_fingerprint: source_fingerprint.to_string(),
            local_tokens,
            projection_generation: self.projection_generation.load(Ordering::SeqCst),
            backend_name,
            source_system_prompt,
            source_tools,
            projection: projection.map(CacheProjectionSnapshot::from_projection),
        });
        self.prompt_usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_request = Some(request);
    }

    pub(crate) fn record_agent_usage(&self, usage: &UsageEvent) {
        if usage.input_tokens < 0
            || usage.cache_read_input_tokens < 0
            || usage.cache_creation_input_tokens < 0
        {
            return;
        }
        let Some(provider_prompt_tokens) = usage
            .input_tokens
            .checked_add(usage.cache_read_input_tokens)
            .and_then(|value| value.checked_add(usage.cache_creation_input_tokens))
            .and_then(|value| usize::try_from(value).ok())
        else {
            return;
        };
        let mut state = self
            .prompt_usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(request) = state.latest_request.clone() else {
            return;
        };
        if request.projection_generation != self.projection_generation.load(Ordering::SeqCst) {
            return;
        }
        state.baseline = Some(PromptUsageBaseline {
            request,
            provider_prompt_tokens,
        });
    }

    pub(crate) fn clear_prompt_usage(&self) {
        *self
            .prompt_usage
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = PromptUsageState::default();
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
        self.prepend_dynamic_checkpoints(&mut messages, &state)?;
        Ok(messages)
    }

    fn prepend_dynamic_checkpoints(
        &self,
        messages: &mut Vec<Value>,
        state: &CompactionState,
    ) -> Result<()> {
        if state.active_start == 0 {
            return Ok(());
        }
        let mut checkpoints = Vec::new();
        if !state.summary.trim().is_empty() {
            checkpoints.push(compacted_summary_message(&state.summary));
        }
        if let Some(plan) = read_active_plan_checkpoint(&self.summary_path)? {
            checkpoints.push(plan);
        }
        checkpoints.append(messages);
        *messages = checkpoints;
        Ok(())
    }

    pub async fn evaluate_and_compact(
        &self,
        trigger: &str,
        context_tokens: usize,
        target: LlmModelTarget<'_>,
    ) -> Result<(bool, String)> {
        self.evaluate_and_compact_with_prefix(trigger, context_tokens, target, None, None)
            .await
    }

    pub(crate) async fn evaluate_and_compact_with_prefix(
        &self,
        trigger: &str,
        context_tokens: usize,
        target: LlmModelTarget<'_>,
        source_fingerprint: Option<&str>,
        current_projection: Option<&LlmCacheProjection>,
    ) -> Result<(bool, String)> {
        let _guard = self.compact_lock.lock().await;
        if self.config.max_context_tokens == 0 && matches!(trigger, "auto" | "preflight") {
            return Ok((false, "automatic compaction disabled".into()));
        }
        let pressure = self.pressure_decision(
            trigger,
            context_tokens,
            target.model,
            source_fingerprint,
            current_projection,
        );
        let threshold_tokens = compaction_trigger_tokens(&self.config);
        self.log_compaction_check(trigger, &pressure, context_tokens, threshold_tokens);
        if !is_forced_trigger(trigger) && pressure.effective_tokens < threshold_tokens {
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

        let kept = &active[cut..];
        let total_tokens = estimate_messages_tokens(&active);
        let kept_tokens = estimate_messages_tokens(kept);
        let saved_tokens = total_tokens.saturating_sub(kept_tokens);
        if total_tokens == 0 || (saved_tokens as u128) * 10 < total_tokens as u128 {
            return Ok((false, "savings too small".into()));
        }

        let (summary, summary_meta) = self
            .run_summary_call(
                &active,
                cut,
                (!state.summary.is_empty()).then_some(state.summary.as_str()),
                state.active_start > 0,
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
        let result = format!(
            "compacted_at_trigger={trigger}_kept={}_input_reduction={}_input_mode={}_aligned_messages={}_aligned_estimated_tokens={}_reduced_suffix_messages={}_fallback_reason={}",
            kept.len(),
            self.config.context_compact_input_reduction,
            summary_meta.input_mode,
            summary_meta.aligned_messages,
            summary_meta.aligned_estimated_tokens,
            summary_meta.reduced_suffix_messages,
            summary_meta.fallback_reason.as_deref().unwrap_or("none"),
        );
        if self.config.log_events {
            self.write_event(crate::events::EventLog::Compact {
                version: Some(2),
                trigger: trigger.to_string(),
                result: result.clone(),
            });
        }
        Ok((true, result))
    }

    fn pressure_decision(
        &self,
        trigger: &str,
        local_tokens: usize,
        model: &str,
        source_fingerprint: Option<&str>,
        current_projection: Option<&LlmCacheProjection>,
    ) -> PressureDecision {
        if trigger == "preflight" {
            return PressureDecision {
                source: "local_preflight",
                effective_tokens: local_tokens,
                provider_baseline_tokens: None,
            };
        }
        if trigger != "auto" {
            return PressureDecision {
                source: "local_fallback",
                effective_tokens: local_tokens,
                provider_baseline_tokens: None,
            };
        }
        let state = self
            .prompt_usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let generation = self.projection_generation.load(Ordering::SeqCst);
        let Some(baseline) = state.baseline.as_ref().filter(|baseline| {
            baseline.request.model == model
                && source_fingerprint
                    .is_some_and(|fingerprint| fingerprint == baseline.request.source_fingerprint)
                && baseline.request.projection_generation == generation
                && self.llm_backend.prompt_usage_calibration_safe()
                && baseline.request.projection.as_ref().is_none_or(|previous| {
                    current_projection
                        .is_some_and(|current| provider_projection_extends(previous, current))
                })
        }) else {
            return PressureDecision {
                source: "local_fallback",
                effective_tokens: local_tokens,
                provider_baseline_tokens: None,
            };
        };
        let calibrated = (baseline.provider_prompt_tokens as i128) + (local_tokens as i128)
            - (baseline.request.local_tokens as i128);
        PressureDecision {
            source: "provider_calibrated",
            effective_tokens: usize::try_from(calibrated.max(0)).unwrap_or(usize::MAX),
            provider_baseline_tokens: Some(baseline.provider_prompt_tokens),
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
        self.projection_generation.fetch_add(1, Ordering::SeqCst);
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
        active: &[Value],
        cut: usize,
        previous_summary: Option<&str>,
        history_already_compacted: bool,
        target: LlmModelTarget<'_>,
    ) -> Result<(String, SummaryInputMeta)> {
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
            .run_summary_call_with_cancel(
                active,
                cut,
                previous_summary,
                history_already_compacted,
                target,
                request_cancel,
            )
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
        active: &[Value],
        cut: usize,
        previous_summary: Option<&str>,
        history_already_compacted: bool,
        target: LlmModelTarget<'_>,
        request_cancel: crate::cancel::CancellationToken,
    ) -> Result<(String, SummaryInputMeta)> {
        let input = self.build_summary_input(
            active,
            cut,
            previous_summary,
            history_already_compacted,
            target,
        )?;
        let SummaryRequestInput {
            system_prompt,
            tools,
            messages,
            meta,
        } = input;

        if self.config.max_context_tokens > 0 {
            let input_tokens = crate::llm::transport::estimate_openai_context_tokens(
                &messages,
                &tools,
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
        // Cache-aligned summaries deliberately retain the Agent tool schemas so
        // the provider can reuse the immutable request prefix. Those tools are
        // alignment-only: compaction never executes them, and any emitted tool
        // call is rejected explicitly while consuming the response below.
        let request = self.llm_backend.stream(LlmRequest {
            purpose: LlmPurpose::Compaction,
            model: target.model.to_string(),
            model_alias: target.alias.map(str::to_string),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            system_prompt,
            messages,
            tools,
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
        let mut invalid_tool_call = None;
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
                Event::ToolCall(call) if invalid_tool_call.is_none() => {
                    invalid_tool_call = Some((call.name, call.id));
                }
                _ => {}
            }
        }
        if !last_error.is_empty() {
            bail!("failed to generate context summary: {last_error}");
        }
        if let Some((name, id)) = invalid_tool_call {
            bail!(
                "failed to generate context summary: compaction attempted invalid tool call {name} ({id})"
            );
        }
        if !matches!(stop_reason.as_str(), "stop" | "end_turn") {
            bail!("failed to generate context summary: invalid stop reason {stop_reason:?}");
        }
        let summary = strip_dsml_tags(&output);
        if summary.trim().is_empty() {
            bail!("failed to generate context summary: empty response");
        }
        Ok((summary.trim().to_string(), meta))
    }

    fn build_summary_input(
        &self,
        active: &[Value],
        cut: usize,
        previous_summary: Option<&str>,
        history_already_compacted: bool,
        target: LlmModelTarget<'_>,
    ) -> Result<SummaryRequestInput> {
        let dropped = &active[..cut];
        let mut source_messages = Vec::new();
        if let Some(summary) = previous_summary.filter(|summary| !summary.trim().is_empty()) {
            source_messages.push(compacted_summary_message(summary));
        }
        if history_already_compacted
            && let Some(plan) = read_active_plan_checkpoint(&self.summary_path)?
        {
            source_messages.push(plan);
        }
        let dynamic_prefix_len = source_messages.len();
        source_messages.extend(crate::llm::image_projection::project_consumed_attachments(
            active,
        ));
        let source_prefix_len = dynamic_prefix_len.saturating_add(cut);

        let latest = self
            .prompt_usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_request
            .clone();
        let fallback_reason;
        if let Some(latest) = latest {
            let aligned = (|| -> Result<SummaryRequestInput, String> {
                if latest.model != target.model {
                    return Err("model_changed".into());
                }
                if latest.backend_name != self.llm_backend.name() {
                    return Err("cache_domain_changed".into());
                }
                if latest.projection_generation != self.projection_generation.load(Ordering::SeqCst)
                {
                    return Err("projection_generation_changed".into());
                }
                let Some(recent_projection) = latest.projection.as_ref() else {
                    return Err("backend_projection_unavailable".into());
                };
                let source_request = LlmRequest {
                    purpose: LlmPurpose::Agent,
                    model: target.model.to_string(),
                    model_alias: target.alias.map(str::to_string),
                    api_url: self.api_url.clone(),
                    api_key: self.api_key.clone(),
                    system_prompt: latest.source_system_prompt.clone(),
                    messages: source_messages.clone(),
                    tools: latest.source_tools.clone(),
                    max_tokens: effective_max_tokens(&self.config),
                    cancel: self.cancel.clone(),
                    verbose: self.config.verbose,
                    display: self.display.clone(),
                };
                let Some(candidate) = self
                    .llm_backend
                    .cache_projection(&source_request, source_prefix_len)
                else {
                    return Err("backend_projection_unavailable".into());
                };
                if candidate.model != recent_projection.model {
                    return Err("model_changed".into());
                }
                if candidate.system_prompt != recent_projection.system_prompt
                    || candidate.tools != recent_projection.tools
                {
                    return Err("system_tools_changed".into());
                }
                if candidate.messages.len() != source_prefix_len {
                    return Err("source_boundary_unproven".into());
                }
                let candidate_hashes = message_hashes(&candidate.messages);
                let aligned_messages = candidate_hashes
                    .iter()
                    .zip(&recent_projection.message_hashes)
                    .take_while(|(left, right)| left == right)
                    .count();
                let aligned_messages = rollback_incomplete_tool_exchange_boundary(
                    &candidate.messages,
                    aligned_messages,
                );
                if aligned_messages == 0 {
                    return Err("no_safe_message_prefix".into());
                }
                let mut messages = candidate.messages[..aligned_messages].to_vec();
                let suffix = &candidate.messages[aligned_messages..];
                let reduced_suffix_messages = suffix.len();
                if !suffix.is_empty() {
                    if self.config.context_compact_input_reduction {
                        messages.push(json!({
                            "role": "user",
                            "internal": true,
                            "content": format!(
                                "<compaction-uncached-suffix>\n{}\n</compaction-uncached-suffix>",
                                compaction_input::reduce_for_summary(suffix)
                            ),
                        }));
                    } else {
                        let mut raw_suffix = suffix.to_vec();
                        crate::llm::image_projection::degrade_images_for_summary(&mut raw_suffix);
                        messages.extend(raw_suffix);
                    }
                }
                let aligned_estimated_tokens =
                    crate::llm::transport::estimate_openai_context_tokens(
                        &candidate.messages[..aligned_messages],
                        &candidate.tools,
                        &candidate.system_prompt,
                    )
                    .map_err(|error| format!("aligned_estimate_failed:{error}"))?;
                messages.push(compaction_instruction_message());
                if summary_input_over_budget(
                    &self.config,
                    &messages,
                    &candidate.tools,
                    &candidate.system_prompt,
                )
                .map_err(|error| format!("aligned_estimate_failed:{error}"))?
                {
                    return Err("aligned_input_over_budget".into());
                }
                Ok(SummaryRequestInput {
                    system_prompt: candidate.system_prompt,
                    tools: candidate.tools,
                    messages,
                    meta: SummaryInputMeta {
                        input_mode: if aligned_messages == candidate.messages.len() {
                            "cache_aligned"
                        } else {
                            "partial_aligned"
                        },
                        aligned_messages,
                        aligned_estimated_tokens,
                        reduced_suffix_messages,
                        fallback_reason: None,
                    },
                })
            })();
            match aligned {
                Ok(input) => return Ok(input),
                Err(reason) => fallback_reason = Some(reason),
            }
        } else {
            fallback_reason = Some("no_recent_agent_request".into());
        }

        let mut history = Vec::new();
        if let Some(summary) = previous_summary.filter(|summary| !summary.trim().is_empty()) {
            history.push(compacted_summary_message(summary));
        }
        history.extend(crate::llm::image_projection::project_consumed_attachments(
            dropped,
        ));
        crate::llm::image_projection::degrade_images_for_summary(&mut history);
        let input_mode = if self.config.context_compact_input_reduction {
            "reduced"
        } else {
            "raw"
        };
        let mut messages = if self.config.context_compact_input_reduction {
            vec![json!({
                "role": "user",
                "internal": true,
                "content": compaction_input::reduce_for_summary(&history),
            })]
        } else {
            history
        };
        messages.push(compaction_instruction_message());
        if summary_input_over_budget(&self.config, &messages, &[], FALLBACK_SYSTEM_PROMPT)? {
            bail!("compaction summary input exceeds configured budget");
        }
        Ok(SummaryRequestInput {
            system_prompt: FALLBACK_SYSTEM_PROMPT.to_string(),
            tools: Vec::new(),
            messages,
            meta: SummaryInputMeta {
                input_mode,
                aligned_messages: 0,
                aligned_estimated_tokens: 0,
                reduced_suffix_messages: dropped.len(),
                fallback_reason,
            },
        })
    }

    fn log_compaction_check(
        &self,
        trigger: &str,
        pressure: &PressureDecision,
        local_tokens: usize,
        threshold_tokens: usize,
    ) {
        if !self.config.log_events {
            return;
        }
        self.write_event(crate::events::EventLog::CompactionCheck {
            trigger: trigger.to_string(),
            pressure_source: pressure.source.to_string(),
            local_tokens,
            provider_baseline_tokens: pressure.provider_baseline_tokens,
            calibrated_tokens: (pressure.source == "provider_calibrated")
                .then_some(pressure.effective_tokens),
            threshold_tokens,
            projection_generation: self.projection_generation.load(Ordering::SeqCst),
        });
    }

    fn write_event(&self, event: crate::events::EventLog) {
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        if let Some(writer) = &self.event_log_writer {
            writer.send(line);
        }
    }

    fn log_compact_event(&self, usage: &UsageEvent) {
        if !self.config.log_events {
            return;
        }
        self.write_event(crate::events::EventLog::usage(usage, "compact"));
    }
}

fn compacted_summary_message(summary: &str) -> Value {
    json!({
        "role": "user",
        "internal": true,
        "content": format!(
            "This is an automatically generated checkpoint condensing an earlier span of the conversation. Treat it as established background and continue from the messages that follow without acknowledging the checkpoint.\n\n<compacted-summary>\n{}\n</compacted-summary>",
            summary.trim()
        ),
    })
}

fn compaction_instruction_message() -> Value {
    json!({
        "role": "user",
        "internal": true,
        "content": COMPACTION_INSTRUCTION,
    })
}

fn read_active_plan_checkpoint(summary_path: &Path) -> Result<Option<Value>> {
    let plan_path = summary_path.with_file_name("plan.md");
    let content = match std::fs::read_to_string(&plan_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "cannot read active plan checkpoint {}: {error}",
                plan_path.display()
            ));
        }
    };
    let content = content.trim();
    Ok((!content.is_empty()).then(|| {
        json!({
            "role": "user",
            "internal": true,
            "content": format!("<active-plan-checkpoint>\n{content}\n</active-plan-checkpoint>"),
        })
    }))
}

fn summary_input_over_budget(
    config: &Config,
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
) -> Result<bool> {
    if config.max_context_tokens == 0 {
        return Ok(false);
    }
    let input_tokens =
        crate::llm::transport::estimate_openai_context_tokens(messages, tools, system_prompt)?;
    let input_limit = config
        .max_context_tokens
        .saturating_sub(usize::try_from(compaction_max_output_tokens(config)).unwrap_or(0));
    Ok(input_tokens > input_limit)
}

pub(crate) fn prefix_fingerprint(system_prompt: &str, tools: &[Value]) -> String {
    crate::session::prefix::ImmutablePrefix::compute_fingerprint(system_prompt, tools, None)
}

fn provider_projection_extends(
    previous: &CacheProjectionSnapshot,
    current: &LlmCacheProjection,
) -> bool {
    previous.model == current.model
        && previous.system_prompt == current.system_prompt
        && previous.tools == current.tools
        && previous.message_hashes.len() <= current.messages.len()
        && previous
            .message_hashes
            .iter()
            .zip(message_hashes(&current.messages))
            .all(|(left, right)| left == &right)
}

impl CacheProjectionSnapshot {
    fn from_projection(projection: LlmCacheProjection) -> Self {
        Self {
            model: projection.model,
            system_prompt: projection.system_prompt,
            tools: projection.tools,
            message_hashes: message_hashes(&projection.messages),
        }
    }
}

fn message_hashes(messages: &[Value]) -> Vec<[u8; 32]> {
    use sha2::{Digest, Sha256};

    messages
        .iter()
        .map(|message| {
            let mut hasher = Sha256::new();
            hasher.update(serde_json::to_vec(message).unwrap_or_default());
            hasher.finalize().into()
        })
        .collect()
}

/// A cache LCP may end between an assistant tool call and its user-side tool
/// result (for example when an attachment in that result changed projection).
/// Keep the aligned prefix protocol-complete: otherwise OpenAI conversion
/// strips the orphan call before the reduced suffix can describe the exchange.
fn rollback_incomplete_tool_exchange_boundary(messages: &[Value], boundary: usize) -> usize {
    let prefix = &messages[..boundary.min(messages.len())];
    let result_ids = prefix
        .iter()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();

    prefix
        .iter()
        .enumerate()
        .filter(|(_, message)| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .find_map(|(index, message)| {
            message
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(Value::as_str) == Some("tool_use")
                            && block
                                .get("id")
                                .and_then(Value::as_str)
                                .is_none_or(|id| id.is_empty() || !result_ids.contains(id))
                    })
                })
                .then_some(index)
        })
        .unwrap_or(boundary)
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
    matches!(trigger, "manual" | "preflight" | "overflow")
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
