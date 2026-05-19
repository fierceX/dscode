use crate::agent::sub_pool::{SubAgentPool, SubAgentReport};
use crate::agent::turn::{TurnExecutor, TurnDecision, TurnEffect};
use crate::agent::belief::BeliefTracker;
use crate::context::AgentSharedContext;
use crate::llm::client::{AsyncLlClient, LlmClient};
use crate::errors;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// OrchActor is the central orchestrator: receives user inputs and sub-agent results,
/// dispatches them to TurnExecutor, and manages the lifecycle.
pub struct OrchActor {
    ctx: Arc<AgentSharedContext>,
    cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
    sub_pool: Arc<SubAgentPool>,
    belief: BeliefTracker,
    forced_model: Option<crate::config::ModelTier>,
    /// 等待本轮子代理完成的 receiver 列表
    pending_sub_agents: Vec<tokio::sync::oneshot::Receiver<crate::agent::sub_pool::SubAgentReport>>,
}

/// Commands received by the orchestrator.
pub enum OrchCmd {
    UserInput { input: String, done: oneshot::Sender<()> },
    SetModel(String),
    Compact { done: oneshot::Sender<()> },
    SubAgentResult(SubAgentReport),
}

impl OrchActor {
    pub fn new(
        ctx: Arc<AgentSharedContext>,
        cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
        sub_pool: Arc<SubAgentPool>,
    ) -> Self {
        Self {
            ctx, cmd_rx, sub_pool,
            belief: BeliefTracker::new(16),
            forced_model: None,
            pending_sub_agents: Vec::new(),
        }
    }

