use crate::context::AgentSharedContext;
use crate::llm::client::LlmClient;
use crate::protocol::{Event, ToolCallEvent, UsageEvent};
use crate::session::store::{ToolResult, build_tool_call_summary, first_line};
use crate::sse::toolcall::build_tool_call_event;
use crate::tools::runner::ToolRunner;
use crate::ui::ToolResultDisplay;
use crate::util::truncate_str;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// 存储 LLM 流式响应阶段（Phase 1）的输出。
struct StreamOutput {
    text: String,
    thinking: String,
    calls: Vec<ToolCallEvent>,
    stop: String,
    usage: Option<UsageEvent>,
}

#[derive(Debug)]
struct ContextOverflowError {
    message: String,
}

impl std::fmt::Display for ContextOverflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ContextOverflowError {}

/// TurnExecutor runs a single "turn" of the agent loop:
///   Stream (LLM response) → Persist → Tools → Decide (continue/stop)
pub struct TurnExecutor {
    ctx: Arc<AgentSharedContext>,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRunner>,
    prefix: crate::agent::prefix::PrefixManager,
    compactor: crate::agent::compactor::TurnCompactor,
    signal_processor: crate::agent::tool_signals::ToolSignalProcessor,
    plan_actions: crate::agent::plan_actions::PlanActionHandler,
    sub_agents: crate::agent::sub_coordinator::SubAgentCoordinator,
    tool_call_count: u32,
    /// 决策引擎（含冷却逻辑，由引擎内部管理）。
    decision_engine: crate::agent::decision::DecisionEngine,
    recovery_policy: crate::agent::recovery_policy::RecoveryPolicy,
    /// Set after a signal injection. The next tool batch must observe before mutating.
    signal_recovery_guard: bool,
    /// 恢复守卫连续拦截计数（达到 guard_max_blocks 后绕过守卫并强制证据注入）。
    guard_blocks: usize,
    /// 守卫已达上限被绕过：下一次决策强制注入证据（即使处于冷却）。
    guard_bypassed: bool,
    /// 本输入内 Warning 级响应的累计次数（连续 2 次触发 R3 策略重启）。
    warning_count: usize,
    /// 本输入内已尝试的 R3 策略重启次数。
    replan_attempts: usize,
    /// 当前用户输入原文（R3 任务报告引用）。
    current_user_input: String,
    /// 子代理配置副本（R3 用，含当前活动模型）。
    sub_agent_config: crate::config::Config,
    todo_final_reminder_sent: bool,
    todo_progress_reminder_sent: bool,
    successful_work_calls_since_todo_advance: u32,
    final_text: String,
    final_thinking: String,
}

/// Represents the outcome of a turn that needs to be actioned.
#[derive(Debug, Clone)]
pub enum TurnEffect {
    PlanCleared,
    PlanConfirmed,
}

#[derive(Debug, PartialEq)]
pub enum TurnDecision {
    Continue,
    Stop,
    Interrupted,
    MaxTurnsExceeded,
    Failed(String),
}

impl TurnExecutor {
    pub fn new(ctx: Arc<AgentSharedContext>, llm: Arc<dyn LlmClient>) -> Self {
        let tools = Arc::new(ToolRunner::new(Arc::new(
            crate::context::ToolContext::from(ctx.as_ref()),
        )));
        let prefix = crate::agent::prefix::PrefixManager::new(ctx.clone());
        let mut sub_agent_config = ctx.config.clone();
        if let Some(alias) = llm.model_alias() {
            sub_agent_config
                .model_aliases
                .insert(alias.to_string(), llm.model().to_string());
            sub_agent_config.model = alias.to_string();
        } else {
            sub_agent_config.model = llm.model().to_string();
        }
        Self {
            ctx: ctx.clone(),
            llm,
            tools,
            prefix,
            compactor: crate::agent::compactor::TurnCompactor::new(ctx.clone()),
            signal_processor: crate::agent::tool_signals::ToolSignalProcessor::new(),
            plan_actions: crate::agent::plan_actions::PlanActionHandler,
            sub_agents: crate::agent::sub_coordinator::SubAgentCoordinator::new(
                ctx.clone(),
                sub_agent_config.clone(),
            ),
            sub_agent_config,
            tool_call_count: 0,
            decision_engine: crate::agent::decision::DecisionEngine::from_config(
                &ctx.config.signal,
            ),
            recovery_policy: crate::agent::recovery_policy::RecoveryPolicy::from_resolved(
                &ctx.tool_capabilities,
                crate::tools::catalog::ToolCatalog::builtin()
                    .expect("built-in tool catalog was validated during context construction"),
            ),
            signal_recovery_guard: false,
            guard_blocks: 0,
            guard_bypassed: false,
            warning_count: 0,
            replan_attempts: 0,
            current_user_input: String::new(),
            todo_final_reminder_sent: false,
            todo_progress_reminder_sent: false,
            successful_work_calls_since_todo_advance: 0,
            final_text: String::new(),
            final_thinking: String::new(),
        }
    }

    /// Return the total number of tool calls made during this turn.
    pub fn tool_call_count(&self) -> u32 {
        self.tool_call_count
    }

    /// Number of tool calls that produced at least one tool_error signal.
    pub fn tool_error_count(&self) -> u32 {
        self.signal_processor.tool_error_count()
    }

    /// Collected signals from all tool calls in this turn.
    pub fn collected_signals(&self) -> &[crate::guard::collector::Signal] {
        self.signal_processor.collected_signals()
    }

    pub fn text(&self) -> &str {
        &self.final_text
    }

    pub fn thinking(&self) -> &str {
        &self.final_thinking
    }

    fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
        self.prefix.ensure()
    }

    fn project_request_messages(
        &self,
        messages: &[serde_json::Value],
    ) -> Result<Vec<serde_json::Value>> {
        crate::session::plan::project_current_plan(
            &self.ctx.plan_path,
            messages,
            self.ctx.config.plan_projection_tail,
        )
    }

    async fn reconcile_todo_state(&self, messages: &mut Vec<serde_json::Value>) -> Result<bool> {
        let Some(read_provider) = self.ctx.todo_read_provider() else {
            return Ok(false);
        };
        let visible = crate::session::todo::visible_revision(messages)?;
        let snapshot = self.ctx.todo_store.snapshot();
        if visible > snapshot.revision {
            anyhow::bail!(
                "todo conversation revision {visible} is newer than persisted revision {}; refusing to continue",
                snapshot.revision
            );
        }
        if visible == snapshot.revision {
            return Ok(false);
        }
        let message = crate::session::todo::sync_message(&snapshot, read_provider);
        self.ctx
            .store
            .append_runtime_message(message.clone())
            .await?;
        messages.push(message);
        Ok(true)
    }

    /// 通过 turn 级统一守卫尝试上下文压缩。成功时更新 messages/system_prompt/tools_json。
    /// 返回 true 表示进行了压缩。
    async fn try_compact(
        &mut self,
        trigger: &str,
        messages: &mut Vec<serde_json::Value>,
        system_prompt: &mut String,
        tools_json: &mut Vec<serde_json::Value>,
    ) -> Result<bool> {
        let compacted = self
            .compactor
            .maybe_compact(
                trigger,
                messages,
                system_prompt,
                tools_json,
                &self.prefix,
                self.llm.model_target(),
            )
            .await?;
        if compacted {
            self.reconcile_todo_state(messages).await?;
        }
        Ok(compacted)
    }

    /// Phase 1: 发送 LLM 请求并流式读取响应，返回 `StreamOutput`。
    /// 网络/协议错误通过 `bail!` 传播，由调用方转为 `TurnDecision::Failed`。
    async fn stream_llm_response(
        &mut self,
        messages: &[serde_json::Value],
        system_prompt: &str,
        tools_json: &[serde_json::Value],
        current_context_tokens: usize,
    ) -> anyhow::Result<StreamOutput> {
        let mut stream = match self
            .llm
            .stream(&self.ctx, messages, tools_json, system_prompt)
            .await
        {
            Ok(stream) => stream,
            Err(error) if is_context_overflow_message(&error.to_string()) => {
                return Err(anyhow::Error::new(ContextOverflowError {
                    message: error.to_string(),
                }));
            }
            Err(error) => return Err(error),
        };

        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCallEvent> = Vec::new();
        let mut stop = String::new();
        let mut usage: Option<UsageEvent> = None;
        let mut saw_stop = false;
        let mut saw_any_event = false;
        let mut saw_visible_output = false;
        let stream_started = Instant::now();
        let mut last_event_at = stream_started;
        let mut last_heartbeat_at = stream_started;
        let first_event_timeout = positive_duration(self.ctx.config.llm_first_event_timeout_secs);
        let idle_timeout = positive_duration(self.ctx.config.llm_idle_timeout_secs);
        let heartbeat = positive_duration(self.ctx.config.llm_wait_heartbeat_secs);

        loop {
            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx
                    .log_event(serde_json::json!({"type":"stop","reason":"interrupted"}));
                stop = "interrupted".into();
                saw_stop = true;
                break;
            }

            let result = tokio::select! {
                result = stream.next() => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {
                    self.check_llm_wait_timeout(
                        saw_any_event,
                        stream_started,
                        last_event_at,
                        first_event_timeout,
                        idle_timeout,
                    )?;
                    self.maybe_render_llm_wait_heartbeat(
                        saw_any_event,
                        stream_started,
                        last_event_at,
                        &mut last_heartbeat_at,
                        heartbeat,
                    );
                    if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                        self.ctx
                            .log_event(serde_json::json!({"type":"stop","reason":"interrupted"}));
                        stop = "interrupted".into();
                        saw_stop = true;
                        break;
                    }
                    continue;
                }
            };
            let Some(result) = result else {
                break;
            };
            let evt = result?;
            saw_any_event = true;
            last_event_at = Instant::now();

            match evt {
                Event::Thinking(t) => {
                    saw_visible_output = true;
                    self.ctx
                        .log_event(serde_json::json!({"type":"thinking","content":t.content}));
                    self.ctx.display.render_thinking(&t.content);
                    thinking.push_str(&t.content);
                }
                Event::Text(t) => {
                    saw_visible_output = true;
                    self.ctx
                        .log_event(serde_json::json!({"type":"text","content":t.content}));
                    self.ctx.display.render_text(&t.content);
                    text.push_str(&t.content);
                }
                Event::ToolCall(call) => {
                    saw_visible_output = true;
                    self.ctx.log_event(serde_json::json!({"type":"tool_call","name":call.name,"id":call.id,"input":call.input_json}));
                    let summary = build_tool_call_summary(&call.name, &call.fields);
                    self.ctx
                        .display
                        .render_tool_call_detail(&crate::ui::ToolCallDisplay {
                            tool_use_id: &call.id,
                            tool_name: &call.name,
                            summary: &summary,
                            input: Some(&call.input_json),
                        });
                    calls.push(call);
                }
                Event::Usage(u) => {
                    self.ctx.log_event(serde_json::json!({
                        "type":"usage",
                        "input_tokens":u.input_tokens,
                        "output_tokens":u.output_tokens,
                        "cache_read_input_tokens":u.cache_read_input_tokens,
                        "cache_creation_input_tokens":u.cache_creation_input_tokens,
                        "context_tokens": current_context_tokens,
                        "max_context": self.ctx.config.max_context_tokens,
                        "kind":"agent",
                    }));
                    usage = Some(u);
                }
                Event::UsageUnavailable => {}
                Event::Stop(s) => {
                    self.ctx
                        .log_event(serde_json::json!({"type":"stop","reason":s.reason}));
                    stop = s.reason;
                    saw_stop = true;
                    break;
                }
                Event::Error(e) => {
                    self.ctx
                        .log_event(serde_json::json!({"type":"error","message":e.message}));
                    if !saw_visible_output && is_context_overflow_message(&e.message) {
                        return Err(anyhow::Error::new(ContextOverflowError {
                            message: e.message,
                        }));
                    }
                    anyhow::bail!("{}", e.message);
                }
                Event::Retry(_) => {
                    self.ctx.log_event(serde_json::json!({"type":"retry"}));
                    text.clear();
                    thinking.clear();
                    calls.clear();
                    stop.clear();
                    usage = None;
                    saw_stop = false;
                    self.ctx.display.render_retry();
                }
            }
        }

        drop(stream);
        if !saw_stop {
            anyhow::bail!("stream ended without stop event");
        }
        Ok(StreamOutput {
            text,
            thinking,
            calls,
            stop,
            usage,
        })
    }

    fn check_llm_wait_timeout(
        &self,
        saw_any_event: bool,
        stream_started: Instant,
        last_event_at: Instant,
        first_event_timeout: Option<Duration>,
        idle_timeout: Option<Duration>,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        if !saw_any_event {
            if let Some(timeout) = first_event_timeout
                && now.duration_since(stream_started) >= timeout
            {
                let message = format!(
                    "LLM stream first event timeout after {} seconds",
                    timeout.as_secs()
                );
                self.ctx.log_event(serde_json::json!({
                    "type": "turn_error",
                    "category": "llm_first_event_timeout",
                    "error": message,
                    "elapsed_ms": now.duration_since(stream_started).as_millis(),
                }));
                anyhow::bail!(message);
            }
            return Ok(());
        }

        if let Some(timeout) = idle_timeout
            && now.duration_since(last_event_at) >= timeout
        {
            let message = format!(
                "LLM stream idle timeout after {} seconds without events",
                timeout.as_secs()
            );
            self.ctx.log_event(serde_json::json!({
                "type": "turn_error",
                "category": "llm_idle_timeout",
                "error": message,
                "idle_ms": now.duration_since(last_event_at).as_millis(),
            }));
            anyhow::bail!(message);
        }
        Ok(())
    }

    fn maybe_render_llm_wait_heartbeat(
        &self,
        saw_any_event: bool,
        stream_started: Instant,
        last_event_at: Instant,
        last_heartbeat_at: &mut Instant,
        heartbeat: Option<Duration>,
    ) {
        let Some(heartbeat) = heartbeat else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(*last_heartbeat_at) < heartbeat {
            return;
        }
        *last_heartbeat_at = now;
        let phase = if saw_any_event { "idle" } else { "first_event" };
        let elapsed = now.duration_since(stream_started).as_secs();
        let idle = now.duration_since(last_event_at).as_secs();
        self.ctx.log_event(serde_json::json!({
            "type": "llm_wait",
            "phase": phase,
            "elapsed_secs": elapsed,
            "idle_secs": idle,
        }));
        self.ctx.display.render_info(&format!(
            "Waiting for model response... elapsed={}s idle={}s",
            elapsed, idle
        ));
    }

    /// Phase 1b: 从 thinking/text 中回收漏报的工具调用（scavenge）。
    fn scavenge_calls(
        &self,
        thinking: &str,
        text: &str,
        mut calls: Vec<ToolCallEvent>,
    ) -> (Vec<ToolCallEvent>, bool) {
        if thinking.is_empty() && text.is_empty() {
            return (calls, false);
        }
        let (scavenged, notes) = crate::repair::scavenge_combined(
            if thinking.is_empty() {
                None
            } else {
                Some(thinking)
            },
            if text.is_empty() { None } else { Some(text) },
            4,
        );
        let mut recovered = false;
        for sc in &scavenged {
            let cid = format!("scavenged_{}", calls.len());
            match build_tool_call_event(&sc.name, &cid, &sc.arguments) {
                Ok(call) => {
                    let duplicate = calls
                        .iter()
                        .any(|c| c.name == call.name && c.input_json == call.input_json);
                    if !duplicate {
                        calls.push(call);
                        recovered = true;
                    }
                }
                Err(e) => {
                    self.ctx.log_event(serde_json::json!({
                        "type":"scavenge",
                        "note": format!("discarded invalid scavenged call {}: {e}", sc.name),
                    }));
                }
            }
        }
        for note in &notes {
            self.ctx
                .log_event(serde_json::json!({"type":"scavenge","note":note}));
        }
        (calls, recovered)
    }

    /// Phase 2: 持久化 assistant 消息 + 用量统计。
    async fn persist_assistant(
        &self,
        text: &str,
        thinking: &str,
        calls: &[ToolCallEvent],
        usage: &Option<UsageEvent>,
    ) -> Result<()> {
        self.ctx.store.add_assistant(text, thinking, calls).await?;
        if let Some(u) = usage {
            self.ctx
                .stats
                .record_usage_with_model(u, self.llm.model())
                .await;
        }
        Ok(())
    }

    /// 执行一轮中的所有工具调用（Phase 3）。
    /// 处理 Plan 状态转换、子代理生成与收集、结果定稿、信号采集和持久化。
    #[allow(clippy::too_many_arguments)]
    async fn execute_tools_inner(
        &mut self,
        calls: Vec<ToolCallEvent>,
        mut belief: Option<&mut crate::agent::belief::BeliefTracker>,
        effects: &mut Vec<TurnEffect>,
    ) -> Result<Option<&'static str>> {
        self.tool_call_count += calls.len() as u32;
        let (calls_to_execute, mut guarded_results) =
            self.apply_signal_recovery_guard(calls, belief.as_deref_mut());
        let executed = if calls_to_execute.is_empty() {
            Ok(Vec::new())
        } else {
            self.tools.execute_all(calls_to_execute.clone()).await
        };
        let mut results = match executed {
            Ok(results) => results,
            Err(error) => {
                let synthetic: Vec<crate::tools::runner::ToolRunResult> = calls_to_execute
                    .into_iter()
                    .map(|call| {
                        crate::tools::runner::blocked_tool_result(
                            call.id,
                            call.name,
                            call.fields,
                            format!("tool execution failed: {error:#}"),
                        )
                    })
                    .collect();
                let tool_results: Vec<ToolResult> =
                    synthetic.iter().map(ToolResult::from).collect();
                self.ctx.store.add_tool_results(&tool_results).await?;
                return Err(error);
            }
        };
        guarded_results.append(&mut results);
        let results = guarded_results;

        let mut prepared_results = Vec::new();
        let mut plan_compaction_trigger = None;
        for mut result in results {
            if let Some(trigger) = self.plan_actions.handle(&mut result, effects) {
                plan_compaction_trigger = Some(trigger);
            }
            prepared_results.push(result);
        }

        let mut processed_results = self.sub_agents.process(prepared_results).await;
        self.tools.finalize_deferred_results(&mut processed_results);
        for result in &mut processed_results {
            let model_label = self.llm.model_label();
            self.signal_processor
                .process(result, belief.as_deref_mut(), &self.ctx, model_label)
                .await;
        }

        let tool_results: Vec<ToolResult> =
            processed_results.iter().map(ToolResult::from).collect();

        self.ctx.store.add_tool_results(&tool_results).await?;
        self.observe_todo_progress(&processed_results);
        self.maybe_append_todo_progress_reminder().await?;

        for r in &processed_results {
            let preview = if r.tool_name == "Edit" {
                r.content.clone()
            } else if r.tool_name == "Read" || r.tool_name == "Write" {
                first_line(&r.content).to_string() + "\n"
            } else {
                truncate_str(&r.content, 200) + "\n"
            };
            self.ctx.log_event(serde_json::json!({
                "type":"tool_result",
                "version": 2,
                "tool_use_id": r.tool_use_id,
                "name": r.tool_name,
                "content": r.content,
                "success": r.success,
                "exit_code": r.exit_code,
                "result_kind": r.result_kind,
                "presentation": r.presentation,
                "artifacts": r.artifacts,
            }));
            self.ctx
                .display
                .render_tool_result_presented(&crate::ui::PresentedToolResultDisplay {
                    base: ToolResultDisplay {
                        tool_name: &r.tool_name,
                        content_preview: &preview,
                        content: &r.content,
                        tool_use_id: Some(&r.tool_use_id),
                        exit_code: r.exit_code,
                    },
                    success: r.success,
                    result_kind: r.result_kind,
                    presentation: r.presentation.as_ref(),
                    artifacts: &r.artifacts,
                });
        }
        Ok(plan_compaction_trigger)
    }

    fn observe_todo_progress(&mut self, results: &[crate::tools::runner::ToolRunResult]) {
        for result in results {
            if result.content.starts_with("Error:") {
                continue;
            }
            if result.tool_name == "TodoAdvance" {
                self.successful_work_calls_since_todo_advance = 0;
                continue;
            }
            if matches!(
                result.tool_name.as_str(),
                "Bash" | "Python" | "PythonSandbox" | "Write" | "Edit" | "TodoWrite" | "SubAgent"
            ) {
                self.successful_work_calls_since_todo_advance = self
                    .successful_work_calls_since_todo_advance
                    .saturating_add(1);
            }
        }
    }

    async fn maybe_append_todo_progress_reminder(&mut self) -> Result<()> {
        if self.todo_progress_reminder_sent
            || self.successful_work_calls_since_todo_advance < 8
            || !self
                .ctx
                .todo_store
                .snapshot()
                .items
                .iter()
                .any(|item| item.status == crate::session::todo::TodoStatus::InProgress)
        {
            return Ok(());
        }
        let Some(provider) = self.ctx.todo_advance_provider() else {
            return Ok(());
        };
        self.ctx
            .store
            .add_runtime_user(&format!(
                "<todo-progress-reminder>Active todo work has continued across several successful operations without a progress transition. Reassess the active batch and call {provider} if any item should be completed, paused, or otherwise advanced.</todo-progress-reminder>"
            ))
            .await?;
        self.todo_progress_reminder_sent = true;
        Ok(())
    }

    fn apply_signal_recovery_guard(
        &mut self,
        calls: Vec<ToolCallEvent>,
        mut belief: Option<&mut crate::agent::belief::BeliefTracker>,
    ) -> (Vec<ToolCallEvent>, Vec<crate::tools::runner::ToolRunResult>) {
        if !self.signal_recovery_guard || calls.is_empty() {
            return (calls, Vec::new());
        }

        let mut iter = calls.into_iter();
        let first = iter
            .next()
            .expect("signal recovery guard already checked calls is non-empty");
        let call_context = crate::tools::semantic_capabilities::CapabilityCallContext {
            tool_name: &first.name,
            input: &first.input_json,
            resource_router: &self.ctx.resource_router,
            filesystem_backend: self.ctx.tool_resolution_context.filesystem_backend(),
        };
        let decision = self.recovery_policy.classify_first_call(
            &call_context,
            crate::tools::catalog::ToolCatalog::builtin()
                .expect("built-in tool catalog was validated during context construction"),
        );
        if let crate::agent::recovery_policy::RecoveryFirstCallDecision::Blocked(guidance) =
            decision
        {
            guidance
                .validate(&self.ctx.tool_surface)
                .expect("RecoveryPolicy emitted an inactive tool reference");
            // 拦截必须喂回信念（不变式 7）：顽固循环才能升级，而非无限拦截。
            if let Some(bt) = belief.as_mut() {
                let guard_signal = crate::guard::collector::Signal::synthetic(
                    crate::guard::collector::SignalKind::ToolFailed,
                    0.9,
                    "SignalRecoveryGuard",
                    format!("recovery guard blocked non-inspection call {}", first.name),
                );
                bt.observe(std::slice::from_ref(&guard_signal));
            }
            self.guard_blocks += 1;
            let max_blocks = self.ctx.config.signal.guard_max_blocks;
            self.ctx.log_event(serde_json::json!({
                "type": "signal_recovery_guard",
                "action": "blocked_non_inspection",
                "tool": first.name.clone(),
                "tool_use_id": first.id.clone(),
                "reason": guidance.content,
                "guard_blocks": self.guard_blocks,
            }));
            if self.guard_blocks >= max_blocks {
                // 达到上限：绕过守卫放行调用，并强制下一次决策注入证据。
                self.signal_recovery_guard = false;
                self.guard_bypassed = true;
                let mut allowed = Vec::new();
                allowed.push(first);
                allowed.extend(iter);
                return (allowed, Vec::new());
            }
            let blocked: Vec<crate::tools::runner::ToolRunResult> = std::iter::once(first)
                .chain(iter)
                .map(|call| blocked_by_signal_recovery(call, guidance.content.clone()))
                .collect();
            return (Vec::new(), blocked);
        }

        self.signal_recovery_guard = false;
        let mut allowed = Vec::new();
        allowed.push(first);
        allowed.extend(iter);
        (allowed, Vec::new())
    }

    /// R2 状态操作：把**循环窗口内**（最近 ROLLBACK_WINDOW_STEPS 步）被编辑过的
    /// 路径回滚到**最后一次 Read/Write 基线**（SIGNAL_RESPONSE_REDESIGN S4，B1/B2/D2 修复）：
    /// - 回滚目标是 read 基线而非 record_edit 的编辑后内容（否则恒等 no-op）；
    /// - 只回滚窗口内路径，窗口之前的合法编辑保持不动；
    /// - 写回经 atomic_replace（同目录临时文件 + rename），失败只记录不中断 turn。
    async fn apply_rollback(&mut self) -> Result<()> {
        const ROLLBACK_WINDOW_STEPS: usize = 6; // 与 guard/collector 的 seq_window 对齐。
        let paths = self
            .signal_processor
            .evidence()
            .edited_paths_since(ROLLBACK_WINDOW_STEPS);
        if paths.is_empty() {
            return Ok(());
        }
        let mut rolled_back: Vec<serde_json::Value> = Vec::new();
        for raw in paths {
            let path = std::path::PathBuf::from(&raw);
            let full = if path.is_absolute() {
                path.clone()
            } else {
                self.ctx.cwd.join(&path)
            };
            let candidate = {
                let snapshots = self
                    .ctx
                    .snapshots
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                snapshots.latest_read_snapshot(&full)
            };
            let Some((tag, snapshot_text)) = candidate else {
                continue;
            };
            let Ok(current) = tokio::fs::read_to_string(&full).await else {
                continue;
            };
            let normalized = crate::tools::snapshot::normalize_snapshot_text(&current);
            if normalized == snapshot_text {
                continue; // 幂等：磁盘内容已与基线一致。
            }
            // N2 修复：atomic_replace 以新临时文件替换，会丢失原文件权限
            // （可执行脚本会丢 +x）。先取权限，替换后恢复。
            let original_permissions = std::fs::metadata(&full).map(|meta| meta.permissions()).ok();
            if let Err(error) =
                crate::session::atomic_file::atomic_replace(&full, snapshot_text.as_bytes())
            {
                self.ctx.log_event(serde_json::json!({
                    "type": "signal_rollback_error",
                    "path": full.display().to_string(),
                    "error": error.to_string(),
                }));
                continue;
            }
            if let Some(permissions) = original_permissions {
                let _ = std::fs::set_permissions(&full, permissions);
            }
            self.ctx
                .memo_mutation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            rolled_back.push(serde_json::json!({
                "path": full.display().to_string(),
                "to_tag": tag,
            }));
        }
        if !rolled_back.is_empty() {
            self.ctx.log_event(serde_json::json!({
                "type": "signal_rollback",
                "files": rolled_back,
            }));
            self.ctx.display.render_info(&format!(
                "Signal rollback: restored {} file(s) to their last read snapshot",
                rolled_back.len()
            ));
        }
        Ok(())
    }

    /// R3 策略重启（SIGNAL_RESPONSE_REDESIGN S5）：以 fork=false 启动 fresh 子代理，
    /// 只继承有界任务报告（目标/编辑路径/失败证据），用全新上下文重新规划。
    /// 成功返回子代理文本；不可用或失败返回 None（调用方降级为既有响应）。
    /// `belief` 为当前信念度，如实写入报告（禁止用占位值误导子代理）。
    async fn run_replan(&mut self, belief: f64) -> Result<Option<String>> {
        if !crate::config::SignalResponseTiers::from_env().allows_restart() {
            return Ok(None);
        }
        let s = self.ctx.config.signal.clone();
        if self.replan_attempts >= s.replan_max_attempts {
            return Ok(None);
        }
        if !self.ctx.tool_surface.has("SubAgent") {
            return Ok(None);
        }
        self.replan_attempts += 1;
        let mut child_cfg = self.sub_agent_config.clone();
        child_cfg.max_turns = s.replan_max_turns;
        child_cfg.max_tokens = child_cfg.max_tokens.min(s.replan_token_budget);
        let session_id = format!("replan_{}", crate::session::paths::chrono_session_id());
        let report = self.build_replan_report(belief);
        // B3 修复：子代理初始化失败必须降级（返回 None），不得把整轮炸成 Err。
        let executor = match crate::agent::sub_executor::SubAgentExecutor::new(
            self.ctx.clone(),
            session_id.clone(),
            false,
            child_cfg,
        )
        .await
        {
            Ok(executor) => executor,
            Err(error) => {
                self.ctx.log_event(serde_json::json!({
                    "type": "signal_replan_error",
                    "attempts": self.replan_attempts,
                    "session_id": session_id,
                    "error": error.to_string(),
                }));
                self.ctx.display.render_info(&format!(
                    "Signal replan unavailable: {error}; falling back to evidence/guard",
                ));
                return Ok(None);
            }
        };
        let result = executor.execute(report).await;
        self.ctx.log_event(serde_json::json!({
            "type": "signal_replan",
            "attempts": self.replan_attempts,
            "session_id": session_id,
            "status": result.status,
            "text_len": result.text.len(),
        }));
        if result.status != "ok" || result.text.trim().is_empty() {
            return Ok(None);
        }
        let injection = format!(
            "[replan] A fresh sub-agent re-analyzed the task with a clean context and produced \
             this revised plan. Treat it as new evidence, not a user request.\n{}",
            result.text
        );
        self.ctx.store.add_runtime_user(&injection).await?;
        self.ctx
            .display
            .render_info("Signal replan: fresh sub-agent produced a revised plan");
        Ok(Some(result.text))
    }

    /// 构造 R3 的有界任务报告：原始目标 + 编辑路径 + 失败证据（无父对话历史）。
    fn build_replan_report(&self, belief: f64) -> String {
        let budget = self.ctx.config.signal.evidence_max_chars.min(2_000);
        let evidence = self.signal_processor.evidence().render(budget, belief).text;
        let paths = self.signal_processor.evidence().edited_paths.join(", ");
        format!(
            "You are re-planning for a parent coding agent that is stuck in a failure loop.\n\
             Original user goal: {}\n\
             Files edited this turn: {}\n\
             Failure evidence:\n{}\n\
             Produce a short revised plan: diagnose the most likely root cause, then list the \
             next 3 concrete verification-first steps. Output the plan only; do not continue \
             the parent's work.",
            self.current_user_input,
            if paths.is_empty() { "(none)" } else { &paths },
            evidence,
        )
    }

    /// Phase 4: 根据 stop reason 决策本轮是否结束。
    /// 返回 `Some(TurnDecision)` 表示需要从 execute() 返回，`None` 表示继续循环。
    async fn decide_next(
        &mut self,
        stop: &str,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
    ) -> Result<Option<TurnDecision>> {
        // 更新标题栏信念度
        let _ = self.ctx.stats.flush_if_dirty().await;
        let current_belief = belief.as_ref().map_or(0.0, |bt| bt.belief());
        crate::ui::render_title_snapshot(&self.ctx, self.llm.model_label(), current_belief).await;

        match stop {
            "tool_use" | "tool_calls" => {
                // DecisionEngine 决策是否注入（含内部冷却逻辑）
                let signal_enabled = crate::config::SignalMode::from_env().enabled();
                if let Some(decision) = self.decide_signal_recovery(signal_enabled, belief).await? {
                    return Ok(Some(decision));
                }
                Ok(None) // 继续循环
            }
            "end_turn" | "stop" | "done" => {
                if !self.todo_final_reminder_sent
                    && self
                        .ctx
                        .todo_store
                        .snapshot()
                        .items
                        .iter()
                        .any(|item| item.status == crate::session::todo::TodoStatus::InProgress)
                    && let Some(provider) = self.ctx.todo_advance_provider()
                {
                    self.ctx
                        .store
                        .add_runtime_user(&format!(
                            "<todo-final-reminder>Todo items remain in_progress. Before finishing, call {provider} to complete verified work or pause work that is no longer active. If the work is blocked and should remain active, state that explicitly.</todo-final-reminder>"
                        ))
                        .await?;
                    self.todo_final_reminder_sent = true;
                    return Ok(None);
                }
                self.ctx.display.render_stop_with_reason(stop);
                Ok(Some(TurnDecision::Stop))
            }
            "error" | "max_tokens" | "length" => {
                self.ctx.display.render_stop_with_reason(stop);
                Ok(Some(TurnDecision::Failed(format!("stop: {stop}"))))
            }
            _ => {
                self.ctx.display.render_stop_with_reason(stop);
                Ok(Some(TurnDecision::Stop))
            }
        }
    }

    async fn decide_signal_recovery(
        &mut self,
        signal_enabled: bool,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
    ) -> Result<Option<TurnDecision>> {
        if !signal_enabled {
            return Ok(None);
        }
        if let Some(bt) = belief {
            let b = bt.belief();
            let hard = self.signal_processor.hard_failures() as usize;
            let soft = self.signal_processor.soft_failures() as usize;
            let decision = if self.guard_bypassed {
                // 守卫达到上限被绕过：强制一次证据注入，即使处于冷却。
                self.guard_bypassed = false;
                crate::agent::decision::Decision::Inject(
                    crate::agent::decision::RecoveryDirective {
                        belief: b,
                        severity: crate::agent::decision::RecoverySeverity::Warning,
                    },
                )
            } else {
                self.decision_engine.decide_with_signals(b, hard, soft)
            };
            match decision {
                crate::agent::decision::Decision::Inject(directive) => {
                    // R1 证据注入（SIGNAL_RESPONSE_REDESIGN）：注入轨迹事实而非命令。
                    let budget = self.ctx.config.signal.evidence_max_chars;
                    let batch = self.signal_processor.evidence().render(budget, b);
                    let fresh = self.signal_processor.evidence().is_fresh(batch.hash);
                    if fresh {
                        self.signal_processor
                            .evidence_mut()
                            .mark_injected(batch.hash);
                    }
                    let text = if fresh {
                        batch.text
                    } else {
                        format!(
                            "[trajectory]
- no new evidence since the last injection
[detector] belief {b:.2} (reference only)"
                        )
                    };
                    self.ctx
                        .display
                        .render_info(&format!("Injecting trajectory evidence (belief {b:.2})"));
                    self.ctx.store.add_runtime_user(&text).await?;
                    let tiers = crate::config::SignalResponseTiers::from_env();
                    // R2 状态操作：Warning 级先回滚循环窗口内的编辑（state-ops 档及以上）。
                    if directive.severity == crate::agent::decision::RecoverySeverity::Warning
                        && self.ctx.config.signal.rollback_enabled
                        && tiers.allows_state_ops()
                    {
                        self.apply_rollback().await?;
                    }
                    // R3 策略重启：同一输入内连续第 2 次 Warning 时，用 fresh 子代理
                    // 重新规划；成功则注入新计划并跳过守卫（新证据重置策略）。
                    let is_warning =
                        directive.severity == crate::agent::decision::RecoverySeverity::Warning;
                    if is_warning {
                        self.warning_count += 1;
                    }
                    let mut replanned = false;
                    if is_warning && self.warning_count >= 2 && tiers.allows_restart() {
                        replanned = self.run_replan(b).await?.is_some();
                        if replanned {
                            // 新上下文 = 新证据基线（SIGNAL_RESPONSE_REDESIGN R3）。
                            bt.reset();
                        }
                    }
                    // 恢复守卫降级为状态门：仅在 Warning 级启用且未成功重规划时。
                    self.signal_recovery_guard =
                        is_warning && !replanned && tiers.allows_state_ops();
                }
                crate::agent::decision::Decision::Abort => {
                    let tiers = crate::config::SignalResponseTiers::from_env();
                    // 非交互环境先降级为 R3 策略重启（restart 档及以上）。
                    if tiers.allows_restart()
                        && !self.ctx.config.interactive
                        && self.run_replan(b).await?.is_some()
                    {
                        // 策略重启成功：重置信念并继续本轮（新证据基线）。
                        bt.reset();
                        self.ctx.display.render_info(&format!(
                            "DecisionEngine: belief {b:.2} — retrying with a fresh replan instead of handing over",
                        ));
                        return Ok(None);
                    }
                    // R4 用户接管仅在全档位启用；低档位直接失败。
                    if !tiers.allows_handover() {
                        self.ctx
                            .display
                            .render_error(&format!("DecisionEngine: aborting (belief {b:.2})."));
                        return Ok(Some(TurnDecision::Failed(
                            "aborted by DecisionEngine".into(),
                        )));
                    }
                    let budget = self.ctx.config.signal.evidence_max_chars;
                    let batch = self.signal_processor.evidence().render(budget, b);
                    let report = serde_json::json!({
                        "type": "signal_handover",
                        "belief": b,
                        "edited_paths": self.signal_processor.evidence().edited_paths,
                        "evidence": batch.text,
                        "options": ["retry", "rollback_and_retry", "replan", "abandon"],
                    });
                    self.ctx.log_event(report);
                    self.ctx.display.render_error(&format!(
                        "DecisionEngine: handing over (belief {b:.2}).\n{}",
                        batch.text
                    ));
                    return Ok(Some(TurnDecision::Failed(
                        "signal handover: reliability belief fell below the abort threshold".into(),
                    )));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Execute a full turn: send user input, stream response, execute tools, decide next.
    pub async fn execute(
        &mut self,
        user_input: &str,
        mut belief: Option<&mut crate::agent::belief::BeliefTracker>,
    ) -> Result<(TurnDecision, Vec<TurnEffect>)> {
        // New user intent: reset storm breaker window, compact guard, and decision engine
        self.tools.reset_storm();
        self.tool_call_count = 0;
        self.compactor.reset();
        self.signal_processor.reset();
        self.decision_engine.reset();
        self.signal_recovery_guard = false;
        self.guard_blocks = 0;
        self.guard_bypassed = false;
        self.warning_count = 0;
        self.replan_attempts = 0;
        self.current_user_input = user_input.to_string();
        self.todo_final_reminder_sent = false;
        self.todo_progress_reminder_sent = false;
        self.successful_work_calls_since_todo_advance = 0;
        self.final_text.clear();
        self.final_thinking.clear();

        let mut messages = self.ctx.compaction.active_messages().await?;
        self.reconcile_todo_state(&mut messages).await?;
        self.ctx.store.add_user(user_input).await?;
        self.ctx.stats.record_turn().await;
        self.ctx
            .log_event(serde_json::json!({"type":"user_input","content":user_input}));

        let mut turn = 0;
        let mut effects = Vec::new();
        let mut overflow_recovery_attempted = false;
        let max_turns = self.ctx.max_turns() as usize;

        let (mut system_prompt, mut tools_json) = self.ensure_prefix()?;
        messages = self.ctx.compaction.active_messages().await?;
        while turn < max_turns {
            turn += 1;

            // Phase 0: 上下文压缩
            self.try_compact("auto", &mut messages, &mut system_prompt, &mut tools_json)
                .await?;
            let mut request_messages = self.project_request_messages(&messages)?;
            if !self.compactor.compacted_this_turn() {
                let estimated_tokens = crate::llm::transport::estimate_openai_context_tokens(
                    &request_messages,
                    &tools_json,
                    &system_prompt,
                )?;
                if estimated_tokens
                    > crate::session::compaction::request_input_limit(&self.ctx.config)
                {
                    self.try_compact(
                        "preflight",
                        &mut messages,
                        &mut system_prompt,
                        &mut tools_json,
                    )
                    .await?;
                    request_messages = self.project_request_messages(&messages)?;
                }
            }
            let estimated_tokens = crate::llm::transport::estimate_openai_context_tokens(
                &request_messages,
                &tools_json,
                &system_prompt,
            )?;
            let input_limit = crate::session::compaction::request_input_limit(&self.ctx.config);
            if estimated_tokens > input_limit {
                anyhow::bail!(
                    "context remains over the request input budget after compaction: \
                     estimated {estimated_tokens} tokens, limit {input_limit}"
                );
            }
            // 当前请求上下文估计（每轮更新，随 usage 事件广播给前端指标行）
            let current_context_tokens = estimated_tokens;

            // Phase 1: LLM 流式响应
            let stream_output = loop {
                match self
                    .stream_llm_response(
                        &request_messages,
                        &system_prompt,
                        &tools_json,
                        current_context_tokens,
                    )
                    .await
                {
                    Ok(output) => break output,
                    Err(error)
                        if error.downcast_ref::<ContextOverflowError>().is_some()
                            && !overflow_recovery_attempted
                            && !self.compactor.compacted_this_turn() =>
                    {
                        overflow_recovery_attempted = true;
                        if !self
                            .try_compact(
                                "overflow",
                                &mut messages,
                                &mut system_prompt,
                                &mut tools_json,
                            )
                            .await?
                        {
                            return Err(error);
                        }
                        request_messages = self.project_request_messages(&messages)?;
                        self.ctx
                            .display
                            .render_info("Context overflow detected; compacted and retrying once.");
                    }
                    Err(error) => return Err(error),
                }
            };
            let StreamOutput {
                text,
                thinking,
                mut calls,
                mut stop,
                usage,
            } = stream_output;
            self.final_text.push_str(&text);
            self.final_thinking.push_str(&thinking);

            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx.display.render_stop_with_reason("interrupted");
                return Ok((TurnDecision::Interrupted, effects));
            }

            // Phase 1b: 从 thinking/text 回收漏报的工具调用
            let recovered_calls;
            (calls, recovered_calls) = self.scavenge_calls(&thinking, &text, calls);
            if recovered_calls
                && !calls.is_empty()
                && matches!(stop.as_str(), "end_turn" | "stop" | "done")
            {
                stop = "tool_use".into();
            }

            // Phase 2: 持久化 assistant 消息 + 用量
            self.persist_assistant(&text, &thinking, &calls, &usage)
                .await?;

            // Phase 3: 工具执行
            let plan_compaction_trigger = if calls.is_empty() {
                None
            } else {
                self.execute_tools_inner(calls, belief.as_deref_mut(), &mut effects)
                    .await?
            };

            if let Some(trigger) = plan_compaction_trigger {
                messages = self.ctx.compaction.active_messages().await?;
                self.try_compact(trigger, &mut messages, &mut system_prompt, &mut tools_json)
                    .await?;
            }

            // Phase 4: 决策 — 继续或结束
            if let Some(decision) = self.decide_next(&stop, belief.as_deref_mut()).await? {
                return Ok((decision, effects));
            }
            // tool_use 路径：重新加载 messages 继续循环
            messages = self.ctx.compaction.active_messages().await?;
        }

        Ok((TurnDecision::MaxTurnsExceeded, effects))
    }
}

fn is_context_overflow_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "maximum context size",
        "context window exceeded",
        "exceeds the context window",
        "context length exceeded",
        "too many tokens",
        "prompt is too long",
        "input is too long",
        "max sequence length",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn positive_duration(seconds: i32) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds as u64))
}

