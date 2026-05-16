use crate::context::AgentSharedContext;
use crate::llm::client::LlmClient;
use crate::protocol::{Event, ToolCallEvent, UsageEvent};
use crate::session::prefix::ImmutablePrefix;
use crate::session::store::{build_tool_call_summary, first_line, ToolResult};
use crate::tools::runner::ToolRunner;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;

/// TurnExecutor runs a single "turn" of the agent loop:
///   Stream (LLM response) → Persist → Tools → Decide (continue/stop)
pub struct TurnExecutor {
    ctx: Arc<AgentSharedContext>,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRunner>,
    compacted_this_turn: bool,
}

/// Represents the outcome of a turn that needs to be actioned.
#[derive(Debug, Clone)]
pub enum TurnEffect {
    SubAgentLaunched {
        session_id: String,
        prompt: String,
        description: String,
        fork: bool,
    },
    PlanCleared,
    PlanConfirmed,
    NeedsPro,
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
        let tools = Arc::new(ToolRunner::new(ctx.clone()));
        Self { ctx, llm, tools, compacted_this_turn: false }
    }

    /// Build or reuse the ImmutablePrefix. Returns the current system_prompt and tools_json.
    /// The prefix is invalidated after plan/summary changes (PlanClear, PlanConfirm, compaction)
    /// and rebuilt on the next call to this method.
    fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
        let mut guard = self.ctx.immutable_prefix.lock().unwrap();
        if let Some(ref prefix) = *guard {
            // Verify fingerprint to catch cache-drift bugs early.
            // If this panics, a mutation path bypassed invalidate_prefix().
            if !prefix.verify_fingerprint() {
                panic!(
                    "ImmutablePrefix fingerprint mismatch — prefix mutated without invalidation. \
                     This will break DeepSeek's prefix-cache alignment."
                );
            }
            return Ok((prefix.system_prompt().to_string(), prefix.tools_json().to_vec()));
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
        Ok((system_prompt, tools_json))
    }

    /// Mark the prefix as stale so it is rebuilt on the next ensure_prefix() call.
    fn invalidate_prefix(&self) {
        *self.ctx.immutable_prefix.lock().unwrap() = None;
    }

    /// Execute a full turn: send user input, stream response, execute tools, decide next.
    pub async fn execute(&mut self, user_input: &str) -> Result<(TurnDecision, Vec<TurnEffect>)> {
        // New user intent: reset storm breaker window and compact guard
        self.tools.reset_storm();
        self.compacted_this_turn = false;

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

            // Compact before each LLM call (skip if already compacted this turn)
            if !self.compacted_this_turn {
                let stats = self.ctx.stats.snapshot().await;
                let compacted = self.ctx.compaction.evaluate_and_compact(
                    "auto",
                    stats.current_context_tokens as usize,
                ).await;
                if let Ok((did_compact, _)) = compacted {
                    if did_compact {
                        self.compacted_this_turn = true;
                        self.invalidate_prefix();
                        messages = self.ctx.store.lines().await?;
                        (system_prompt, tools_json) = self.ensure_prefix()?;
                    }
                }
            }

            // Preflight: estimate tokens and emergency compact if >95% context
            if !self.compacted_this_turn {
                let estimated_tokens: usize = messages.iter()
                    .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
                    .sum::<usize>() + system_prompt.len() / 4;
                let max_ctx = self.ctx.config.max_context_tokens;
                if max_ctx > 0 && estimated_tokens > max_ctx * 95 / 100 {
                    let stats = self.ctx.stats.snapshot().await;
                    let compacted = self.ctx.compaction.evaluate_and_compact(
                        "preflight",
                        stats.current_context_tokens as usize,
                    ).await;
                    if let Ok((did_compact, _)) = compacted {
                        if did_compact {
                            self.compacted_this_turn = true;
                            self.invalidate_prefix();
                        }
                    }
                    messages = self.ctx.store.lines().await?;
                    (system_prompt, tools_json) = self.ensure_prefix()?;
                }
            }

            let mut stream = match self.llm.stream(&self.ctx, messages.clone(), &tools_json, &system_prompt).await {
                Ok(s) => s,
                Err(e) => {
                    return Ok((TurnDecision::Failed(e.to_string()), effects));
                }
            };

            let mut text = String::new();
            let mut thinking = String::new();
            let mut calls: Vec<ToolCallEvent> = Vec::new();
            let mut stop = String::new();
            let mut usage: Option<UsageEvent> = None;
            let mut needs_pro = false;

            // Phase 1: Stream LLM response
            while let Some(result) = stream.next().await {
                let evt = match result {
                    Ok(e) => e,
                    Err(e) => {
                        return Ok((TurnDecision::Failed(e.to_string()), effects));
                    }
                };

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
                        self.ctx.log_event(serde_json::json!({
                            "type":"tool_call",
                            "name": call.name,
                            "id": call.id,
                            "input": call.input_json,
                        }));
                        let summary = build_tool_call_summary(&call.name, &call.fields);
                        self.ctx.display.render_tool_call(&call.name, &summary);
                        calls.push(call);
                    }
                    Event::Usage(u) => {
                        self.ctx.log_event(serde_json::json!({
                            "type":"usage",
                            "input_tokens": u.input_tokens,
                            "output_tokens": u.output_tokens,
                            "cache_read_input_tokens": u.cache_read_input_tokens,
                            "cache_creation_input_tokens": u.cache_creation_input_tokens,
                            "kind":"agent",
                        }));
                        usage = Some(u);
                    }
                    Event::Stop(s) => {
                        self.ctx.log_event(serde_json::json!({"type":"stop","reason":s.reason}));
                        stop = s.reason.clone();
                        break;
                    }
                    Event::Error(e) => {
                        self.ctx.log_event(serde_json::json!({"type":"error","message":e.message}));
                        return Ok((TurnDecision::Failed(e.message), effects));
                    }
                    Event::SelfReport(e) => {
                        self.ctx.log_event(serde_json::json!({"type":"self_report","reason":e.reason}));
                        needs_pro = true;
                    }
                    Event::Retry(_) => {
                        self.ctx.log_event(serde_json::json!({"type":"retry"}));
                        text.clear();
                        thinking.clear();
                        calls.clear();
                        self.ctx.display.render_retry();
                    }
                }
            }

            drop(stream);

            if self.ctx.cancel.is_cancelled() {
                self.ctx.display.render_stop();
                return Ok((TurnDecision::Interrupted, effects));
            }

            // Phase 1b: Scavenge additional tool calls from reasoning/text content
            if !thinking.is_empty() || !text.is_empty() {
                let (scavenged, scavenge_notes) = crate::repair::scavenge_combined(
                    if thinking.is_empty() { None } else { Some(thinking.as_str()) },
                    if text.is_empty() { None } else { Some(text.as_str()) },
                    4,
                );
                for sc in &scavenged {
                    // Only add if not already declared (dedup by name+args)
                    if !calls.iter().any(|c| c.name == sc.name) {
                        let cid = format!("scavenged_{}", calls.len());
                        let fields = std::collections::BTreeMap::new();
                        let input_json: serde_json::Value =
                            serde_json::from_str(&sc.arguments).unwrap_or_default();
                        calls.push(crate::protocol::ToolCallEvent {
                            name: sc.name.clone(),
                            id: cid,
                            input_json,
                            fields,
                            order: Vec::new(),
                        });
                    }
                }
                for note in &scavenge_notes {
                    self.ctx.log_event(serde_json::json!({"type":"scavenge","note":note}));
                }
            }

            // Phase 2: Persist
            self.ctx.store.add_assistant(&text, &thinking, &calls).await?;

            if let Some(ref u) = usage {
                self.ctx.stats.record_usage(u).await;
            }

            // Phase 3: Execute tools
            if !calls.is_empty() {
                let results = self.tools.execute_all(calls.clone()).await?;

                let mut processed_results = Vec::new();
                for mut result in results {
                    if result.tool_name == "PlanClear" {
                        let _ = self.ctx.compaction.evaluate_and_compact(
                            "plan_clear", 0,
                        ).await;
                        let _ = tokio::fs::write(&self.ctx.plan_path, "").await;
                        result.content = "Plan cleared.".to_string();
                        effects.push(TurnEffect::PlanCleared);
                        self.invalidate_prefix();
                    }

                    if result.tool_name == "PlanConfirm" {
                        match tokio::fs::read(&self.ctx.plan_draft_path).await {
                            Ok(data) if !data.is_empty() => {
                                let _ = self.ctx.compaction.evaluate_and_compact(
                                    "plan_confirm", 0,
                                ).await;
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

                    if result.spawns_sub_agent {
                        if let Some(prompt) = result.sub_agent_prompt.take() {
                            let session_id = format!("sub_{}", crate::session::paths::chrono_session_id());
                            effects.push(TurnEffect::SubAgentLaunched {
                                session_id: session_id.clone(),
                                prompt: prompt.clone(),
                                description: result.sub_agent_description.clone().unwrap_or_default(),
                                fork: result.sub_agent_fork,
                            });
                            result.content = format!("Sub-agent started: session_id={}", session_id);
                        }
                    }

                    processed_results.push(result);
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
            }

            // Phase 4: Decide
            let _ = self.ctx.stats.flush_if_dirty().await;
            let stats = self.ctx.stats.snapshot().await;
            self.ctx.display.render_title_update(self.llm.model(), &crate::ui::StatsSnapshot {
                current_turn_count: stats.current_turn_count,
                agent_request_count: stats.agent_request_count,
                total_input_tokens: stats.total_input_tokens,
                total_output_tokens: stats.total_output_tokens,
                current_context_tokens: stats.current_context_tokens,
                max_context_tokens: self.ctx.config.max_context_tokens as u64,
                total_cache_read_tokens: stats.total_cache_read_tokens,
            });
            if needs_pro {
                effects.push(TurnEffect::NeedsPro);
            }
            match stop.as_str() {
                "tool_use" | "tool_calls" => {
                    messages = self.ctx.store.lines().await?;
                    continue;
                }
                "end_turn" | "stop" => {
                    self.ctx.display.render_stop();
                    return Ok((TurnDecision::Stop, effects));
                }
                "error" | "max_tokens" | "length" => {
                    self.ctx.display.render_stop();
                    return Ok((TurnDecision::Failed(format!("stop: {stop}")), effects));
                }
                _ => {
                    self.ctx.display.render_stop();
                    if !stop.is_empty() {
                        return Ok((TurnDecision::Stop, effects));
                    }
                    return Ok((TurnDecision::Stop, effects));
                }
            }
        }

        Ok((TurnDecision::Stop, effects))
    }
}

fn truncate_str(s: &str, n: usize) -> String {
    if s.len() <= n { return s.to_string(); }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    format!("{}...", &s[..end])
}
