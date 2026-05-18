use crate::agent::sub_pool::{SubAgentPool, SubAgentReport};
use crate::agent::turn::{TurnExecutor, TurnDecision, TurnEffect};
use crate::agent::failure_tracker::{TurnFailureTracker, category_to_signal_kind};
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
    auto_upgrade_score: u32,
    model_locked: bool,
    forced_model: Option<crate::config::ModelTier>,
    upgrade_threshold: u32,
    failure_tracker: TurnFailureTracker,
    auto_model_enabled: bool,
    self_report_enabled: bool,
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
        let auto_model = std::env::var("AUTO_MODEL").map(|v| v == "1" || v == "true").unwrap_or(false);
        let threshold = std::env::var("AUTO_UPGRADE_THRESHOLD")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(4);
        let self_report_enabled = std::env::var("AUTO_SELF_REPORT")
            .map(|v| v == "1" || v == "true").unwrap_or(false);
        Self {
            ctx, cmd_rx, sub_pool,
            auto_upgrade_score: 0,
            model_locked: false,
            forced_model: None,
            upgrade_threshold: threshold,
            failure_tracker: TurnFailureTracker::new(threshold),
            auto_model_enabled: auto_model,
            self_report_enabled,
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
                                *self.ctx.immutable_prefix.lock().unwrap() = None;

                                // Show the compacted summary once, as normal content (no prefix, no gray)
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
                                // Refresh title bar
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
                                };
                                let model_label = crate::config::resolve_model_label(&self.ctx.config.model);
                                self.ctx.display.render_title_update(model_label, &snapshot);
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
                        self.handle_sub_agent_result(report).await;
                    }
                    None => break,
                },
                _ = self.ctx.cancel.cancelled() => {
                    self.ctx.display.render_info("Shutting down...");
                    break;
                }
            }

            // Exit if non-interactive and no active sub-agents
            if !self.ctx.interactive() && self.sub_pool.active_count() == 0 {
                break;
            }
        }

        // Graceful shutdown
        self.sub_pool.drain().await;
        let _ = self.ctx.stats.flush().await;
        Ok(())
    }

    async fn handle_user_input(&mut self, input: String) {
        // Fresh user turn: reset failure tracker (new intent)
        self.failure_tracker.reset();

        let (model, api_url) = self.resolve_active();
        let llm: Arc<dyn LlmClient> = match AsyncLlClient::new(
            &model, self.ctx.api_key(), &api_url,
        ) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                self.ctx.display.render_error(&format!("Failed to create LLM client: {e}"));
                return;
            }
        };

        let mut executor = TurnExecutor::new(self.ctx.clone(), llm);

        match executor.execute(&input).await {
            Ok((decision, effects)) => {
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
                                self.ctx.clone(), prompt.clone(), description.clone(), fork,
                            ).await {
                                Ok(_) => {
                                    self.ctx.display.render_sub_agent_status(&session_id, "launched", 0, 0);
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
                        TurnEffect::NeedsPro => {
                            if self.self_report_enabled && self.auto_model_enabled && !self.model_locked {
                                self.auto_upgrade_score += 3;
                                self.ctx.display.render_info("Model requested upgrade (NEEDS_PRO).");
                                if self.auto_upgrade_score >= self.upgrade_threshold {
                                    self.model_locked = true;
                                }
                            }
                        }
                    }
                }

                self.update_after_turn(&decision);

                if let TurnDecision::Failed(ref msg) = decision
                    && msg != "interrupted" {
                        self.ctx.display.render_error(msg);
                    }
            }
            Err(e) => {
                let info = errors::classify_anyhow(&e);
                self.record_error_signal(info.category);
                if info.severity == errors::ErrorSeverity::Fatal {
                    self.ctx.display.render_error(&format!("Fatal error: {e}"));
                } else {
                    self.ctx.display.render_error(&format!("Turn execution error: {e}"));
                }
            }
        }
    }

    async fn handle_sub_agent_result(&mut self, report: SubAgentReport) {
        self.ctx.stats.record_sub_agent(
            report.usage.agent_request_count,
            report.usage.total_input_tokens,
            report.usage.total_output_tokens,
            report.usage.total_cache_read_tokens,
            report.usage.total_cache_creation_tokens,
        ).await;

        self.ctx.log_event(serde_json::json!({
            "type":"sub_agent_end",
            "session_id": &report.session_id,
            "timestamp": chrono_now(),
            "status": &report.status,
        }));
        self.ctx.log_event(serde_json::json!({
            "type":"usage",
            "input_tokens": report.usage.total_input_tokens,
            "output_tokens": report.usage.total_output_tokens,
            "cache_read_input_tokens": report.usage.total_cache_read_tokens,
            "cache_creation_input_tokens": report.usage.total_cache_creation_tokens,
            "kind":"sub_agent",
            "sub_session_id": &report.session_id,
        }));

        if report.status == "ok" {
            self.ctx.display.render_sub_agent_status(
                &report.session_id, "ok",
                report.usage.total_input_tokens,
                report.usage.total_output_tokens,
            );
        } else {
            self.ctx.display.render_error(&format!("[sub-agent {}] failed", report.session_id));
        }

        if !report.thinking.is_empty() {
            self.ctx.display.render_info(&truncate_str(&report.thinking, 120));
        }
        if !report.text.is_empty() {
            self.ctx.display.render_info(&truncate_str(&report.text, 120));
        }

        // Inject sub-agent result into conversation and re-run agent loop
        let context = format!(
            "[sub-agent {}] {} (in={}, out={})\nThinking: {}\nText: {}",
            report.session_id, report.status,
            report.usage.total_input_tokens, report.usage.total_output_tokens,
            report.thinking, report.text
        );

        self.handle_user_input(context).await;
    }

    async fn handle_model_command(&mut self, model: &str) {
        match crate::config::ModelTier::parse(model) {
            Ok(t) => {
                let label = t.label();
                if t == crate::config::ModelTier::Flash {
                    self.forced_model = None;
                } else {
                    self.forced_model = Some(t);
                }
                self.model_locked = false;
                self.auto_upgrade_score = 0;
                self.ctx.display.render_info(&format!("Switched to {label} model."));
            }
            Err(_) => {
                self.ctx.display.render_error(&format!("Unknown model tier: {model}. Use /flash or /pro"));
            }
        }
    }

    fn resolve_active(&self) -> (String, String) {
        let tier = if let Some(forced) = self.forced_model {
            forced
        } else if !self.auto_model_enabled {
            crate::config::ModelTier::parse(&self.ctx.config.model).unwrap_or(crate::config::ModelTier::Flash)
        } else if self.model_locked || self.auto_upgrade_score >= self.upgrade_threshold {
            crate::config::ModelTier::Pro
        } else {
            crate::config::ModelTier::parse(&self.ctx.config.model).unwrap_or(crate::config::ModelTier::Flash)
        };
        (tier.model_name().to_string(), self.ctx.api_url.clone())
    }

    fn update_after_turn(&mut self, decision: &TurnDecision) {
        if !self.auto_model_enabled {
            return;
        }

        match decision {
            TurnDecision::Failed(msg) if msg != "interrupted" => {
                let category = classify_failure_message(msg);
                let kind = category_to_signal_kind(category);
                let weight = errors::upgrade_weight(category);

                if self.failure_tracker.note_and_crossed_threshold(kind) {
                    self.auto_upgrade_score += weight;
                }
                self.apply_supervisory_degradation();

                if self.auto_upgrade_score >= self.upgrade_threshold && !self.model_locked {
                    self.model_locked = true;
                    self.ctx.log_event(serde_json::json!({
                        "type": "model_upgrade",
                        "reason": format!("score={}, signals=[{}]", self.auto_upgrade_score, self.failure_tracker.format_breakdown()),
                        "new_model": self.resolve_active().0,
                    }));
                    self.ctx.display.render_info(&format!(
                        "Auto-upgrade: switching to {} (score={})",
                        self.resolve_active().0, self.auto_upgrade_score
                    ));
                }
            }
            TurnDecision::Stop => {
                if !self.model_locked {
                    self.auto_upgrade_score = 0;
                    self.failure_tracker.reset();
                }
            }
            _ => {}
        }
    }

    fn record_error_signal(&mut self, category: errors::ErrorCategory) {
        if self.auto_model_enabled && errors::is_upgrade_signal(category) {
            let kind = category_to_signal_kind(category);
            let weight = errors::upgrade_weight(category);
            self.failure_tracker.note_and_crossed_threshold(kind);
            self.auto_upgrade_score += weight;
            if self.auto_upgrade_score >= self.upgrade_threshold && !self.model_locked {
                self.model_locked = true;
                self.ctx.display.render_info(&format!(
                    "Auto-upgrade: switching to {} (signal={:?})",
                    self.resolve_active().0, category
                ));
            }
        }
    }

    fn apply_supervisory_degradation(&mut self) {
        // Check accumulated signals — when the tracker crosses at
        // threshold (signals on the verge of escalation) show a user
        // advisory so they know something is trending wrong.
        if self.failure_tracker.format_breakdown().contains("×") {
            // Show breakdown on the 2nd accumulated signal (visible before escalation at threshold)
        }
        if self.auto_upgrade_score >= self.upgrade_threshold.saturating_sub(1) && self.auto_upgrade_score > 0 {
            self.ctx.display.render_info(&format!(
                "Repeated failures: {}. Consider /pro or Ctrl-C.",
                self.failure_tracker.format_breakdown()
            ));
        }
    }
}

fn truncate_str(s: &str, n: usize) -> String {
    if s.len() <= n { return s.to_string(); }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    format!("{}...", &s[..end])
}

fn classify_failure_message(msg: &str) -> errors::ErrorCategory {
    errors::classify_error_from_message(msg).category
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

/// Creates the OrchActor + command sender pair.
pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
    sub_pool: Arc<SubAgentPool>,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx, sub_pool);
    (actor, cmd_tx)
}
