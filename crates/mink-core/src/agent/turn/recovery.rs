const MAX_REPLAN_ATTEMPTS_PER_INPUT: usize = 1;

use super::*;

impl super::TurnExecutor {
    pub(super) fn apply_signal_recovery_guard(
        &mut self,
        calls: Vec<ToolCallEvent>,
    ) -> (Vec<ToolCallEvent>, Vec<crate::tools::runner::ToolExecution>) {
        if !self.local.signal_recovery_guard || calls.is_empty() {
            return (calls, Vec::new());
        }

        let mut iter = calls.into_iter();
        let first = iter
            .next()
            .expect("signal recovery guard already checked calls is non-empty");
        let call_context = crate::tools::semantic_capabilities::CapabilityCallContext {
            tool_name: &first.name,
            input: &first.input_json,
            resource_router: &self.ctx.resource_router,
            filesystem_backend: self.ctx.tool_resolution_context.filesystem_backend(),
        };
        let decision = self.recovery_policy.classify_first_call(&call_context);
        if let crate::agent::recovery_policy::RecoveryFirstCallDecision::Blocked(guidance) =
            decision
        {
            guidance
                .validate(&self.ctx.tool_surface)
                .expect("RecoveryPolicy emitted an inactive tool reference");
            let max_blocks = self.ctx.config.signal.guard_max_blocks;
            if self.local.guard_blocks < max_blocks {
                self.local.guard_blocks += 1;
                self.ctx
                    .log_event(crate::events::EventLog::SignalRecoveryGuard {
                        action: "blocked_non_inspection".into(),
                        tool: first.name.clone(),
                        tool_use_id: first.id.clone(),
                        reason: guidance.content.clone(),
                        guard_blocks: self.local.guard_blocks,
                    });
                let blocked: Vec<crate::tools::runner::ToolExecution> = std::iter::once(first)
                    .chain(iter)
                    .map(|call| blocked_by_signal_recovery(call, guidance.content.clone()))
                    .collect();
                return (Vec::new(), blocked);
            }
            // 连续拦截达到 guard_max_blocks：绕过守卫放行调用，并强制下一次
            // 决策注入证据。事件 action 与实际行为保持一致（bypassed 而非 blocked）。
            self.local.signal_recovery_guard = false;
            self.local.guard_bypassed = true;
            self.ctx
                .log_event(crate::events::EventLog::SignalRecoveryGuard {
                    action: "bypassed_max_blocks".into(),
                    tool: first.name.clone(),
                    tool_use_id: first.id.clone(),
                    reason: guidance.content.clone(),
                    guard_blocks: self.local.guard_blocks,
                });
            let mut allowed = Vec::new();
            allowed.push(first);
            allowed.extend(iter);
            return (allowed, Vec::new());
        }

        self.local.signal_recovery_guard = false;
        let mut allowed = Vec::new();
        allowed.push(first);
        allowed.extend(iter);
        (allowed, Vec::new())
    }

