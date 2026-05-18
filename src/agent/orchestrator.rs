use crate::agent::sub_pool::{SubAgentPool, SubAgentReport};
use crate::agent::turn::{TurnExecutor, TurnDecision, TurnEffect};
use crate::agent::controller::{Controller, ControlAction};
use crate::agent::model_selector::ModelSelector;
use crate::context::AgentSharedContext;
use crate::llm::client::{AsyncLlClient, LlmClient};
use crate::errors;
use crate::util::truncate_str;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// OrchActor is the central orchestrator: receives user inputs and sub-agent results,
/// dispatches them to TurnExecutor, and manages the lifecycle.
pub struct OrchActor {
    ctx: Arc<AgentSharedContext>,
    cmd_rx: mpsc::UnboundedReceiver<OrchCmd>,
    sub_pool: Arc<SubAgentPool>,
    controller: Controller,
    model_selector: ModelSelector,
    model_beliefs_path: PathBuf,
    forced_model: Option<crate::config::ModelTier>,
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
        model_beliefs_path: PathBuf,
    ) -> Self {
        let auto_model = std::env::var("AUTO_MODEL").map(|v| v == "1" || v == "true").unwrap_or(false);
        let self_report_enabled = std::env::var("AUTO_SELF_REPORT")
            .map(|v| v == "1" || v == "true").unwrap_or(false);
        let mut model_selector = ModelSelector::new();
        // Load historical beliefs if continuing a session
        let _ = model_selector.load_from_path(&model_beliefs_path);
        Self {
            ctx, cmd_rx, sub_pool,
            controller: Controller::new(),
            model_selector,
            model_beliefs_path,
            forced_model: None,
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
                                *self.ctx.immutable_prefix.lock().unwrap_or_else(|e| e.into_inner()) = None;

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
        // Fresh user turn: reset controller per-turn counters
        self.controller.reset_per_turn();

        // Register models for selector if auto-model is enabled
        if self.auto_model_enabled {
            self.model_selector.ensure("flash");
            self.model_selector.ensure("pro");
        }

        let (model, api_url) = self.resolve_active();

        // [LOG] Turn start: model selection + system state
        self.ctx.log_event(serde_json::json!({
            "type": "turn_start",
            "model": &model,
            "auto_model_enabled": self.auto_model_enabled,
            "controller": self.controller.snapshot(),
            "model_selector": self.model_selector.snapshot_beliefs(),
            "forced_model": self.forced_model.map(|t| t.label()),
        }));

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
                // Track tool call count from executor for fix loop detection
                let tool_call_count = executor.tool_call_count();
                for _ in 0..tool_call_count {
                    self.controller.note_tool_call();
                }

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
                            if self.self_report_enabled && self.auto_model_enabled && !self.controller.is_locked() {
                                self.controller.note_error(false);
                                self.controller.note_error(false);
                                self.controller.note_error(false);
                                self.ctx.display.render_info("Model requested upgrade (NEEDS_PRO).");
                            }
                        }
                    }
                }

                self.update_after_turn(&decision);

                // 将传感器信号（聚合后）喂入 Controller
                // 一轮中无论多少个错误信号，只计 1 次 note_error(false)
                // 仅在轮次失败时补充，成功轮次（Stop）已由 update_after_turn 重置
                if matches!(decision, TurnDecision::Failed{..}) {
                    let signals = executor.accumulated_signals();
                    if signals.iter().any(|s| s.kind == "tool_error") {
                        self.controller.note_error(false);
                        let signal_kinds: Vec<&str> = signals.iter().map(|s| s.kind.as_str()).collect();
                        let signal_details: Vec<&str> = signals.iter().map(|s| s.detail.as_str()).collect();
                        self.ctx.log_event(serde_json::json!({
                            "type": "sensor_signal_aggregated",
                            "signal_count": signals.len(),
                            "signal_kinds": signal_kinds,
                            "signal_details": signal_details,
                            "controller_k": self.controller.no_progress_count(),
                            "controller_p": self.controller.stall_probability(),
                        }));
                    }
                }

                // Check control actions after each turn
                self.handle_control_actions().await;

                // Update model selector based on outcome
                if self.auto_model_enabled {
                    let success = matches!(decision, TurnDecision::Stop);
                    // Use tier label ("pro"/"flash") not API model name, to avoid phantom entries
                    let tier_label = crate::config::ModelTier::parse(&model)
                        .map(|t| t.label().to_string())
                        .unwrap_or_else(|_| model.clone());
                    self.model_selector.update(&tier_label, success);
                    // Persist beliefs to disk for session resume
                    let _ = self.model_selector.save_to_path(&self.model_beliefs_path);
                    self.ctx.log_event(serde_json::json!({
                        "type": "model_selector_update",
                        "model": &tier_label,
                        "success": success,
                        "beliefs": self.model_selector.format_beliefs(),
                    }));
                }

                // [LOG] Turn end: comprehensive tracking snapshot
                {
                    let signals = executor.accumulated_signals();
                    let decision_str = match &decision {
                        TurnDecision::Stop => "Stop",
                        TurnDecision::Continue => "Continue",
                        TurnDecision::Interrupted => "Interrupted",
                        TurnDecision::Failed(_) => "Failed",
                    };
                    let signal_kinds: Vec<&str> = signals.iter().map(|s| s.kind.as_str()).collect();
                    let signal_details: Vec<&str> = signals.iter().map(|s| s.detail.as_str()).collect();
                    self.ctx.log_event(serde_json::json!({
                        "type": "turn_tracking",
                        "decision": decision_str,
                        "tool_call_count": executor.tool_call_count(),
                        "signal_count": signals.len(),
                        "signal_kinds": signal_kinds,
                        "signal_details": signal_details,
                        "controller": self.controller.snapshot(),
                        "model_selector": self.model_selector.snapshot_beliefs(),
                        "model": &model,
                    }));
                }

                if let TurnDecision::Failed(ref msg) = decision
                    && msg != "interrupted" {
                        self.ctx.display.render_error(msg);
                    }
            }
            Err(e) => {
                let info = errors::classify_anyhow(&e);
                self.controller.note_error(false);
                // [LOG] Turn error with full context
                self.ctx.log_event(serde_json::json!({
                    "type": "turn_error",
                    "error": format!("{e}"),
                    "category": format!("{:?}", info.category),
                    "severity": format!("{:?}", info.severity),
                    "controller": self.controller.snapshot(),
                    "model_selector": self.model_selector.snapshot_beliefs(),
                    "model": &model,
                }));
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
                self.controller.reset_stall();
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
        } else if self.controller.is_locked() || matches!(self.controller.get_control_action(), Some(ControlAction::UpgradeModel) | Some(ControlAction::Abort)) {
            crate::config::ModelTier::Pro
        } else {
            // Use Thompson Sampling / Greedy selector for normal model choice
            let selected = self.model_selector.select_greedy();
            crate::config::ModelTier::parse(selected).unwrap_or(crate::config::ModelTier::Flash)
        };
        (tier.model_name().to_string(), self.ctx.api_url.clone())
    }

    fn update_after_turn(&mut self, decision: &TurnDecision) {
        if !self.auto_model_enabled {
            return;
        }

        match decision {
            TurnDecision::Failed(msg) if msg != "interrupted" => {
                // Use Bayesian stall probability update
                self.controller.note_error(false);

                let state = self.controller.format_state();
                self.ctx.log_event(serde_json::json!({
                    "type": "controller_state",
                    "state": state,
                    "decision": "failed",
                }));

                // Show advisory when stall probability is elevated
                if self.controller.stall_probability() > 0.8 {
                    self.ctx.display.render_info(&format!(
                        "Repeated failures: P(stall)={:.3}. Consider /pro or Ctrl-C.",
                        self.controller.stall_probability()
                    ));
                }
            }
            TurnDecision::Stop => {
                // Successful turn → reset stall probability
                self.controller.note_end_turn();
                self.controller.note_progress(true);
                self.controller.reset_stall();
            }
            _ => {}
        }
    }

    async fn handle_control_actions(&mut self) {
        let Some(action) = self.controller.get_control_action() else {
            return;
        };

        let action_str = match action {
            ControlAction::InjectReflectionHint => "InjectReflectionHint",
            ControlAction::UpgradeModel => "UpgradeModel",
            ControlAction::Abort => "Abort",
        };

        // [LOG] Control action taken with full context
        self.ctx.log_event(serde_json::json!({
            "type": "control_action",
            "action": action_str,
            "P_stall": self.controller.stall_probability(),
            "k": self.controller.no_progress_count(),
            "fix_loop": self.controller.has_fix_loop(),
            "controller_snapshot": self.controller.snapshot(),
        }));

        match action {
            ControlAction::InjectReflectionHint => {
                self.ctx.display.render_info(&format!(
                    "Controller: P(stall)={:.3} — injecting reflection hint.",
                    self.controller.stall_probability()
                ));
            }
            ControlAction::UpgradeModel => {
                self.ctx.display.render_info(&format!(
                    "Controller: P(stall)={:.3} — upgrading to Pro.",
                    self.controller.stall_probability()
                ));
                self.ctx.log_event(serde_json::json!({
                    "type": "model_upgrade",
                    "reason": format!("P(stall)={:.3}, k={}", self.controller.stall_probability(), self.controller.no_progress_count()),
                    "new_model": self.resolve_active().0,
                }));
            }
            ControlAction::Abort => {
                self.ctx.display.render_error(&format!(
                    "Controller: P(stall)={:.3}, k={} — agent is stuck. Requesting human intervention.",
                    self.controller.stall_probability(),
                    self.controller.no_progress_count()
                ));
                self.ctx.log_event(serde_json::json!({
                    "type": "controller_abort",
                    "reason": format!("P(stall)={:.3}, k={}", self.controller.stall_probability(), self.controller.no_progress_count()),
                }));
            }
        }
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

/// Creates the OrchActor + command sender pair.
pub fn new_orchestrator(
    ctx: Arc<AgentSharedContext>,
    sub_pool: Arc<SubAgentPool>,
    model_beliefs_path: PathBuf,
) -> (OrchActor, mpsc::UnboundedSender<OrchCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let actor = OrchActor::new(ctx, cmd_rx, sub_pool, model_beliefs_path);
    (actor, cmd_tx)
}
