use crate::agent::belief::BeliefTracker;
use crate::agent::turn::{TurnDecision, TurnEffect, TurnExecutor};
use crate::context::AgentSharedContext;
use crate::errors;
use crate::llm::client::{AsyncLlClient, LlmClient};
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
        done: oneshot::Sender<()>,
    },
    SetModel(String),
    Compact {
        done: oneshot::Sender<()>,
    },
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
                        self.handle_user_input(input).await;
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

    async fn handle_user_input(&mut self, input: String) {
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
                return;
            }
        };

        match executor.execute(&input, Some(&mut self.belief)).await {
            Ok((decision, effects)) => {
                self.post_process_turn(decision, effects, &executor, &model)
                    .await;
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
    ) {
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

        self.log_turn_end(executor, &decision, model);

        if let TurnDecision::Failed(ref msg) = decision
            && msg != "interrupted"
        {
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
            self.ctx
                .display
                .render_error(&format!("Turn execution error: {e}"));
        }
    }

    async fn refresh_title(&self) {
        let model_label = crate::config::resolve_model_label(&self.ctx.config.model);
        crate::ui::render_title_snapshot(&self.ctx, model_label, self.belief.belief()).await;
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
}

pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx);
    (actor, cmd_tx)
}