    /// 回滚循环窗口内（最近 SignalConfig.seq_window 步）被编辑过的路径。
    /// - 回滚目标是 read 基线而非 record_edit 的编辑后内容（否则恒等 no-op）；
    /// - 只回滚窗口内路径，窗口之前的合法编辑保持不动；
    /// - 写回经 atomic_replace（同目录临时文件 + rename），失败只记录不中断 turn。
    async fn apply_rollback(&mut self) -> Result<()> {
        let rollback_window_steps = self.ctx.config.signal.seq_window;
        let paths = self
            .signal_processor
            .evidence()
            .edited_paths_since(rollback_window_steps);
        if paths.is_empty() {
            return Ok(());
        }
        let mut rolled_back: Vec<serde_json::Value> = Vec::new();
        for raw in paths {
            let path = std::path::PathBuf::from(&raw);
            let full = if path.is_absolute() {
                path.clone()
            } else {
                self.ctx.cwd.join(&path)
            };
            let candidate = {
                let snapshots = self
                    .ctx
                    .snapshots
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                snapshots.latest_read_snapshot(&full)
            };
            let Some(snapshot) = candidate else {
                continue;
            };
            let Ok(current) = tokio::fs::read_to_string(&full).await else {
                continue;
            };
            let normalized = crate::tools::snapshot::normalize_snapshot_text(&current);
            if normalized == snapshot.text {
                continue; // 幂等：磁盘内容已与基线一致。
            }
            let restored = crate::tools::snapshot::restore_text_shape(
                snapshot.bom,
                snapshot.crlf,
                &snapshot.text,
            );
            // atomic_replace 会用临时文件替换目标，因此必须显式保留原权限。
            let original_permissions = std::fs::metadata(&full).map(|meta| meta.permissions()).ok();
            if let Err(error) =
                crate::session::atomic_file::atomic_replace(&full, restored.as_bytes())
            {
                self.ctx
                    .log_event(crate::events::EventLog::SignalRollbackError {
                        path: full.display().to_string(),
                        error: error.to_string(),
                    });
                continue;
            }
            if let Some(permissions) = original_permissions {
                let _ = std::fs::set_permissions(&full, permissions);
            }
            self.ctx
                .memo_mutation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            rolled_back.push(serde_json::json!({
                "path": full.display().to_string(),
                "to_tag": snapshot.tag,
            }));
        }
        if !rolled_back.is_empty() {
            self.ctx.log_event(crate::events::EventLog::SignalRollback {
                files: rolled_back.clone(),
            });
            self.ctx.display.render_info(&format!(
                "Signal rollback: restored {} file(s) to their last read snapshot",
                rolled_back.len()
            ));
        }
        Ok(())
    }

    /// 只继承有界任务报告（目标/编辑路径/失败证据），用全新上下文重新规划。
    /// 成功返回子代理文本；不可用或失败返回 None（调用方降级为既有响应）。
    /// `belief` 为当前信念度，如实写入报告（禁止用占位值误导子代理）。
    async fn run_replan(&mut self, belief: f64) -> Result<Option<String>> {
        if !self.ctx.config.signal_policy.allows_restart() {
            return Ok(None);
        }
        let s = self.ctx.config.signal.clone();
        if self.local.replan_attempts >= MAX_REPLAN_ATTEMPTS_PER_INPUT {
            return Ok(None);
        }
        if !self.ctx.tool_surface.has("SubAgent") {
            return Ok(None);
        }
        self.local.replan_attempts += 1;
        let mut child_cfg = self.sub_agent_config.clone();
        child_cfg.max_turns = s.replan_max_turns;
        child_cfg.max_tokens = child_cfg.max_tokens.min(s.replan_token_budget);
        let session_id = format!("replan_{}", crate::session::paths::chrono_session_id());
        let report = self.build_replan_report(belief);
        let executor = match crate::agent::sub_executor::SubAgentExecutor::new(
            self.ctx.clone(),
            session_id.clone(),
            false,
            child_cfg,
        )
        .await
        {
            Ok(executor) => executor,
            Err(error) => {
                self.ctx
                    .log_event(crate::events::EventLog::SignalReplanError {
                        attempts: self.local.replan_attempts,
                        session_id: session_id.clone(),
                        error: error.to_string(),
                    });
                self.ctx.display.render_info(&format!(
                    "Signal replan unavailable: {error}; falling back to evidence/guard",
                ));
                return Ok(None);
            }
        };
        let result = executor.execute(report).await;
        self.ctx.log_event(crate::events::EventLog::SignalReplan {
            attempts: self.local.replan_attempts,
            session_id: session_id.clone(),
            status: result.status.clone(),
            text_len: result.text.len(),
        });
        if result.status != "ok" || result.text.trim().is_empty() {
            return Ok(None);
        }
        let injection = format!(
            "[replan] A fresh sub-agent re-analyzed the task with a clean context and produced \
             this revised plan. Treat it as new evidence, not a user request.\n{}",
            result.text
        );
        self.ctx.store.add_runtime_user(&injection).await?;
        self.ctx
            .display
            .render_info("Signal replan: fresh sub-agent produced a revised plan");
        Ok(Some(result.text))
    }

