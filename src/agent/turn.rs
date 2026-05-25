use crate::context::AgentSharedContext;
use crate::llm::client::LlmClient;
use crate::protocol::{Event, ToolCallEvent, UsageEvent};
use crate::session::store::{ToolResult, build_tool_call_summary, first_line};
use crate::tools::runner::ToolRunner;
use crate::util::truncate_str;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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

        while let Some(result) = stream.next().await {
            let evt = result?;

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
                Event::Stop(s) => {
                    self.ctx
                        .log_event(serde_json::json!({"type":"stop","reason":s.reason}));
                    stop = s.reason;
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
                    self.ctx.display.render_retry();
                }
            }
        }

        drop(stream);
        Ok(StreamOutput {
            text,
            thinking,
            calls,
            stop,
            usage,
        })
    }

    /// Phase 1b: 从 thinking/text 中回收漏报的工具调用（scavenge）。
    fn scavenge_calls(
        &self,
        thinking: &str,
        text: &str,
        mut calls: Vec<ToolCallEvent>,
    ) -> Vec<ToolCallEvent> {
        if thinking.is_empty() && text.is_empty() {
            return calls;
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
            self.ctx
                .log_event(serde_json::json!({"type":"scavenge","note":note}));
        }
        calls
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

        let mut prepared_results = Vec::new();
        for mut result in results {
            self.signal_processor
                .process(
                    &mut result,
                    belief.as_deref_mut(),
                    &self.ctx,
                    crate::config::resolve_model_label(self.llm.model()),
                )
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
        let current_belief = belief.as_ref().map_or(0.0, |bt| bt.belief());
        crate::ui::render_title_snapshot(
            &self.ctx,
            crate::config::resolve_model_label(self.llm.model()),
            current_belief,
        )
        .await;

        match stop {
            "tool_use" | "tool_calls" => {
                // DecisionEngine 决策是否注入（含内部冷却逻辑）
                if let Some(ref bt) = belief {
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
                            self.ctx.display.render_info(&format!(
                                "Injecting hint (belief {:.2}){}",
                                b, recent
                            ));
                            self.ctx.store.add_user(&msg).await?;
                        }
                        crate::agent::decision::Decision::Abort => {
                            self.ctx.display.render_error(&format!(
                                "DecisionEngine: aborting (belief {:.2}).",
                                b
                            ));
                            return Ok(Some(TurnDecision::Failed(
                                "aborted by DecisionEngine".into(),
                            )));
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
                Ok(Some(TurnDecision::Stop))
            }
        }
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
                stop,
                usage,
            } = self
                .stream_llm_response(&messages, &system_prompt, &tools_json)
                .await?;

            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx.display.render_stop();
                return Ok((TurnDecision::Interrupted, effects));
            }

            // Phase 1b: 从 thinking/text 回收漏报的工具调用
            calls = self.scavenge_calls(&thinking, &text, calls);

            // 过滤掉被禁用的工具（scavenge 可能回收了已禁用的工具）
            let disable = &self.ctx.config.tool_disable;
            calls.retain(|c| match c.name.as_str() {
                "Bash" => !disable.disable_bash,
                "Python" => !disable.disable_python,
                "WebSearch" | "WebFetch" => !disable.disable_web,
                "SubAgent" => !disable.disable_sub_agent,
                _ => true,
            });

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

        Ok((TurnDecision::Stop, effects))
    }
}
