use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::context::AgentSharedContext;
use crate::llm::client::LlmClient;
use crate::protocol::{Event, ToolCallEvent, UsageEvent};
use crate::session::prefix::ImmutablePrefix;
use crate::session::store::{build_tool_call_summary, first_line, ToolResult};
use crate::tools::runner::ToolRunner;
use crate::util::truncate_str;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;

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
    signal_collector: crate::guard::collector::SignalCollector,
    compacted_this_turn: bool,
    tool_call_count: u32,
    tool_error_count: u32,
    signals: Vec<crate::guard::collector::Signal>,
    /// 决策引擎（含冷却逻辑，由引擎内部管理）。
    decision_engine: crate::agent::decision::DecisionEngine,
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
    Failed(String),
}

impl TurnExecutor {
    pub fn new(ctx: Arc<AgentSharedContext>, llm: Arc<dyn LlmClient>) -> Self {
        let tools = Arc::new(ToolRunner::new(Arc::new(crate::context::ToolContext::from(ctx.as_ref()))));
        Self { ctx, llm, tools, signal_collector: crate::guard::collector::SignalCollector::new(), compacted_this_turn: false, tool_call_count: 0, tool_error_count: 0, signals: Vec::new(), decision_engine: crate::agent::decision::DecisionEngine::new() }
    }

    /// Return the total number of tool calls made during this turn.
    pub fn tool_call_count(&self) -> u32 {
        self.tool_call_count
    }

    /// Number of tool calls that produced at least one tool_error signal.
    pub fn tool_error_count(&self) -> u32 {
        self.tool_error_count
    }

    /// Collected signals from all tool calls in this turn.
    pub fn collected_signals(&self) -> &[crate::guard::collector::Signal] {
        &self.signals
    }