fn blocked_by_signal_recovery(
    call: ToolCallEvent,
    content: String,
) -> crate::tools::runner::ToolRunResult {
    crate::tools::runner::ToolRunResult {
        tool_use_id: call.id,
        tool_name: "SignalRecoveryGuard".to_string(),
        tool_args: call.fields,
        content,
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_description: None,
        sub_agent_fork: false,
        exit_code: None,
        success: false,
        error_code: Some(crate::tools::metadata::ToolErrorKind::Aborted),
        result_kind: crate::tools::metadata::ToolResultKind::Control,
        presentation: None,
        artifacts: Vec::new(),
        signals: Vec::new(),
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::BackendLlmClient;
    use crate::llm::mock::MockLlmClient;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct OverflowOnceClient {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmClient for OverflowOnceClient {
        fn model(&self) -> &str {
            "private-64k"
        }

        async fn stream(
            &self,
            _ctx: &AgentSharedContext,
            _messages_json: &[serde_json::Value],
            _tools_json: &[serde_json::Value],
            _system_prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>> {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                anyhow::bail!("HTTP 400: maximum context length exceeded");
            }
            Ok(Box::new(futures::stream::iter(vec![
                Ok(Event::Text(crate::protocol::TextEvent {
                    content: "recovered".into(),
                })),
                Ok(Event::Stop(crate::protocol::StopEvent {
                    reason: "stop".into(),
                })),
            ])))
        }
    }

    #[tokio::test]
    async fn signal_recovery_decision_noops_when_signal_mode_disabled() {
        let ctx = crate::regression::test_context_for_agent("turn-signal-disabled")
            .await
            .unwrap();
        let llm = Arc::new(BackendLlmClient::new(
            Arc::new(MockLlmClient::new("flash", vec![])),
            "flash",
            Some("flash".into()),
        ));
        let mut executor = TurnExecutor::new(ctx, llm);
        let mut belief = crate::agent::belief::BeliefTracker::new(16);
        belief.observe(&[crate::guard::collector::Signal {
            kind: crate::guard::collector::SignalKind::ToolFailed,
            severity: 1.0,
            source: "Bash".into(),
            detail: "failed".into(),
            source_tool: "Bash".into(),
            exit_code: Some(1),
            matched_pattern: None,
            message: "failed".into(),
        }]);

        let decision = executor
            .decide_signal_recovery(false, Some(&mut belief))
            .await
            .unwrap();
        assert!(decision.is_none());
    }

    #[tokio::test]
    async fn todo_sync_is_appended_once_when_file_revision_is_ahead() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("turn-todo-sync").await?;
        ctx.store.add_user("existing stable history").await?;
        let stable_prefix = ctx.compaction.active_messages().await?;
        ctx.todo_store.apply_structure(
            0,
            crate::session::todo::TodoChanges {
                add: vec![crate::session::todo::TodoAdd {
                    content: "active".into(),
                }],
                ..Default::default()
            },
        )?;
        ctx.todo_store.advance(
            1,
            crate::session::todo::TodoTransitions {
                activate: vec!["T0001".into()],
                ..Default::default()
            },
        )?;
        let llm = Arc::new(BackendLlmClient::new(
            Arc::new(MockLlmClient::new("flash", vec![])),
            "flash",
            Some("flash".into()),
        ));
        let executor = TurnExecutor::new(ctx.clone(), llm);
        let mut messages = ctx.compaction.active_messages().await?;

        assert!(executor.reconcile_todo_state(&mut messages).await?);
        assert!(!executor.reconcile_todo_state(&mut messages).await?);
        assert!(messages.starts_with(&stable_prefix));
        assert_eq!(crate::session::todo::visible_revision(&messages)?, 2);
        assert_eq!(
            ctx.store
                .lines()
                .await?
                .iter()
                .filter(|message| { message["_mink"]["todo_state_kind"].as_str() == Some("sync") })
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn todo_final_guard_reminds_once_but_does_not_force_a_loop() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("turn-todo-final-guard").await?;
        ctx.todo_store.apply_structure(
            0,
            crate::session::todo::TodoChanges {
                add: vec![crate::session::todo::TodoAdd {
                    content: "active".into(),
                }],
                ..Default::default()
            },
        )?;
        ctx.todo_store.advance(
            1,
            crate::session::todo::TodoTransitions {
                activate: vec!["T0001".into()],
                ..Default::default()
            },
        )?;
        let llm = Arc::new(BackendLlmClient::new(
            Arc::new(MockLlmClient::new("flash", vec![])),
            "flash",
            Some("flash".into()),
        ));
        let mut executor = TurnExecutor::new(ctx.clone(), llm);

        assert!(executor.decide_next("stop", None).await?.is_none());
        assert_eq!(
            executor.decide_next("stop", None).await?,
            Some(TurnDecision::Stop)
        );
        let messages = ctx.store.lines().await?;
        assert_eq!(
            messages
                .iter()
                .filter(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("<todo-final-reminder>"))
                })
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn todo_progress_guard_appends_at_most_one_reminder_per_turn() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("turn-todo-progress-guard").await?;
        ctx.todo_store.apply_structure(
            0,
            crate::session::todo::TodoChanges {
                add: vec![crate::session::todo::TodoAdd {
                    content: "active".into(),
                }],
                ..Default::default()
            },
        )?;
        ctx.todo_store.advance(
            1,
            crate::session::todo::TodoTransitions {
                activate: vec!["T0001".into()],
                ..Default::default()
            },
        )?;
        let llm = Arc::new(BackendLlmClient::new(
            Arc::new(MockLlmClient::new("flash", vec![])),
            "flash",
            Some("flash".into()),
        ));
        let mut executor = TurnExecutor::new(ctx.clone(), llm);
        executor.successful_work_calls_since_todo_advance = 8;

        executor.maybe_append_todo_progress_reminder().await?;
        executor.maybe_append_todo_progress_reminder().await?;
        let messages = ctx.store.lines().await?;
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("<todo-progress-reminder>")
        );
        Ok(())
    }

    #[tokio::test]
    async fn context_overflow_compacts_and_retries_only_once() -> anyhow::Result<()> {
        let summary_backend = Arc::new(MockLlmClient::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(crate::protocol::TextEvent {
                    content: "Task focus: recover\nLatest request: continue\nProgress: compacted\nTool evidence: none\nReflections: none".into(),
                })),
                Ok(Event::Stop(crate::protocol::StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ));
        let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
            "turn-overflow-recovery",
            |config| {
                config.max_context_tokens = 64_000;
                config.context_reserve_tokens = 12_000;
                config.context_compact_tail_tokens = 1_000;
                config.context_compact_max_output_tokens = 2_048;
            },
            summary_backend,
        )
        .await?;
        for index in 0..3 {
            ctx.store
                .add_user(&format!("old request {index}: {}", "x".repeat(1_000)))
                .await?;
            ctx.store
                .add_assistant(
                    &format!("old response {index}: {}", "y".repeat(1_000)),
                    "",
                    &[],
                )
                .await?;
        }
        let llm = Arc::new(OverflowOnceClient {
            calls: AtomicUsize::new(0),
        });
        let mut executor = TurnExecutor::new(ctx.clone(), llm.clone());

        let (decision, _) = executor.execute("continue", None).await?;

        assert_eq!(decision, TurnDecision::Stop);
        assert_eq!(llm.calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(ctx.store.lines().await?.len(), 8);
        assert!(ctx.compaction.current_summary()?.is_some());
        Ok(())
    }

    #[test]
    fn context_overflow_classifier_is_specific() {
        assert!(is_context_overflow_message(
            "HTTP 400: context_length_exceeded"
        ));
        assert!(is_context_overflow_message(
            "This model's maximum context length is 65536 tokens"
        ));
        assert!(!is_context_overflow_message(
            "HTTP 400: invalid tool schema"
        ));
        assert!(!is_context_overflow_message("request timed out"));
    }

    #[tokio::test]
    async fn context_overflow_after_visible_output_is_not_recoverable() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("turn-partial-overflow").await?;
        let backend = Arc::new(MockLlmClient::new(
            "flash",
            vec![vec![
                Ok(Event::Text(crate::protocol::TextEvent {
                    content: "partial".into(),
                })),
                Ok(Event::Retry(crate::protocol::RetryEvent {})),
                Ok(Event::Error(crate::protocol::ErrorEvent {
                    message: "maximum context length exceeded".into(),
                })),
            ]],
        ));
        let llm = Arc::new(BackendLlmClient::new(
            backend,
            "flash",
            Some("flash".into()),
        ));
        let mut executor = TurnExecutor::new(ctx, llm);

        let error = match executor.stream_llm_response(&[], "", &[], 0).await {
            Ok(_) => panic!("overflow after partial output should fail"),
            Err(error) => error,
        };

        assert!(error.downcast_ref::<ContextOverflowError>().is_none());
        Ok(())
    }
}