    /// 构造有界恢复任务报告；不携带父会话历史。
    fn build_replan_report(&self, belief: f64) -> String {
        let budget = self.ctx.config.signal.evidence_max_chars.min(2_000);
        let evidence = self.signal_processor.evidence().render(budget, belief).text;
        let paths = self.signal_processor.evidence().edited_paths.join(", ");
        format!(
            "You are re-planning for a parent coding agent that is stuck in a failure loop.\n\
             Original user goal: {}\n\
             Files edited this turn: {}\n\
             Failure evidence:\n{}\n\
             Produce a short revised plan: diagnose the most likely root cause, then list the \
             next 3 concrete verification-first steps. Output the plan only; do not continue \
             the parent's work.",
            self.local.current_user_input,
            if paths.is_empty() { "(none)" } else { &paths },
            evidence,
        )
    }

    /// Phase 4: 根据 stop reason 决策本轮是否结束。
    /// 返回 `Some(TurnDecision)` 表示需要从 execute() 返回，`None` 表示继续循环。
    /// `is_last_turn` 表示当前已经是最后一个允许的 LLM 轮次；此时 todo 提醒
    /// 只记录、不再强制发起一次额外的 LLM 请求。
    pub(super) async fn decide_next(
        &mut self,
        stop: &str,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
        is_last_turn: bool,
    ) -> Result<Option<TurnDecision>> {
        // 更新标题栏信念度
        let _ = self.ctx.stats.flush_if_dirty().await;
        let current_belief = belief.as_ref().map_or(0.0, |bt| bt.belief());
        crate::ui::render_title_snapshot(&self.ctx, self.model_label(), current_belief).await;

        match stop {
            "tool_use" | "tool_calls" => {
                // DecisionEngine 决策是否注入（含内部冷却逻辑）
                let signal_enabled = self.ctx.config.signal_policy.enabled();
                if let Some(decision) = self.decide_signal_recovery(signal_enabled, belief).await? {
                    return Ok(Some(decision));
                }
                Ok(None) // 继续循环
            }
            "end_turn" | "stop" | "done" => {
                if !self.local.todo_final_reminder_sent
                    && self
                        .ctx
                        .todo_store
                        .snapshot()
                        .items
                        .iter()
                        .any(|item| item.status == crate::session::todo::TodoStatus::InProgress)
                    && let Some(provider) = self.ctx.todo_advance_provider()
                {
                    self.ctx
                        .store
                        .add_runtime_user(&format!(
                            "<todo-final-reminder>Todo items remain in_progress. Before finishing, call {provider} to complete verified work or pause work that is no longer active. If the work is blocked and should remain active, state that explicitly.</todo-final-reminder>"
                        ))
                        .await?;
                    self.local.todo_final_reminder_sent = true;
                    if !is_last_turn {
                        return Ok(None);
                    }
                }
                self.ctx.display.render_stop(stop);
                Ok(Some(TurnDecision::Stop))
            }
            "error" | "max_tokens" | "length" => {
                self.ctx.display.render_stop(stop);
                Ok(Some(TurnDecision::Failed(format!("stop: {stop}"))))
            }
            _ => {
                self.ctx.display.render_stop(stop);
                Ok(Some(TurnDecision::Stop))
            }
        }
    }