    /// Run the orchestrator loop until shutdown.
    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(OrchCmd::UserInput { input, done }) => {
                        self.handle_user_input(input).await;

                        // 等待本轮所有子代理完成（同步阻塞，取最长时间）
                        self.wait_pending_sub_agents().await;

                        let _ = done.send(());
                    }
                    Some(OrchCmd::SetModel(model)) => {
                        self.handle_model_command(&model).await;
                    }
                    Some(OrchCmd::Compact { done }) => {
                        self.ctx.display.render_info("Compressing...");

                        let stats = self.ctx.stats.snapshot().await;
                        let result = self.ctx.compaction.evaluate_and_compact(
                            "manual",
                            stats.current_context_tokens as usize,
                        ).await;

                        match &result {
                            Ok((true, _reason)) => {
                                *self.ctx.immutable_prefix.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                if let Some(summary) = self.ctx.compaction.read_summary().await {
                                    let clean = crate::session::compaction::strip_tool_labels(&summary);
                                    let trimmed = clean.trim();
                                    if !trimmed.is_empty() {
                                        self.ctx.display.render_text(trimmed);
                                        if !trimmed.ends_with('\n') {
                                            self.ctx.display.render_text("\n");
                                        }
                                    }
                                }
                                self.ctx.log_event(serde_json::json!({"type":"compact","trigger":"manual","result":_reason}));
                                self.refresh_title().await;
                            }
                            Ok((false, reason)) => {
                                self.ctx.display.render_info(&format!("Compact skipped: {reason}"));
                            }
                            Err(e) => {
                                self.ctx.display.render_error(&format!("Compact failed: {e}"));
                            }
                        }
                        let _ = done.send(());
                    }
                    Some(OrchCmd::SubAgentResult(report)) => {
                        // 已通过 wait_pending_sub_agents 同步等待，这里只记录日志
                        self.ctx.log_event(serde_json::json!({
                            "type":"sub_agent_result_ignored",
                            "session_id": &report.session_id,
                            "status": &report.status,
                        }));
                    }
                    None => break,
                },
                _ = self.ctx.cancel.cancelled() => {
                    self.ctx.display.render_info("Shutting down...");
                    break;
                }
            }

            if !self.ctx.interactive() && self.sub_pool.active_count() == 0 {
                break;
            }
        }

        self.sub_pool.drain().await;
        let _ = self.ctx.stats.flush().await;
        Ok(())
    }

    /// 同步等待本轮所有子代理完成，批量注入结果
    async fn wait_pending_sub_agents(&mut self) {
        if self.pending_sub_agents.is_empty() {
            return;
        }

        let mut reports = Vec::new();
        let max_wait = std::time::Duration::from_secs(120);
        let deadline = tokio::time::Instant::now() + max_wait;

        for rx in self.pending_sub_agents.drain(..) {
            let timeout = deadline - tokio::time::Instant::now();
            if timeout.is_zero() {
                self.ctx.display.render_error("Sub-agent wait timeout, discarding remaining.");
                break;
            }
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(report)) => {
                    reports.push(report);
                }
                _ => {
                    self.ctx.display.render_error("Sub-agent wait timeout.");
                }
            }
        }

        if reports.is_empty() {
            return;
        }

        // 批量注入: 把所有子代理结果合并成一条用户消息
        let mut context = String::new();
        for (i, report) in reports.iter().enumerate() {
            if i > 0 { context.push('\n'); }
            context.push_str(&format!(
                "[sub-agent {}] {} (in={}, out={})\nThinking: {}\nText: {}",
                report.session_id, report.status,
                report.usage.total_input_tokens, report.usage.total_output_tokens,
                report.thinking, report.text
            ));
            // 显示状态
            if report.status == "ok" {
                self.ctx.display.render_sub_agent_status(
                    &report.session_id, "ok",
                    report.usage.total_input_tokens, report.usage.total_output_tokens,
                );
            } else {
                self.ctx.display.render_error(&format!("[sub-agent {}] failed", report.session_id));
            }
            // 显示子代理的 thinking 和 text 摘要
            if !report.thinking.is_empty() {
                let trimmed: String = report.thinking.chars().take(120).collect();
                self.ctx.display.render_info(&trimmed);
            }
            if !report.text.is_empty() {
                let trimmed: String = report.text.chars().take(120).collect();
                self.ctx.display.render_info(&trimmed);
            }
            if !report.thinking.is_empty() || !report.text.is_empty() {
                self.ctx.stats.record_sub_agent(
                    report.usage.agent_request_count,
                    report.usage.total_input_tokens,
                    report.usage.total_output_tokens,
                    report.usage.total_cache_read_tokens,
                    report.usage.total_cache_creation_tokens,
                ).await;
            }
        }

        // 一次性喂入 LLM（belief 保持上一轮的，不清空）
        self.ctx.log_event(serde_json::json!({"type":"sub_agent_batch","count":reports.len()}));
        self.handle_user_input(context).await;
    }

    async fn handle_user_input(&mut self, input: String) {
        self.belief.reset();
        self.refresh_title().await;
        let prepared = self.prepare_turn().await;
        let (model, _api_url, mut executor) = match prepared {
            Ok(v) => v,
            Err(e) => {
                self.ctx.display.render_error(&format!("Failed to prepare turn: {e}"));
                return;
            }
        };

        match executor.execute(&input, Some(&mut self.belief)).await {
            Ok((decision, effects)) => {
                self.post_process_turn(decision, effects, &executor, &model).await;
            }
            Err(e) => {
                self.handle_turn_error(e, &model).await;
            }
        }

        self.refresh_title().await;
    }

    async fn prepare_turn(&mut self) -> Result<(String, String, TurnExecutor)> {
        let (model, api_url) = self.resolve_active();

        self.ctx.log_event(serde_json::json!({
            "type": "turn_start",
            "model": &model,
            "belief": self.belief.belief(),
            "forced_model": self.forced_model.map(|t| t.label()),
        }));

        let llm: Arc<dyn LlmClient> = Arc::new(AsyncLlClient::new(&model, self.ctx.api_key(), &api_url)?);
        let executor = TurnExecutor::new(self.ctx.clone(), llm);
        Ok((model, api_url, executor))
    }

    async fn post_process_turn(
        &mut self,
        decision: TurnDecision,
        effects: Vec<TurnEffect>,
        executor: &TurnExecutor,
        model: &str,
    ) {
        for effect in effects {
            match effect {
                TurnEffect::SubAgentLaunched { session_id, prompt, description, fork } => {
                    self.ctx.log_event(serde_json::json!({
                        "type":"sub_agent_start",
                        "session_id": &session_id,
                        "timestamp": chrono_now(),
                        "prompt": &prompt,
                        "description": &description,
                        "fork": fork,
                    }));
                    match self.sub_pool.launch(
                        self.ctx.clone(), prompt.clone(), description.clone(), fork, session_id.clone(),
                    ).await {
                        Ok((_, rx)) => {
                            self.ctx.display.render_sub_agent_status(&session_id, "launched", 0, 0);
                            self.pending_sub_agents.push(rx);
                        }
                        Err(e) => {
                            self.ctx.display.render_error(&format!("SubAgent launch failed: {e}"));
                        }
                    }
                }
                TurnEffect::PlanCleared => {
                    self.ctx.display.render_info("Plan cleared.");
                }
                TurnEffect::PlanConfirmed => {
                    self.ctx.display.render_info("Plan confirmed.");
                }
            }
        }

        self.log_turn_end(executor, &decision, model);

        if let TurnDecision::Failed(ref msg) = decision
            && msg != "interrupted" {
                self.ctx.display.render_error(msg);
            }
    }

    fn log_turn_end(&self, executor: &TurnExecutor, decision: &TurnDecision, model: &str) {
        let decision_str = match decision {
            TurnDecision::Stop => "Stop",
            TurnDecision::Continue => "Continue",
            TurnDecision::Interrupted => "Interrupted",
            TurnDecision::Failed(_) => "Failed",
        };
        self.ctx.log_event(serde_json::json!({
            "type": "turn_tracking",
            "decision": decision_str,
            "tool_call_count": executor.tool_call_count(),
            "tool_error_count": executor.tool_error_count(),
            "belief": self.belief.belief(),
            "model": model,
        }));
    }

    async fn handle_turn_error(&mut self, e: anyhow::Error, model: &str) {
        let info = errors::classify_anyhow(&e);
        self.ctx.log_event(serde_json::json!({
            "type": "turn_error",
            "error": format!("{e}"),
            "category": format!("{:?}", info.category),
            "severity": format!("{:?}", info.severity),
            "belief": self.belief.belief(),
            "model": model,
        }));
        if info.severity == errors::ErrorSeverity::Fatal {
            self.ctx.display.render_error(&format!("Fatal error: {e}"));
        } else {
            self.ctx.display.render_error(&format!("Turn execution error: {e}"));
        }
    }

    async fn refresh_title(&self) {
        let new_stats = self.ctx.stats.snapshot().await;
        let snapshot = crate::ui::StatsSnapshot {
            current_turn_count: new_stats.current_turn_count,
            agent_request_count: new_stats.agent_request_count,
            total_input_tokens: new_stats.total_input_tokens,
            total_output_tokens: new_stats.total_output_tokens,
            current_context_tokens: new_stats.current_context_tokens,
            max_context_tokens: self.ctx.config.max_context_tokens as u64,
            total_cache_read_tokens: new_stats.total_cache_read_tokens,
            total_cache_creation_tokens: new_stats.total_cache_creation_tokens,
            flash_cost_micros: new_stats.flash_cost_micros,
            pro_cost_micros: new_stats.pro_cost_micros,
            belief: self.belief.belief(),
        };
        let model_label = crate::config::resolve_model_label(&self.ctx.config.model);
        self.ctx.display.render_title_update(model_label, &snapshot);
    }

    async fn handle_model_command(&mut self, model: &str) {
        match crate::config::ModelTier::parse(model) {
            Ok(t) => {
                if t == crate::config::ModelTier::Flash {
                    self.forced_model = None;
                    self.belief.reset();
                    self.ctx.display.render_info("切回 flash。");
                } else {
                    self.forced_model = Some(t);
                }
                self.ctx.display.render_info(&format!("Switched to {} model.", t.label()));
            }
            Err(_) => {
                self.ctx.display.render_error(&format!("Unknown model tier: {model}. Use /flash or /pro"));
            }
        }
    }

    fn resolve_active(&self) -> (String, String) {
        let tier = if let Some(forced) = self.forced_model {
            forced
        } else {
            crate::config::ModelTier::parse(&self.ctx.config.model)
                .unwrap_or(crate::config::ModelTier::Flash)
        };
        (tier.model_name().to_string(), self.ctx.api_url.clone())
    }
}

fn chrono_now() -> String {
    use time::format_description::FormatItem;
    use time::macros::format_description;
    static FMT: &[FormatItem<'_>] = format_description!("[year][month][day]-[hour][minute][second]");
    let base = time::OffsetDateTime::now_utc().format(FMT).unwrap_or_else(|_| String::new());
    let rand_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u16)
        .unwrap_or(0);
    format!("{}-{:04x}", base, rand_suffix)
}

pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
    sub_pool: Arc<SubAgentPool>,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx, sub_pool);
    (actor, cmd_tx)
}