    /// Build or reuse the ImmutablePrefix. Returns the current system_prompt and tools_json.
    /// The prefix is invalidated after plan/summary changes (PlanClear, PlanConfirm, compaction)
    /// and rebuilt on the next call to this method.
    fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
        loop {
            let mut guard = self.ctx.immutable_prefix.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref prefix) = *guard {
                // Verify fingerprint; on mismatch force-rebuild instead of crashing.
                if prefix.verify_fingerprint() {
                    return Ok((prefix.system_prompt().to_string(), prefix.tools_json().to_vec()));
                }
                // Fingerprint mismatch: mark stale and rebuild
                *guard = None;
                drop(guard);
                continue;
            }
            // Build fresh
            let system_prompt = crate::prompt::Builder {
                cwd: self.ctx.cwd.clone(),
                home: self.ctx.home.clone(),
                skills: self.ctx.config.skills.clone(),
                summary_file: self.ctx.summary_path.clone(),
                plan_file: self.ctx.plan_path.clone(),
                plan_draft_file: self.ctx.plan_draft_path.clone(),
            }
            .build_system_prompt()?;
            let tools_json = serde_json::from_str::<Vec<serde_json::Value>>(crate::assets::TOOLS_JSON)
                .unwrap_or_default();
            *guard = Some(ImmutablePrefix::new(system_prompt.clone(), tools_json.clone()));
            return Ok((system_prompt, tools_json));
        }
    }

    /// Mark the prefix as stale so it is rebuilt on the next ensure_prefix() call.
    fn invalidate_prefix(&self) {
        *self.ctx.immutable_prefix.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        if self.compacted_this_turn {
            return Ok(false);
        }
        let stats = self.ctx.stats.snapshot().await;
        let compacted = self.ctx.compaction.evaluate_and_compact(
            trigger,
            stats.current_context_tokens as usize,
        ).await;
        if let Ok((did_compact, _)) = compacted
            && did_compact {
                self.compacted_this_turn = true;
                self.invalidate_prefix();
                *messages = self.ctx.store.lines().await?;
                (*system_prompt, *tools_json) = self.ensure_prefix()?;
                return Ok(true);
            }
        Ok(false)
    }

    /// Phase 1: 发送 LLM 请求并流式读取响应，返回 `StreamOutput`。
    /// 网络/协议错误通过 `bail!` 传播，由调用方转为 `TurnDecision::Failed`。
    async fn stream_llm_response(
        &mut self,
        messages: &[serde_json::Value],
        system_prompt: &str,
        tools_json: &[serde_json::Value],
    ) -> anyhow::Result<StreamOutput> {
        let mut stream = self.llm.stream(&self.ctx, messages, tools_json, system_prompt).await?;

        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCallEvent> = Vec::new();
        let mut stop = String::new();
        let mut usage: Option<UsageEvent> = None;

        while let Some(result) = stream.next().await {
            let evt = result?;

            match evt {
                Event::Thinking(t) => {
                    self.ctx.log_event(serde_json::json!({"type":"thinking","content":t.content}));
                    self.ctx.display.render_thinking(&t.content);
                    thinking.push_str(&t.content);
                }
                Event::Text(t) => {
                    self.ctx.log_event(serde_json::json!({"type":"text","content":t.content}));
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
                Event::Stop(s) => {
                    self.ctx.log_event(serde_json::json!({"type":"stop","reason":s.reason}));
                    stop = s.reason;
                    break;
                }
                Event::Error(e) => {
                    self.ctx.log_event(serde_json::json!({"type":"error","message":e.message}));
                    anyhow::bail!("{}", e.message);
                }
                Event::Retry(_) => {
                    self.ctx.log_event(serde_json::json!({"type":"retry"}));
                    text.clear();
                    thinking.clear();
                    calls.clear();
                    stop.clear();
                    usage = None;
                    self.ctx.display.render_retry();
                }
            }
        }

        drop(stream);
        Ok(StreamOutput { text, thinking, calls, stop, usage })
    }

    /// Phase 1b: 从 thinking/text 中回收漏报的工具调用（scavenge）。
    fn scavenge_calls(&self, thinking: &str, text: &str, mut calls: Vec<ToolCallEvent>) -> Vec<ToolCallEvent> {
        if thinking.is_empty() && text.is_empty() {
            return calls;
        }
        let (scavenged, notes) = crate::repair::scavenge_combined(
            if thinking.is_empty() { None } else { Some(thinking) },
            if text.is_empty() { None } else { Some(text) },
            4,
        );
        for sc in &scavenged {
            if !calls.iter().any(|c| c.name == sc.name) {
                let cid = format!("scavenged_{}", calls.len());
                let input_json: serde_json::Value =
                    serde_json::from_str(&sc.arguments).unwrap_or_default();
                calls.push(ToolCallEvent {
                    name: sc.name.clone(),
                    id: cid,
                    input_json,
                    fields: std::collections::BTreeMap::new(),
                    order: Vec::new(),
                });
            }
        }
        for note in &notes {
            self.ctx.log_event(serde_json::json!({"type":"scavenge","note":note}));
        }
        calls
    }

    /// Phase 2: 持久化 assistant 消息 + 用量统计。
    async fn persist_assistant(&self, text: &str, thinking: &str, calls: &[ToolCallEvent], usage: &Option<UsageEvent>) -> Result<()> {
        self.ctx.store.add_assistant(text, thinking, calls).await?;
        if let Some(u) = usage {
            let tier = crate::config::ModelTier::parse(self.llm.model())
                .unwrap_or(crate::config::ModelTier::Flash);
            self.ctx.stats.record_usage_with_tier(u, tier).await;
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
        let results = self.tools.execute_all(calls).await?;

        let mut processed_results = Vec::new();
        let (sub_result_tx, mut sub_result_rx) = tokio::sync::mpsc::unbounded_channel::<(usize, String, SubAgentResult)>();
        let mut sub_expected = 0usize;
        let sub_semaphore = Arc::new(tokio::sync::Semaphore::new(8));
        for mut result in results {
            let new_signals = self.signal_collector.collect(
                &result.tool_name, &result.content, result.exit_code, &result.content,
            );
            result.signals = new_signals;
            self.signals.extend(result.signals.clone());
            if let Some(ref mut bt) = belief {
                bt.observe(&result.signals);
                let cur_stats = self.ctx.stats.snapshot().await;
                self.ctx.display.render_title_update(
                    crate::config::resolve_model_label(self.llm.model()),
                    &crate::ui::StatsSnapshot {
                        current_turn_count: cur_stats.current_turn_count,
                        agent_request_count: cur_stats.agent_request_count,
                        total_input_tokens: cur_stats.total_input_tokens,
                        total_output_tokens: cur_stats.total_output_tokens,
                        current_context_tokens: cur_stats.current_context_tokens,
                        max_context_tokens: self.ctx.config.max_context_tokens as u64,
                        total_cache_read_tokens: cur_stats.total_cache_read_tokens,
                        total_cache_creation_tokens: cur_stats.total_cache_creation_tokens,
                        flash_cost_micros: cur_stats.flash_cost_micros,
                        pro_cost_micros: cur_stats.pro_cost_micros,
                        belief: bt.belief(),
                    },
                );
            }

            if result.signals.iter().any(|s| matches!(s.kind, crate::guard::collector::SignalKind::ToolError)) {
                self.tool_error_count += 1;
            }
            if result.tool_name == "PlanClear" {
                let _ = self.ctx.compaction.evaluate_and_compact("plan_clear", 0).await;
                let _ = tokio::fs::write(&self.ctx.plan_path, "").await;
                result.content = "Plan cleared.".to_string();
                effects.push(TurnEffect::PlanCleared);
                self.invalidate_prefix();
            }

            if result.tool_name == "PlanConfirm" {
                match tokio::fs::read(&self.ctx.plan_draft_path).await {
                    Ok(data) if !data.is_empty() => {
                        let _ = self.ctx.compaction.evaluate_and_compact("plan_confirm", 0).await;
                        let _ = tokio::fs::write(&self.ctx.plan_path, &data).await;
                        let _ = tokio::fs::write(&self.ctx.plan_draft_path, "").await;
                        result.content = "Plan confirmed and locked in.".to_string();
                    }
                    _ => {
                        result.content = "Error: no plan draft found to confirm.".to_string();
                    }
                }
                effects.push(TurnEffect::PlanConfirmed);
                self.invalidate_prefix();
            }

            if result.spawns_sub_agent
                && let Some(prompt) = result.sub_agent_prompt.take() {
                    let session_id = format!("sub_{}", crate::session::paths::chrono_session_id());
                    let fork = result.sub_agent_fork;

                    self.ctx.display.render_sub_agent_status(&session_id, "launched", 0, 0);

                    let sub_idx = processed_results.len();
                    processed_results.push(result);
                    sub_expected += 1;

                    let tx = sub_result_tx.clone();
                    let ctx = self.ctx.clone();
                    let sid = session_id.clone();
                    let permit = sub_semaphore.clone().acquire_owned().await
                        .expect("sub-agent semaphore never closed");
                    std::thread::spawn(move || {
                        let _permit = permit;
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("sub-agent runtime");
                            rt.block_on(async move {
                                match SubAgentExecutor::new(ctx, sid, fork).await {
                                    Ok(executor) => executor.execute(prompt).await,
                                    Err(e) => SubAgentResult {
                                        status: "failed".into(),
                                        thinking: String::new(),
                                        text: format!("Failed to create sub-agent: {e}"),
                                        usage: Default::default(),
                                    },
                                }
                            })
                        }));
                        let sa = match result {
                            Ok(sa) => sa,
                            Err(panic_info) => {
                                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                                    s.to_string()
                                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                                    s.clone()
                                } else {
                                    "sub-agent thread panicked".to_string()
                                };
                                SubAgentResult {
                                    status: "failed".into(),
                                    thinking: String::new(),
                                    text: format!("Sub-agent thread panicked: {msg}"),
                                    usage: Default::default(),
                                }
                            }
                        };
                        let _ = tx.send((sub_idx, session_id, sa));
                    });
                } else {
                    processed_results.push(result);
            }
        }

        let timeout = self.ctx.tool_config.sub_agent_timeout_secs;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout as u64);
        drop(sub_result_tx);
        let mut sub_completed = 0usize;
        while sub_completed < sub_expected {
            if self.ctx.cancel.is_cancelled() {
                self.ctx.display.render_info("Sub-agent collection cancelled.");
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                self.ctx.display.render_error(&format!("Sub-agent batch timed out after {}s.", timeout));
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match tokio::time::timeout(remaining, sub_result_rx.recv()).await {
                Ok(Some((idx, session_id, sa))) => {
                    sub_completed += 1;
                    if let Some(ref mut pr) = processed_results.get_mut(idx) {
                        pr.content = format!(
                            "[sub-agent {}] {} (in={}, out={})\nThinking: {}\nText: {}",
                            session_id, sa.status,
                            sa.usage.total_input_tokens, sa.usage.total_output_tokens,
                            sa.thinking, sa.text
                        );
                        let preview = truncate_str(&sa.thinking, 60);
                        if sa.status != "ok" {
                            self.ctx.display.render_error(
                                &format!("[sub-agent {}] failed: {}", session_id, preview),
                            );
                        }
                        self.ctx.stats.record_sub_agent(
                            sa.usage.agent_request_count,
                            sa.usage.total_input_tokens,
                            sa.usage.total_output_tokens,
                            sa.usage.total_cache_read_tokens,
                            sa.usage.total_cache_creation_tokens,
                        ).await;
                    }
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        for pr in processed_results.iter_mut() {
            if pr.spawns_sub_agent && pr.content.is_empty() {
                pr.content = "Sub-agent did not complete.".into();
            }
        }

        let tool_results: Vec<ToolResult> = processed_results.iter().map(|r| {
            ToolResult {
                tool_use_id: r.tool_use_id.clone(),
                tool_name: r.tool_name.clone(),
                tool_args: r.tool_args.clone(),
                content: r.content.clone(),
                conv_content: r.conv_content.clone(),
            }
        }).collect();

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
            self.ctx.display.render_tool_result(&r.tool_name, &preview);
        }
        Ok(())
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
        let stats = self.ctx.stats.snapshot().await;
        let current_belief = belief.as_ref().map_or(0.0, |bt| bt.belief());
        self.ctx.display.render_title_update(
            crate::config::resolve_model_label(self.llm.model()),
            &crate::ui::StatsSnapshot {
                current_turn_count: stats.current_turn_count,
                agent_request_count: stats.agent_request_count,
                total_input_tokens: stats.total_input_tokens,
                total_output_tokens: stats.total_output_tokens,
                current_context_tokens: stats.current_context_tokens,
                max_context_tokens: self.ctx.config.max_context_tokens as u64,
                total_cache_read_tokens: stats.total_cache_read_tokens,
                total_cache_creation_tokens: stats.total_cache_creation_tokens,
                flash_cost_micros: stats.flash_cost_micros,
                pro_cost_micros: stats.pro_cost_micros,
                belief: current_belief,
            },
        );

        match stop {
            "tool_use" | "tool_calls" => {
                // DecisionEngine 决策是否注入（含内部冷却逻辑）
                if let Some(ref bt) = belief {
                    let b = bt.belief();
                    match self.decision_engine.decide(b, &bt.recent_errors) {
                        crate::agent::decision::Decision::Inject(msg) => {
                            self.ctx.display.render_info(&format!(
                                "Injecting hint (belief {:.2}) into task loop.", b
                            ));
                            self.ctx.store.add_user(&msg).await?;
                        }
                        crate::agent::decision::Decision::Abort => {
                            self.ctx.display.render_error(&format!(
                                "DecisionEngine: aborting (belief {:.2}).", b
                            ));
                            return Ok(Some(TurnDecision::Failed("aborted by DecisionEngine".into())));
                        }
                        _ => {}
                    }
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
                if stop.is_empty() {
                    Ok(Some(TurnDecision::Stop))
                } else {
                    Ok(Some(TurnDecision::Stop))
                }
            }
        }
    }

    /// Execute a full turn: send user input, stream response, execute tools, decide next.
    pub async fn execute(&mut self, user_input: &str, mut belief: Option<&mut crate::agent::belief::BeliefTracker>) -> Result<(TurnDecision, Vec<TurnEffect>)> {
        // New user intent: reset storm breaker window, compact guard, and decision engine
        self.tools.reset_storm();
        self.compacted_this_turn = false;
        self.tool_call_count = 0;
        self.tool_error_count = 0;
        self.signals.clear();
        self.decision_engine.reset();

        self.ctx.store.add_user(user_input).await?;
        self.ctx.stats.record_turn().await;
        self.ctx.log_event(serde_json::json!({"type":"user_input","content":user_input}));

        let mut turn = 0;
        let mut effects = Vec::new();
        let max_turns = self.ctx.max_turns() as usize;

        let (mut system_prompt, mut tools_json) = self.ensure_prefix()?;
        let mut messages = self.ctx.store.lines().await?;

        while turn < max_turns {
            turn += 1;

            // Phase 0: 上下文压缩
            self.try_compact("auto", &mut messages, &mut system_prompt, &mut tools_json).await?;
            if !self.compacted_this_turn {
                let estimated_tokens: usize = messages.iter()
                    .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
                    .sum::<usize>() + system_prompt.len() / 4;
                let max_ctx = self.ctx.config.max_context_tokens;
                if max_ctx > 0 && estimated_tokens > max_ctx * 95 / 100 {
                    self.try_compact("preflight", &mut messages, &mut system_prompt, &mut tools_json).await?;
                }
            }

            // Phase 1: LLM 流式响应
            let StreamOutput { text, thinking, mut calls, stop, usage } =
                self.stream_llm_response(&messages, &system_prompt, &tools_json).await?;

            if self.ctx.cancel.is_cancelled() {
                self.ctx.display.render_stop();
                return Ok((TurnDecision::Interrupted, effects));
            }

            // Phase 1b: 从 thinking/text 回收漏报的工具调用
            calls = self.scavenge_calls(&thinking, &text, calls);

            // Phase 2: 持久化 assistant 消息 + 用量
            self.persist_assistant(&text, &thinking, &calls, &usage).await?;

            // Phase 3: 工具执行
            if !calls.is_empty() {
                self.execute_tools_inner(calls, belief.as_deref_mut(), &mut effects).await?;
            }

            // Phase 4: 决策 — 继续或结束
            if let Some(decision) = self.decide_next(&stop, belief.as_deref_mut()).await? {
                return Ok((decision, effects));
            }
            // tool_use 路径：重新加载 messages 继续循环
            messages = self.ctx.store.lines().await?;
        }

        Ok((TurnDecision::Stop, effects))
    }
}
