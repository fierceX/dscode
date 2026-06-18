use crate::agent::belief::BeliefTracker;
use crate::agent::turn::{TurnDecision, TurnEffect, TurnExecutor};
use crate::context::AgentSharedContext;
use crate::errors;
use crate::llm::client::{AsyncLlClient, LlmClient};
use crate::session::usage::{UsageRecord, UsageSummary};
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// OrchActor is the central orchestrator: receives user inputs,
/// dispatches them to TurnExecutor, and manages the lifecycle.
pub struct OrchActor {
    ctx: Arc<AgentSharedContext>,
    cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
    belief: BeliefTracker,
    forced_model: Option<crate::config::ModelTier>,
    #[cfg(test)]
    llm_override: Option<Arc<dyn LlmClient>>,
}

/// Commands received by the orchestrator.
pub enum OrchCmd {
    UserInput {
        input: String,
        done: oneshot::Sender<TurnRunResult>,
    },
    SetModel(String),
    Compact {
        done: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
pub struct TurnRunResult {
    pub billing_turn_id: String,
    pub status: TurnStatus,
    pub tool_call_count: u32,
    pub tool_error_count: u32,
    pub error: Option<String>,
    pub usage_records: Vec<UsageRecord>,
    pub usage: UsageSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Ok,
    Failed,
    Interrupted,
    MaxTurnsExceeded,
}

impl TurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnStatus::Ok => "ok",
            TurnStatus::Failed => "failed",
            TurnStatus::Interrupted => "interrupted",
            TurnStatus::MaxTurnsExceeded => "max_turns_exceeded",
        }
    }
}

impl TurnRunResult {
    pub fn ok(tool_call_count: u32, tool_error_count: u32) -> Self {
        Self {
            billing_turn_id: String::new(),
            status: TurnStatus::Ok,
            tool_call_count,
            tool_error_count,
            error: None,
            usage_records: Vec::new(),
            usage: UsageSummary::default(),
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            billing_turn_id: String::new(),
            status: TurnStatus::Failed,
            tool_call_count: 0,
            tool_error_count: 0,
            error: Some(error.into()),
            usage_records: Vec::new(),
            usage: UsageSummary::default(),
        }
    }

    fn from_decision(decision: &TurnDecision, executor: &TurnExecutor) -> Self {
        let status = match decision {
            TurnDecision::Stop | TurnDecision::Continue => TurnStatus::Ok,
            TurnDecision::Interrupted => TurnStatus::Interrupted,
            TurnDecision::MaxTurnsExceeded => TurnStatus::MaxTurnsExceeded,
            TurnDecision::Failed(_) => TurnStatus::Failed,
        };
        let error = match decision {
            TurnDecision::Failed(msg) => Some(msg.clone()),
            TurnDecision::Interrupted => Some("interrupted".to_string()),
            TurnDecision::MaxTurnsExceeded => {
                Some("max_turns exhausted before end_turn".to_string())
            }
            _ => None,
        };
        Self {
            billing_turn_id: String::new(),
            status,
            tool_call_count: executor.tool_call_count(),
            tool_error_count: executor.tool_error_count(),
            error,
            usage_records: Vec::new(),
            usage: UsageSummary::default(),
        }
    }
}

