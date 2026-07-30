use crate::config::Config;
use crate::llm::client::{LlmBackend, LlmModelTarget, LlmPurpose, LlmRequest, MeteredStream};
use crate::protocol::{ErrorEvent, Event, StopEvent, TextEvent, UsageEvent};
use crate::session::compaction_input;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::session::usage::{UsageJournal, UsageKind};
use crate::ui::Display;
use anyhow::{Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    state: RwLock<std::result::Result<CompactionState, String>>,
    compact_lock: tokio::sync::Mutex<()>,
}

impl CompactionEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<ConversationStore>,
        summary_path: PathBuf,
        api_url: String,
        config: &Config,
        stats: Arc<StatsTracker>,
        usage: Arc<UsageJournal>,
        session_id: String,
        display: Arc<dyn Display>,
        cancel: crate::cancel::CancellationToken,
        llm_backend: Arc<dyn LlmBackend>,
    ) -> Self {
        let state_path = summary_path.with_file_name("context-state.json");
        let state = load_state(&state_path).map_err(|error| error.to_string());
        Self {
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
            state: RwLock::new(state),
            compact_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn current_summary(&self) -> Result<Option<String>> {
        let state = self.current_state()?;
        Ok((!state.summary.trim().is_empty()).then_some(state.summary))
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
        let temp_path = self.state_path.with_extension("json.tmp");
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Err(error) = tokio::fs::write(&temp_path, &data).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
        if let Err(error) = tokio::fs::rename(&temp_path, &self.state_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
        *self
            .state
            .write()
            .map_err(|_| anyhow::anyhow!("context state lock poisoned"))? = Ok(state.clone());
        self.store.prune_cache_before(state.active_start).await;
        let _ = tokio::fs::write(&self.summary_path, format!("{}\n", state.summary)).await;
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
        let previous = serde_json::to_string(&previous_summary)?;
        let instruction = format!(
            "Merge the conversation turns above with the previous context snapshot below.\n\
             Preserve current facts, decisions, constraints, progress, and blockers.\n\
             Previous context snapshot: {previous}\n\n\
             Output these five non-empty fields:\n\
             Task focus:\nLatest request:\nProgress:\nTool evidence:\nReflections:\n\
             Start directly with Task focus: and do not use code fences."
        );
        let mut messages = if self.config.context_compact_input_reduction {
            vec![json!({
                "role": "user",
                "content": compaction_input::reduce_for_summary(dropped),
            })]
        } else {
            dropped.to_vec()
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
        let response = match self
            .llm_backend
            .stream(LlmRequest {
                purpose: LlmPurpose::Compaction,
                model: target.model.to_string(),
                model_alias: target.alias.map(str::to_string),
                api_url: self.api_url.clone(),
                api_key: self.api_key.clone(),
                system_prompt,
                messages,
                tools: Vec::new(),
                max_tokens: compaction_max_output_tokens(&self.config),
                cancel: self.cancel.clone(),
                verbose: self.config.verbose,
                display: self.display.clone(),
            })
            .await
        {
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
        while let Some(event) = stream.next().await {
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
        let events_path = self.summary_path.with_file_name("events.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
        {
            let event = json!({
                "type": "usage",
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_input_tokens": usage.cache_read_input_tokens,
                "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                "kind": "compact",
            });
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = writeln!(file, "{line}");
            }
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
    (1..=candidate)
        .rev()
        .find(|&index| is_safe_context_start(&messages[index]))
        .unwrap_or(0)
}

fn is_safe_context_start(message: &Value) -> bool {
    let role = message.get("role").and_then(Value::as_str);
    role == Some("assistant")
        || (role == Some("user") && message.get("content").is_some_and(Value::is_string))
}

fn strip_dsml_tags(text: &str) -> String {
    let regex = regex::Regex::new(r"</?ds_\w+[^>]*>").unwrap();
    regex.replace_all(text, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::MockLlmClient;
    use crate::protocol::StopEvent;
    use serde_json::json;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct CapturedSummaryRequest {
        model: String,
        model_alias: Option<String>,
        system_prompt: String,
        messages: Vec<Value>,
        max_tokens: i32,
    }

    #[derive(Default)]
    struct CapturingSummaryBackend {
        requests: Mutex<Vec<CapturedSummaryRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmBackend for CapturingSummaryBackend {
        fn name(&self) -> &str {
            "capturing-summary"
        }

        async fn stream(
            &self,
            request: LlmRequest,
        ) -> Result<crate::llm::client::LlmResponseStream> {
            self.requests.lock().unwrap().push(CapturedSummaryRequest {
                model: request.model,
                model_alias: request.model_alias,
                system_prompt: request.system_prompt,
                messages: request.messages,
                max_tokens: request.max_tokens,
            });
            Ok(crate::llm::client::LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(Event::Text(TextEvent {
                        content: "Task focus: test\nLatest request: compact\nProgress: retained\nTool evidence: cargo test\nReflections: none".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ])),
                attempt_count: 1,
            })
        }
    }

    fn summary_backend() -> Arc<dyn LlmBackend> {
        Arc::new(MockLlmClient::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "Task focus: test\nLatest request: compact\nProgress: retained\nTool evidence: none\nReflections: none".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ))
    }

    async fn add_tool_history(ctx: &crate::context::AgentSharedContext) -> anyhow::Result<()> {
        ctx.store.add_user("fix the test suite").await?;
        ctx.store
            .add_assistant(
                "I will run the tests.",
                "private reasoning",
                &[crate::protocol::ToolCallEvent {
                    name: "Bash".into(),
                    id: "bash-1".into(),
                    input_json: json!({"command":"cargo test"}),
                    fields: Default::default(),
                    order: Vec::new(),
                }],
            )
            .await?;
        ctx.store
            .add_tool_results(&[crate::session::store::ToolResult {
                tool_use_id: "bash-1".into(),
                tool_name: "Bash".into(),
                tool_args: Default::default(),
                content: "Process completed with exit code 1.".into(),
                conv_content: "Process completed with exit code 1.".into(),
                state_metadata: None,
            }])
            .await?;
        ctx.store.add_user("keep the API stable").await?;
        Ok(())
    }

    async fn compact(
        ctx: &crate::context::AgentSharedContext,
        trigger: &str,
        context_tokens: usize,
    ) -> anyhow::Result<(bool, String)> {
        let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
        ctx.compaction
            .evaluate_and_compact(
                trigger,
                context_tokens,
                LlmModelTarget::new(&resolved.actual, resolved.alias.as_deref()),
            )
            .await
    }

    #[test]
    fn output_tokens_use_explicit_reserve() {
        let config = Config {
            max_context_tokens: 64_000,
            max_tokens: 81_920,
            context_reserve_tokens: 12_000,
            context_compact_max_output_tokens: 2_048,
            ..Config::default()
        };
        assert_eq!(effective_max_tokens(&config), 12_000);
        assert_eq!(request_input_limit(&config), 52_000);
        assert_eq!(compaction_max_output_tokens(&config), 2_048);
    }

    #[test]
    fn trigger_uses_explicit_percentage_and_reserve() {
        let config = Config {
            max_context_tokens: 64_000,
            context_compact_pct: 90,
            context_reserve_tokens: 12_000,
            ..Config::default()
        };
        assert_eq!(compaction_trigger_tokens(&config), 52_000);
    }

    #[test]
    fn zero_context_window_keeps_request_budget_unbounded() {
        let config = Config {
            max_context_tokens: 0,
            max_tokens: 16_000,
            ..Config::default()
        };
        assert_eq!(effective_max_tokens(&config), 16_000);
        assert_eq!(request_input_limit(&config), usize::MAX);
    }

    #[test]
    fn cut_point_can_compact_completed_tool_exchanges_in_one_user_turn() {
        let messages = vec![
            json!({"role":"user","content":"fix it"}),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"a","name":"Read","input":{"path":"a"}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"x".repeat(2000)}]}),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"b","name":"Read","input":{"path":"b"}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"b","content":"new"}]}),
        ];
        let cut = find_compaction_cut_point(&messages, 10);
        assert!(cut > 0);
        assert_eq!(messages[cut]["role"], "assistant");
    }

    #[test]
    fn compaction_state_rejects_boundary_beyond_history() {
        let state = CompactionState {
            active_start: 2,
            ..CompactionState::default()
        };
        let messages = [json!({"role":"user","content":"only"})];
        assert!(state.active_start > messages.len());
    }

    #[tokio::test]
    async fn compaction_keeps_full_history_and_persists_state() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-keeps-history",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_compact_pct = 65;
                config.context_reserve_tokens = 12_000;
                config.context_compact_tail_tokens = 16_000;
                config.context_compact_max_output_tokens = 2_048;
            },
            summary_backend(),
        )
        .await?;
        for index in 0..4 {
            ctx.store
                .add_user(&format!("request {index}: {}", "x".repeat(8_000)))
                .await?;
            ctx.store
                .add_assistant(&format!("progress {index}: {}", "y".repeat(8_000)), "", &[])
                .await?;
        }

        let full_history = ctx.store.lines().await?;
        let (compacted, _) = compact(&ctx, "manual", 50_000).await?;
        assert!(compacted);
        assert_eq!(ctx.store.lines().await?, full_history);

        let projected = ctx.compaction.active_messages().await?;
        assert!(projected.len() < full_history.len());
        let persisted = load_state(&ctx.summary_path.with_file_name("context-state.json"))?;
        assert!(persisted.active_start > 0);
        assert!(!persisted.summary.is_empty());
        assert!(
            !ctx.summary_path
                .with_file_name("context-state.json")
                .with_extension("json.tmp")
                .exists()
        );
        Ok(())
    }

    #[tokio::test]
    async fn enabled_input_reduction_changes_only_the_summary_request() -> anyhow::Result<()> {
        let backend = Arc::new(CapturingSummaryBackend::default());
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-input-reduction",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_reserve_tokens = 8_000;
                config.context_compact_tail_tokens = 1;
                config.context_compact_max_output_tokens = 1_234;
                config.context_compact_input_reduction = true;
            },
            backend.clone(),
        )
        .await?;
        add_tool_history(&ctx).await?;

        let full_history = ctx.store.lines().await?;
        let (compacted, _) = ctx
            .compaction
            .evaluate_and_compact(
                "manual",
                0,
                LlmModelTarget::new("active-summary-model", Some("active-summary")),
            )
            .await?;
        assert!(compacted);
        assert_eq!(ctx.store.lines().await?, full_history);

        let guard = backend.requests.lock().unwrap();
        let request = guard.first().expect("summary request captured");
        assert_eq!(request.model, "active-summary-model");
        assert_eq!(request.model_alias.as_deref(), Some("active-summary"));
        assert_eq!(request.max_tokens, 1_234);
        assert!(
            request
                .system_prompt
                .starts_with("Summarize coding-agent history")
        );
        let serialized = serde_json::to_string(&request.messages)?;
        assert!(serialized.contains("command=cargo test"));
        assert!(serialized.contains("Process completed with exit code 1."));
        assert!(!serialized.contains("private reasoning"));
        Ok(())
    }

    #[tokio::test]
    async fn disabled_input_reduction_sends_original_structured_history() -> anyhow::Result<()> {
        let backend = Arc::new(CapturingSummaryBackend::default());
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-without-input-reduction",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_reserve_tokens = 8_000;
                config.context_compact_tail_tokens = 1;
                config.context_compact_max_output_tokens = 1_234;
                config.context_compact_input_reduction = false;
            },
            backend.clone(),
        )
        .await?;
        add_tool_history(&ctx).await?;

        assert!(compact(&ctx, "manual", 0).await?.0);

        let guard = backend.requests.lock().unwrap();
        let request = guard.first().expect("summary request captured");
        let serialized = serde_json::to_string(&request.messages)?;
        assert!(serialized.contains("private reasoning"));
        assert!(serialized.contains("\"type\":\"tool_use\""));
        assert!(serialized.contains("cargo test"));
        assert!(!serialized.contains("<conversation>"));
        Ok(())
    }

    #[tokio::test]
    async fn repeated_compaction_advances_boundary_and_merges_previous_summary()
    -> anyhow::Result<()> {
        let backend = Arc::new(CapturingSummaryBackend::default());
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-repeatedly",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_reserve_tokens = 8_000;
                config.context_compact_tail_tokens = 1;
                config.context_compact_max_output_tokens = 1_234;
                config.context_compact_input_reduction = true;
            },
            backend.clone(),
        )
        .await?;
        for index in 0..3 {
            ctx.store
                .add_user(&format!(
                    "first batch request {index}: {}",
                    "x".repeat(2_000)
                ))
                .await?;
            ctx.store
                .add_assistant(
                    &format!("first batch progress {index}: {}", "y".repeat(2_000)),
                    "",
                    &[],
                )
                .await?;
        }

        assert!(compact(&ctx, "manual", 0).await?.0);
        let state_path = ctx.summary_path.with_file_name("context-state.json");
        let first_state = load_state(&state_path)?;
        assert!(first_state.active_start > 0);

        for index in 0..3 {
            ctx.store
                .add_user(&format!(
                    "second batch request {index}: {}",
                    "a".repeat(2_000)
                ))
                .await?;
            ctx.store
                .add_assistant(
                    &format!("second batch progress {index}: {}", "b".repeat(2_000)),
                    "",
                    &[],
                )
                .await?;
        }
        let full_history = ctx.store.lines().await?;

        assert!(compact(&ctx, "manual", 0).await?.0);

        let second_state = load_state(&state_path)?;
        assert!(second_state.active_start > first_state.active_start);
        assert_eq!(ctx.store.lines().await?, full_history);
        let projected = ctx.compaction.active_messages().await?;
        assert_eq!(projected[0]["role"], "system");
        assert!(projected.len() < full_history.len());

        let guard = backend.requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        let second_instruction = guard[1]
            .messages
            .last()
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .expect("second summary instruction");
        let encoded_previous = serde_json::to_string(&Some(first_state.summary.as_str()))?;
        assert!(
            second_instruction.contains(&format!("Previous context snapshot: {encoded_previous}"))
        );
        let second_request = serde_json::to_string(&guard[1].messages)?;
        assert!(second_request.contains("second batch request"));
        Ok(())
    }

    #[tokio::test]
    async fn summary_preserves_tool_commands_and_paths() -> anyhow::Result<()> {
        let backend = Arc::new(MockLlmClient::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "Task focus: fix build\nLatest request: continue\nProgress: Read(src/lib.rs)\nTool evidence: Bash(cargo test) failed\nReflections: none".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ));
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-preserves-tool-evidence",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_compact_tail_tokens = 1;
            },
            backend,
        )
        .await?;
        ctx.store.add_user("fix the build").await?;
        ctx.store.add_assistant("checking", "", &[]).await?;

        let (compacted, _) = compact(&ctx, "manual", 0).await?;

        assert!(compacted);
        let summary = ctx.compaction.current_summary()?.unwrap();
        assert!(summary.contains("Read(src/lib.rs)"));
        assert!(summary.contains("Bash(cargo test)"));
        Ok(())
    }

    #[tokio::test]
    async fn zero_context_window_disables_auto_but_allows_manual_compaction() -> anyhow::Result<()>
    {
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "compact-zero-window",
            |config| {
                config.max_context_tokens = 0;
                config.context_compact_tail_tokens = 4_000;
            },
            summary_backend(),
        )
        .await?;
        for index in 0..3 {
            ctx.store
                .add_user(&format!("request {index}: {}", "x".repeat(4_000)))
                .await?;
            ctx.store
                .add_assistant(&format!("progress {index}: {}", "y".repeat(4_000)), "", &[])
                .await?;
        }

        let (automatic, reason) = compact(&ctx, "auto", usize::MAX).await?;
        assert!(!automatic);
        assert_eq!(reason, "automatic compaction disabled");

        let (manual, _) = compact(&ctx, "manual", usize::MAX).await?;
        assert!(manual);
        Ok(())
    }
}
