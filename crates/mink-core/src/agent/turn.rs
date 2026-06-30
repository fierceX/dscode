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
    /// Set after a signal injection. The next tool batch must observe before mutating.
    signal_recovery_guard: bool,
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
        Self {
            ctx: ctx.clone(),
            llm,
            tools,
            prefix,
            compactor: crate::agent::compactor::TurnCompactor::new(ctx.clone()),
            signal_processor: crate::agent::tool_signals::ToolSignalProcessor::new(),
            plan_actions: crate::agent::plan_actions::PlanActionHandler::new(ctx.clone()),
            sub_agents: crate::agent::sub_coordinator::SubAgentCoordinator::new(ctx.clone()),
            tool_call_count: 0,
            decision_engine: crate::agent::decision::DecisionEngine::new(),
            signal_recovery_guard: false,
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

    fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
        self.prefix.ensure()
    }

    /// 尝试上下文压缩（auto 或 preflight）。成功时更新 messages/system_prompt/tools_json。
    /// 返回 true 表示进行了压缩。
    async fn try_compact(
        &mut self,
        trigger: &str,
        messages: &mut Vec<serde_json::Value>,
        system_prompt: &mut String,
        tools_json: &mut Vec<serde_json::Value>,
    ) -> Result<bool> {
        self.compactor
            .maybe_compact(trigger, messages, system_prompt, tools_json, &self.prefix)
            .await
    }

    /// Phase 1: 发送 LLM 请求并流式读取响应，返回 `StreamOutput`。
    /// 网络/协议错误通过 `bail!` 传播，由调用方转为 `TurnDecision::Failed`。
    async fn stream_llm_response(
        &mut self,
        messages: &[serde_json::Value],
        system_prompt: &str,
        tools_json: &[serde_json::Value],
    ) -> anyhow::Result<StreamOutput> {
        let mut stream = self
            .llm
            .stream(&self.ctx, messages, tools_json, system_prompt)
            .await?;

        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCallEvent> = Vec::new();
        let mut stop = String::new();
        let mut usage: Option<UsageEvent> = None;
        let mut saw_stop = false;
        let mut saw_any_event = false;
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
                    self.ctx
                        .log_event(serde_json::json!({"type":"thinking","content":t.content}));
                    self.ctx.display.render_thinking(&t.content);
                    thinking.push_str(&t.content);
                }
                Event::Text(t) => {
                    self.ctx
                        .log_event(serde_json::json!({"type":"text","content":t.content}));
                    self.ctx.display.render_text(&t.content);
                    text.push_str(&t.content);
                }
                Event::ToolCall(call) => {
                    self.ctx.log_event(serde_json::json!({"type":"tool_call","name":call.name,"id":call.id,"input":call.input_json}));
                    let summary = build_tool_call_summary(&call.name, &call.fields);
                    self.ctx.display.render_tool_call(&call.name, &summary);
                    calls.push(call);
                }
                Event::Usage(u) => {
                    self.ctx.log_event(serde_json::json!({"type":"usage","input_tokens":u.input_tokens, "output_tokens":u.output_tokens, "cache_read_input_tokens":u.cache_read_input_tokens, "cache_creation_input_tokens":u.cache_creation_input_tokens, "kind":"agent"}));
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
    /// 处理信号采集、信念追踪、PlanClear/PlanConfirm、子代理生成与收集、持久化。
    #[allow(clippy::too_many_arguments)]
    async fn execute_tools_inner(
        &mut self,
        calls: Vec<ToolCallEvent>,
        mut belief: Option<&mut crate::agent::belief::BeliefTracker>,
        effects: &mut Vec<TurnEffect>,
    ) -> Result<()> {
        self.tool_call_count += calls.len() as u32;
        let (calls_to_execute, mut guarded_results) = self.apply_signal_recovery_guard(calls);
        let mut results = if calls_to_execute.is_empty() {
            Vec::new()
        } else {
            self.tools.execute_all(calls_to_execute).await?
        };
        guarded_results.append(&mut results);
        let results = guarded_results;

        let mut prepared_results = Vec::new();
        for mut result in results {
            let model_label = self.llm.model_label();
            self.signal_processor
                .process(&mut result, belief.as_deref_mut(), &self.ctx, model_label)
                .await;
            self.plan_actions
                .handle(&mut result, effects, &self.prefix)
                .await;
            prepared_results.push(result);
        }

        let processed_results = self.sub_agents.process(prepared_results).await;

        let tool_results: Vec<ToolResult> = processed_results
            .iter()
            .map(|r| ToolResult {
                tool_use_id: r.tool_use_id.clone(),
                tool_name: r.tool_name.clone(),
                tool_args: r.tool_args.clone(),
                content: r.content.clone(),
                conv_content: r.conv_content.clone(),
            })
            .collect();

        self.ctx.store.add_tool_results(&tool_results).await?;

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
                "tool_use_id": r.tool_use_id,
                "name": r.tool_name,
                "content": r.content,
            }));
            self.ctx
                .display
                .render_tool_result_detail(&ToolResultDisplay {
                    tool_name: &r.tool_name,
                    content_preview: &preview,
                    content: &r.content,
                    tool_use_id: Some(&r.tool_use_id),
                    exit_code: r.exit_code,
                });
        }
        Ok(())
    }

    fn apply_signal_recovery_guard(
        &mut self,
        calls: Vec<ToolCallEvent>,
    ) -> (Vec<ToolCallEvent>, Vec<crate::tools::runner::ToolRunResult>) {
        if !self.signal_recovery_guard || calls.is_empty() {
            return (calls, Vec::new());
        }

        self.signal_recovery_guard = false;
        let mut iter = calls.into_iter();
        let first = iter
            .next()
            .expect("signal recovery guard already checked calls is non-empty");

        if is_recovery_blocked_tool(&first.name) {
            self.ctx.log_event(serde_json::json!({
                "type": "signal_recovery_guard",
                "action": "blocked_first_mutation",
                "tool": first.name.clone(),
                "tool_use_id": first.id.clone(),
                "reason": "SIGNAL_RECOVERY requires inspection before the first file mutation",
            }));
            let remaining: Vec<ToolCallEvent> = iter.collect();
            return (remaining, vec![blocked_by_signal_recovery(first)]);
        }

        let mut allowed = Vec::new();
        allowed.push(first);
        allowed.extend(iter);
        (allowed, Vec::new())
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
                let signal_enabled = crate::agent::signal_mode::SignalMode::from_env().enabled();
                if let Some(decision) = self
                    .decide_signal_recovery(signal_enabled, belief.as_deref())
                    .await?
                {
                    return Ok(Some(decision));
                }
                Ok(None) // 继续循环
            }
            "end_turn" | "stop" => {
                self.ctx.display.render_stop();
                Ok(Some(TurnDecision::Stop))
            }
            "error" | "max_tokens" | "length" => {
                self.ctx.display.render_stop();
                Ok(Some(TurnDecision::Failed(format!("stop: {stop}"))))
            }
            _ => {
                self.ctx.display.render_stop();
                Ok(Some(TurnDecision::Stop))
            }
        }
    }

    async fn decide_signal_recovery(
        &mut self,
        signal_enabled: bool,
        belief: Option<&crate::agent::belief::BeliefTracker>,
    ) -> Result<Option<TurnDecision>> {
        if !signal_enabled {
            return Ok(None);
        }
        if let Some(bt) = belief {
            let b = bt.belief();
            match self.decision_engine.decide(b, &bt.recent_errors) {
                crate::agent::decision::Decision::Inject(msg) => {
                    let recent = if bt.recent_errors.is_empty() {
                        String::new()
                    } else {
                        format!(
                            ": recent issues: {}",
                            bt.recent_errors
                                .iter()
                                .rev()
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("; ")
                        )
                    };
                    self.ctx
                        .display
                        .render_info(&format!("Injecting hint (belief {:.2}){}", b, recent));
                    self.ctx.store.add_user(&msg).await?;
                    self.signal_recovery_guard = true;
                }
                crate::agent::decision::Decision::Abort => {
                    self.ctx
                        .display
                        .render_error(&format!("DecisionEngine: aborting (belief {:.2}).", b));
                    return Ok(Some(TurnDecision::Failed(
                        "aborted by DecisionEngine".into(),
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

        self.ctx.store.add_user(user_input).await?;
        self.ctx.stats.record_turn().await;
        self.ctx
            .log_event(serde_json::json!({"type":"user_input","content":user_input}));

        let mut turn = 0;
        let mut effects = Vec::new();
        let max_turns = self.ctx.max_turns() as usize;

        let (mut system_prompt, mut tools_json) = self.ensure_prefix()?;
        let mut messages = self.ctx.store.lines().await?;

        while turn < max_turns {
            turn += 1;

            // Phase 0: 上下文压缩
            self.try_compact("auto", &mut messages, &mut system_prompt, &mut tools_json)
                .await?;
            if !self.compactor.compacted_this_turn() {
                let estimated_tokens: usize = messages
                    .iter()
                    .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
                    .sum::<usize>()
                    + system_prompt.len() / 4;
                let max_ctx = self.ctx.config.max_context_tokens;
                if max_ctx > 0 && estimated_tokens > max_ctx * 95 / 100 {
                    self.try_compact(
                        "preflight",
                        &mut messages,
                        &mut system_prompt,
                        &mut tools_json,
                    )
                    .await?;
                }
            }

            // Phase 1: LLM 流式响应
            let StreamOutput {
                text,
                thinking,
                mut calls,
                mut stop,
                usage,
            } = self
                .stream_llm_response(&messages, &system_prompt, &tools_json)
                .await?;

            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx.display.render_stop();
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
            if !calls.is_empty() {
                self.execute_tools_inner(calls, belief.as_deref_mut(), &mut effects)
                    .await?;
            }

            // Phase 4: 决策 — 继续或结束
            if let Some(decision) = self.decide_next(&stop, belief.as_deref_mut()).await? {
                return Ok((decision, effects));
            }
            // tool_use 路径：重新加载 messages 继续循环
            messages = self.ctx.store.lines().await?;
        }

        Ok((TurnDecision::MaxTurnsExceeded, effects))
    }
}

fn is_recovery_blocked_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write")
}

fn positive_duration(seconds: i32) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds as u64))
}

fn blocked_by_signal_recovery(call: ToolCallEvent) -> crate::tools::runner::ToolRunResult {
    crate::tools::runner::ToolRunResult {
        tool_use_id: call.id,
        tool_name: "SignalRecoveryGuard".to_string(),
        tool_args: call.fields,
        content: "SIGNAL_RECOVERY guard: the requested Edit/Write was not executed. Inspect current state first with Read, Grep, Glob, or a focused Bash verification/state command before mutating.".to_string(),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_description: None,
        sub_agent_fork: false,
        exit_code: None,
        signals: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::BackendLlmClient;
    use crate::llm::mock::MockLlmClient;
    use std::sync::Arc;

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
            .decide_signal_recovery(false, Some(&belief))
            .await
            .unwrap();
        assert!(decision.is_none());
    }
}