impl OrchActor {
    pub fn new(ctx: Arc<AgentSharedContext>, cmd_rx: mpsc::UnboundedReceiver<OrchCmd>) -> Self {
        Self {
            ctx,
            cmd_rx,
            belief: BeliefTracker::new(16),
            forced_model: None,
            #[cfg(test)]
            llm_override: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_llm(
        ctx: Arc<AgentSharedContext>,
        cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
        llm: Arc<dyn LlmClient>,
    ) -> Self {
        Self {
            ctx,
            cmd_rx,
            belief: BeliefTracker::new(16),
            forced_model: None,
            llm_override: Some(llm),
        }
    }

    /// Run the orchestrator loop until shutdown.
    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(OrchCmd::UserInput { input, done }) => {
                        let result = self.handle_user_input(input).await;
                        let _ = done.send(result);
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
                        self.ctx.display.render_stop();
                        let _ = done.send(());
                    }
                    None => break,
                },
                _ = self.ctx.cancel.cancelled() => {
                    self.ctx.display.render_info("Shutting down...");
                    break;
                }
            }
        }

        let _ = self.ctx.stats.flush().await;
        Ok(())
    }

    async fn handle_user_input(&mut self, input: String) -> TurnRunResult {
        let started_at = Instant::now();
        let billing_turn_id = self.ctx.usage.begin_turn();
        self.belief.reset();
        self.ctx.interrupt.store(false, Ordering::SeqCst);
        self.refresh_title().await;
        let prepared = self.prepare_turn().await;
        let (model, _api_url, mut executor) = match prepared {
            Ok(v) => v,
            Err(e) => {
                self.ctx
                    .display
                    .render_error(&format!("Failed to prepare turn: {e}"));
                self.refresh_title().await;
                let result = self.finish_usage(
                    TurnRunResult::failed(format!("failed to prepare turn: {e}")),
                    &billing_turn_id,
                );
                self.log_turn_final(&result, started_at.elapsed().as_millis() as u64);
                return result;
            }
        };

        let result = match executor.execute(&input, Some(&mut self.belief)).await {
            Ok((decision, effects)) => {
                self.post_process_turn(decision, effects, &executor, &model)
                    .await
            }
            Err(e) => self.handle_turn_error(e, &executor, &model).await,
        };

        let result = self.finish_usage(result, &billing_turn_id);
        self.refresh_title().await;
        self.log_turn_final(&result, started_at.elapsed().as_millis() as u64);
        result
    }

    fn finish_usage(&self, mut result: TurnRunResult, billing_turn_id: &str) -> TurnRunResult {
        result.billing_turn_id = billing_turn_id.to_string();
        match self.ctx.usage.records_for(billing_turn_id) {
            Ok(records) => {
                result.usage = UsageSummary::from_records(&records);
                result.usage_records = records;
            }
            Err(error) => {
                self.ctx.display.render_error(&format!(
                    "Failed to read usage records for turn {billing_turn_id}: {error}"
                ));
            }
        }
        self.ctx.usage.end_turn(billing_turn_id);
        result
    }

    async fn prepare_turn(&mut self) -> Result<(String, String, TurnExecutor)> {
        let (model, api_url) = self.resolve_active();

        self.ctx.log_event(serde_json::json!({
            "type": "turn_start",
            "model": &model,
            "belief": self.belief.belief(),
            "forced_model": self.forced_model.map(|t| t.label()),
        }));

        #[cfg(test)]
        let llm: Arc<dyn LlmClient> = if let Some(llm) = &self.llm_override {
            llm.clone()
        } else {
            Arc::new(AsyncLlClient::new(&model, self.ctx.api_key(), &api_url)?)
        };
        #[cfg(not(test))]
        let llm: Arc<dyn LlmClient> =
            Arc::new(AsyncLlClient::new(&model, self.ctx.api_key(), &api_url)?);
        let executor = TurnExecutor::new(self.ctx.clone(), llm);
        Ok((model, api_url, executor))
    }

    async fn post_process_turn(
        &mut self,
        decision: TurnDecision,
        effects: Vec<TurnEffect>,
        executor: &TurnExecutor,
        model: &str,
    ) -> TurnRunResult {
        for effect in &effects {
            match effect {
                TurnEffect::PlanCleared => {
                    self.ctx.display.render_info("Plan cleared.");
                }
                TurnEffect::PlanConfirmed => {
                    self.ctx.display.render_info("Plan confirmed.");
                }
            }
        }

        self.log_turn_tracking(executor, &decision, model);

        if let TurnDecision::Failed(ref msg) = decision
            && msg != "interrupted"
        {
            self.ctx.display.render_error(msg);
        }
        if decision == TurnDecision::MaxTurnsExceeded {
            self.ctx
                .display
                .render_error("max_turns exhausted before end_turn");
        }
        TurnRunResult::from_decision(&decision, executor)
    }

    fn log_turn_tracking(&self, executor: &TurnExecutor, decision: &TurnDecision, model: &str) {
        let decision_str = match decision {
            TurnDecision::Stop => "Stop",
            TurnDecision::Continue => "Continue",
            TurnDecision::Interrupted => "Interrupted",
            TurnDecision::MaxTurnsExceeded => "MaxTurnsExceeded",
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

    fn log_turn_final(&self, result: &TurnRunResult, elapsed_ms: u64) {
        self.ctx.log_event(serde_json::json!({
            "type": "turn_final",
            "billing_turn_id": result.billing_turn_id,
            "status": result.status.as_str(),
            "tool_call_count": result.tool_call_count,
            "tool_error_count": result.tool_error_count,
            "elapsed_ms": elapsed_ms,
            "error": result.error.clone(),
            "usage": result.usage,
        }));
    }

    async fn handle_turn_error(
        &mut self,
        e: anyhow::Error,
        executor: &TurnExecutor,
        model: &str,
    ) -> TurnRunResult {
        let info = errors::classify_anyhow(&e);
        let error = format!("{e}");
        self.ctx.log_event(serde_json::json!({
            "type": "turn_error",
            "error": error,
            "category": format!("{:?}", info.category),
            "severity": format!("{:?}", info.severity),
            "belief": self.belief.belief(),
            "model": model,
        }));
        if info.severity == errors::ErrorSeverity::Fatal {
            self.ctx.display.render_error(&format!("Fatal error: {e}"));
        } else {
            self.ctx
                .display
                .render_error(&format!("Turn execution error: {e}"));
        }
        TurnRunResult {
            billing_turn_id: String::new(),
            status: TurnStatus::Failed,
            tool_call_count: executor.tool_call_count(),
            tool_error_count: executor.tool_error_count(),
            error: Some(error),
            usage_records: Vec::new(),
            usage: UsageSummary::default(),
        }
    }

    async fn refresh_title(&self) {
        crate::ui::render_title_snapshot(
            &self.ctx,
            self.active_model_label(),
            self.belief.belief(),
        )
        .await;
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
                self.ctx
                    .display
                    .render_info(&format!("Switched to {} model.", t.label()));
                self.refresh_title().await;
            }
            Err(_) => {
                self.ctx
                    .display
                    .render_error(&format!("Unknown model tier: {model}. Use /flash or /pro"));
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

    fn active_model_label(&self) -> &'static str {
        if let Some(forced) = self.forced_model {
            forced.label()
        } else {
            crate::config::resolve_model_label(&self.ctx.config.model)
        }
    }
}

pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx);
    (actor, cmd_tx)
}
