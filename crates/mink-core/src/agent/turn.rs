use crate::agent::text::truncate_str;
use crate::context::AgentSharedContext;
use crate::llm::client::{LlmBackend, LlmModelTarget};
use crate::protocol::{Event, ToolCallEvent, UsageEvent};
use crate::session::store::{build_tool_call_summary, first_line};
use crate::sse::toolcall::build_tool_call_event;
use crate::tools::runner::ToolRunner;
use crate::ui::ToolResultDisplay;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod recovery;
mod stream;
mod tools;

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
    llm_backend: Arc<dyn LlmBackend>,
    model_name: String,
    model_alias: Option<String>,
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
    /// 本输入内 Warning 级响应的累计次数；连续两次触发策略重启。
    warning_count: usize,
    /// 本输入内已尝试的策略重启次数。
    replan_attempts: usize,
    /// 当前用户输入原文，供恢复任务报告引用。
    current_user_input: String,
    /// 恢复子代理配置副本，包含当前活动模型。
    sub_agent_config: crate::config::ResolvedConfig,
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
    Stop,
    Interrupted,
    MaxTurnsExceeded,
    Failed(String),
}

impl TurnExecutor {
    pub fn new(ctx: Arc<AgentSharedContext>, llm_backend: Arc<dyn LlmBackend>) -> Self {
        let tools = Arc::new(ToolRunner::new(Arc::new(
            crate::context::ToolContext::from(ctx.as_ref()),
        )));
        let prefix = crate::agent::prefix::PrefixManager::new(ctx.clone());
        let mut sub_agent_config = ctx.config.clone();
        let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
        if let Some(alias) = resolved.alias.as_deref() {
            sub_agent_config
                .model_aliases
                .insert(alias.to_string(), resolved.actual.clone());
            sub_agent_config.model = alias.to_string();
        } else {
            sub_agent_config.model = resolved.actual.clone();
        }
        Self {
            ctx: ctx.clone(),
            llm_backend,
            model_name: resolved.actual,
            model_alias: resolved.alias,
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

    pub(crate) fn with_model_target(
        mut self,
        model_name: impl Into<String>,
        model_alias: Option<String>,
    ) -> Self {
        self.model_name = model_name.into();
        self.model_alias = model_alias;
        if let Some(alias) = self.model_alias.as_deref() {
            self.sub_agent_config
                .model_aliases
                .insert(alias.to_string(), self.model_name.clone());
            self.sub_agent_config.model = alias.to_string();
        } else {
            self.sub_agent_config.model = self.model_name.clone();
        }
        self.sub_agents = crate::agent::sub_coordinator::SubAgentCoordinator::new(
            self.ctx.clone(),
            self.sub_agent_config.clone(),
        );
        self
    }

    fn model_label(&self) -> &str {
        self.model_alias.as_deref().unwrap_or(&self.model_name)
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
    #[cfg(test)]
    pub fn collected_signals(&self) -> &[crate::guard::collector::Signal] {
        self.signal_processor.collected_signals()
    }

    pub fn text(&self) -> &str {
        &self.final_text
    }

    pub fn thinking(&self) -> &str {
        &self.final_thinking
    }
}

impl TurnExecutor {
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
                self.ctx.display.render_stop("interrupted");
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
) -> crate::tools::runner::ToolExecution {
    crate::tools::runner::ToolExecution {
        tool_use_id: call.id,
        tool_name: call.name,
        tool_args: call.fields,
        content,
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_description: None,
        sub_agent_fork: false,
        exit_code: None,
        status: crate::tools::metadata::ToolStatus::Blocked(
            crate::tools::metadata::ToolBlocker::RecoveryGuard,
        ),
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
#[path = "turn_tests.rs"]
mod tests;