    pub(super) async fn decide_signal_recovery(
        &mut self,
        signal_enabled: bool,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
    ) -> Result<Option<TurnDecision>> {
        if !signal_enabled {
            return Ok(None);
        }
        if let Some(bt) = belief {
            let b = bt.belief();
            let hard = self.signal_processor.hard_failures() as usize;
            let soft = self.signal_processor.soft_failures() as usize;
            let decision = if self.local.guard_bypassed {
                // 守卫达到上限被绕过：强制一次证据注入，即使处于冷却。
                self.local.guard_bypassed = false;
                crate::agent::decision::Decision::Inject(
                    crate::agent::decision::RecoveryDirective {
                        severity: crate::agent::decision::RecoverySeverity::Warning,
                    },
                )
            } else {
                self.decision_engine.decide_with_signals(b, hard, soft)
            };
            match decision {
                crate::agent::decision::Decision::Inject(directive) => {
                    let budget = self.ctx.config.signal.evidence_max_chars;
                    let batch = self.signal_processor.evidence().render(budget, b);
                    let fresh = self.signal_processor.evidence().is_fresh(batch.hash);
                    if fresh {
                        self.signal_processor
                            .evidence_mut()
                            .mark_injected(batch.hash);
                    }
                    let text = if fresh {
                        batch.text
                    } else {
                        format!(
                            "[trajectory]
- no new evidence since the last injection
[detector] belief {b:.2} (reference only)"
                        )
                    };
                    self.ctx
                        .display
                        .render_info(&format!("Injecting trajectory evidence (belief {b:.2})"));
                    self.ctx.store.add_runtime_user(&text).await?;
                    let policy = self.ctx.config.signal_policy;
                    // Warning 级在 state-ops 及以上策略先回滚窗口内编辑。
                    if directive.severity == crate::agent::decision::RecoverySeverity::Warning
                        && policy.allows_state_ops()
                    {
                        self.apply_rollback().await?;
                    }
                    // 同一输入内连续第二次 Warning 时用 fresh 子代理重新规划；
                    // 成功后注入新计划并跳过守卫。
                    let is_warning =
                        directive.severity == crate::agent::decision::RecoverySeverity::Warning;
                    if is_warning {
                        self.local.warning_count += 1;
                    }
                    let mut replanned = false;
                    if is_warning && self.local.warning_count >= 2 && policy.allows_restart() {
                        replanned = self.run_replan(b).await?.is_some();
                        if replanned {
                            bt.reset();
                        }
                    }
                    // 恢复守卫降级为状态门：仅在 Warning 级启用且未成功重规划时。
                    self.local.signal_recovery_guard =
                        is_warning && !replanned && policy.allows_state_ops();
                }
                crate::agent::decision::Decision::Abort => {
                    let policy = self.ctx.config.signal_policy;
                    // 非交互环境在 restart 及以上策略先尝试重启。
                    if policy.allows_restart()
                        && !self.ctx.config.interactive
                        && self.run_replan(b).await?.is_some()
                    {
                        // 策略重启成功：重置信念并继续本轮（新证据基线）。
                        bt.reset();
                        self.ctx.display.render_info(&format!(
                            "DecisionEngine: belief {b:.2} — retrying with a fresh replan instead of handing over",
                        ));
                        return Ok(None);
                    }
                    // 用户接管仅在 full 策略启用；较低策略直接失败。
                    if !policy.allows_handover() {
                        self.ctx
                            .display
                            .render_error(&format!("DecisionEngine: aborting (belief {b:.2})."));
                        return Ok(Some(TurnDecision::Failed(
                            "aborted by DecisionEngine".into(),
                        )));
                    }
                    let budget = self.ctx.config.signal.evidence_max_chars;
                    let batch = self.signal_processor.evidence().render(budget, b);
                    self.ctx.log_event(crate::events::EventLog::SignalHandover {
                        belief: b,
                        edited_paths: self.signal_processor.evidence().edited_paths.clone(),
                        evidence: batch.text.clone(),
                        options: vec![
                            "retry".into(),
                            "rollback_and_retry".into(),
                            "replan".into(),
                            "abandon".into(),
                        ],
                    });
                    self.ctx.display.render_error(&format!(
                        "DecisionEngine: handing over (belief {b:.2}).\n{}",
                        batch.text
                    ));
                    return Ok(Some(TurnDecision::Failed(
                        "signal handover: reliability belief fell below the abort threshold".into(),
                    )));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}
