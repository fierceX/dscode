use crate::agent::belief::BeliefTracker;
use crate::agent::turn::{TurnDecision, TurnEffect, TurnExecutor};
use crate::context::AgentSharedContext;
use crate::errors;
use crate::session::usage::{UsageRecord, UsageSummary};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// OrchActor is the central orchestrator: receives user inputs,
/// dispatches them to TurnExecutor, and manages the lifecycle.
pub struct OrchActor {
    ctx: Arc<AgentSharedContext>,
    cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
    belief: BeliefTracker,
    forced_model: Option<String>,
}

/// Commands received by the orchestrator.
pub enum OrchCmd {
    UserInput {
        input: String,
        turn_id: crate::runtime::TurnId,
        emitter: std::sync::Arc<crate::runtime::TurnEventEmitter>,
        done: oneshot::Sender<TurnRunResult>,
    },
    SetModel {
        model: String,
        done: oneshot::Sender<anyhow::Result<()>>,
    },
    Compact {
        done: oneshot::Sender<anyhow::Result<crate::runtime::CompactOutcome>>,
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
    pub text: String,
    pub thinking: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            billing_turn_id: String::new(),
            status: TurnStatus::Failed,
            tool_call_count: 0,
            tool_error_count: 0,
            error: Some(error.into()),
            usage_records: Vec::new(),
            usage: UsageSummary::default(),
            text: String::new(),
            thinking: String::new(),
        }
    }

    fn from_decision(decision: &TurnDecision, executor: &TurnExecutor) -> Self {
        let status = match decision {
            TurnDecision::Stop => TurnStatus::Ok,
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
            text: executor.text().to_string(),
            thinking: executor.thinking().to_string(),
        }
    }
}

impl OrchActor {
    pub fn new(ctx: Arc<AgentSharedContext>, cmd_rx: mpsc::UnboundedReceiver<OrchCmd>) -> Self {
        let (window, alpha, beta) = (
            ctx.config.signal.window_size,
            ctx.config.signal.alpha_prior,
            ctx.config.signal.beta_prior,
        );
        Self {
            ctx,
            cmd_rx,
            belief: BeliefTracker::new_with_priors(window, alpha, beta),
            forced_model: None,
        }
    }

    /// Run the orchestrator loop until shutdown.
    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(OrchCmd::UserInput {
                        input,
                        turn_id,
                        emitter,
                        done,
                    }) => {
                        emitter.emit(crate::runtime::AgentEventKind::TurnStarted);
                        self.ctx.log_event(crate::events::EventLog::RuntimeTurnStarted {
                            turn_id: turn_id.to_string(),
                        });
                        let result = self.handle_user_input(input).await;
                        let _ = done.send(result);
                    }
                    Some(OrchCmd::SetModel { model, done }) => {
                        let result = self.handle_model_command(&model).await;
                        let _ = done.send(result);
                    }
                    Some(OrchCmd::Compact { done }) => {
                        self.ctx.display.render_info("Compressing...");
                        let active_model = self.resolve_active();

                        let result = self.ctx.compaction.evaluate_and_compact(
                            "manual",
                            0,
                            crate::llm::client::LlmModelTarget::new(
                                &active_model.actual,
                                active_model.alias.as_deref(),
                            ),
                        ).await;

                        let outcome = match &result {
                            Ok((true, _reason)) => {
                                if let Some(summary) = self.ctx.compaction.read_summary().await {
                                    let trimmed = summary.trim();
                                    if !trimmed.is_empty() {
                                        self.ctx.display.render_text(trimmed);
                                        if !trimmed.ends_with('\n') {
                                            self.ctx.display.render_text("\n");
                                        }
                                    }
                                }
                                self.ctx.log_event(crate::events::EventLog::Compact {
                                    version: None,
                                    trigger: "manual".into(),
                                    result: _reason.clone(),
                                });
                                self.refresh_title().await;
                                Ok(crate::runtime::CompactOutcome::Compacted { reason: _reason.clone() })
                            }
                            Ok((false, reason)) => {
                                self.ctx.display.render_info(&format!("Compact skipped: {reason}"));
                                Ok(crate::runtime::CompactOutcome::Skipped { reason: reason.clone() })
                            }
                            Err(e) => {
                                self.ctx.display.render_error(&format!("Compact failed: {e}"));
                                Err(anyhow::anyhow!("{e:#}"))
                            }
                        };
                        self.ctx.display.render_stop("end_turn");
                        if let Err(error) = self.ctx.flush_event_log().await {
                            self.ctx
                                .display
                                .render_error(&format!("Event log flush failed: {error}"));
                        }
                        let _ = done.send(outcome);
                    }
                    None => break,
                },
                _ = self.ctx.cancel.cancelled() => {
                    self.ctx.display.render_info("Shutting down...");
                    break;
                }
            }
        }

        if let Err(error) = self.ctx.flush_event_log().await {
            self.ctx
                .display
                .render_error(&format!("Event log flush failed: {error}"));
        }
        let _ = self.ctx.stats.flush().await;
        Ok(())
    }

    async fn handle_user_input(&mut self, input: String) -> TurnRunResult {
        let started_at = Instant::now();
        let billing_turn_id = self.ctx.usage.begin_turn();
        // 跨轮重复失败可累积升级，单次偶然失败自然消退。
        self.belief.decay(self.ctx.config.signal.decay_per_input);
        self.refresh_title().await;
        let prepared = self.prepare_turn().await;
        let (model, mut executor) = match prepared {
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
                self.log_turn_final(&result, started_at.elapsed().as_millis() as u64)
                    .await;
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
        self.log_turn_final(&result, started_at.elapsed().as_millis() as u64)
            .await;
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

    async fn prepare_turn(&mut self) -> Result<(String, TurnExecutor)> {
        let resolved = self.resolve_active();

        self.ctx.log_event(crate::events::EventLog::TurnStart {
            model: resolved.actual.clone(),
            model_alias: resolved.alias.clone(),
            belief: self.belief.belief(),
            forced_model: self.forced_model.clone(),
        });

        let executor = TurnExecutor::new(self.ctx.clone(), self.ctx.llm_backend.clone())
            .with_model_target(resolved.actual.clone(), resolved.alias.clone());
        Ok((resolved.actual, executor))
    }

    async fn post_process_turn(
        &mut self,
        decision: TurnDecision,
        effects: Vec<TurnEffect>,
        executor: &TurnExecutor,
        model: &str,
    ) -> TurnRunResult {
        for effect in &effects {
            self.ctx.display.render_info(effect);
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
            TurnDecision::Interrupted => "Interrupted",
            TurnDecision::MaxTurnsExceeded => "MaxTurnsExceeded",
            TurnDecision::Failed(_) => "Failed",
        };
        self.ctx.log_event(crate::events::EventLog::TurnTracking {
            version: None,
            decision: decision_str.into(),
            tool_call_count: executor.tool_call_count(),
            tool_error_count: executor.tool_error_count(),
            belief: self.belief.belief(),
            model: model.into(),
        });
    }

    async fn log_turn_final(&self, result: &TurnRunResult, elapsed_ms: u64) {
        self.ctx.log_event(crate::events::EventLog::TurnFinal {
            billing_turn_id: result.billing_turn_id.clone(),
            status: result.status.as_str().into(),
            tool_call_count: result.tool_call_count,
            tool_error_count: result.tool_error_count,
            elapsed_ms,
            error: result.error.clone(),
            usage: result.usage.clone(),
        });
        if let Err(error) = self.ctx.flush_event_log().await {
            self.ctx
                .display
                .render_error(&format!("Event log flush failed: {error}"));
        }
    }

    async fn handle_turn_error(
        &mut self,
        e: anyhow::Error,
        executor: &TurnExecutor,
        model: &str,
    ) -> TurnRunResult {
        let info = errors::classify_anyhow(&e);
        let error = format!("{e}");
        self.ctx.log_event(crate::events::EventLog::TurnError {
            error: error.clone(),
            category: format!("{:?}", info.category),
            severity: Some(format!("{:?}", info.severity)),
            belief: Some(self.belief.belief()),
            model: Some(model.into()),
            elapsed_ms: None,
            idle_ms: None,
        });
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
            text: executor.text().to_string(),
            thinking: executor.thinking().to_string(),
        }
    }

    async fn refresh_title(&self) {
        let label = self.active_model_label();
        crate::ui::render_title_snapshot(&self.ctx, &label, self.belief.belief()).await;
    }

    async fn handle_model_command(&mut self, model: &str) -> anyhow::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            self.ctx
                .display
                .render_error("Model name must not be empty.");
            anyhow::bail!("Model name must not be empty.");
        }
        // Model switch is gated by the frozen session capability snapshot
        // (v7 §3.3): an Unsupported session accepts any model but stays
        // text-only; an image-capable session requires an exact capability
        // fingerprint match.
        let candidate = crate::capabilities::model_capabilities::SessionModelCapabilities::resolve(
            model,
            &self.ctx.config,
            self.ctx.llm_backend.as_ref(),
        );
        if !self.ctx.model_capabilities.is_compatible_with(&candidate) {
            self.ctx.display.render_error(&format!(
                "Model switch rejected: this session was initialized with image capability {}. The selected model has incompatible capabilities. Start a new session to use that model.",
                self.ctx.model_capabilities.capability_fingerprint
            ));
            anyhow::bail!(
                "model switch rejected: capability fingerprint {} does not match the session's frozen {}",
                candidate.capability_fingerprint,
                self.ctx.model_capabilities.capability_fingerprint
            );
        }
        if model == self.ctx.config.model {
            self.forced_model = None;
        } else {
            self.forced_model = Some(model.to_string());
        }
        let resolved = self.resolve_active();
        self.ctx
            .display
            .render_info(&format!("Switched to {} model.", resolved.label));
        self.refresh_title().await;
        Ok(())
    }

    fn resolve_active(&self) -> crate::config::ResolvedModel {
        let requested = if let Some(forced) = self.forced_model.as_deref() {
            forced
        } else {
            &self.ctx.config.model
        };
        crate::config::model_resolver(&self.ctx.config).resolve(requested)
    }

    fn active_model_label(&self) -> String {
        self.resolve_active().label
    }
}

pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx);
    (actor, cmd_tx)
}
