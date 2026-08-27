use super::*;

impl super::TurnExecutor {
    pub(super) fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
        self.prefix.ensure()
    }

    pub(super) fn project_request_messages(
        &self,
        messages: &[serde_json::Value],
    ) -> Result<Vec<serde_json::Value>> {
        // Full request projection: single-consumption image lifecycle (§7.3)
        // then the plan — identical to what the compactor estimates with.
        crate::session::plan::project_full_request(
            &self.ctx.plan_path,
            self.ctx.config.plan_projection_tail,
            messages,
        )
    }

    pub(super) async fn reconcile_todo_state(
        &self,
        messages: &mut Vec<serde_json::Value>,
    ) -> Result<bool> {
        let Some(read_provider) = self.ctx.todo_read_provider() else {
            return Ok(false);
        };
        let visible = crate::session::todo::visible_revision(messages)?;
        let snapshot = self.ctx.todo_store.snapshot();
        if visible > snapshot.revision {
            anyhow::bail!(
                "todo conversation revision {visible} is newer than persisted revision {}; refusing to continue",
                snapshot.revision
            );
        }
        if visible == snapshot.revision {
            return Ok(false);
        }
        let message = crate::session::todo::sync_message(&snapshot, read_provider);
        self.ctx
            .store
            .append_runtime_message(message.clone())
            .await?;
        messages.push(message);
        Ok(true)
    }

    /// 通过 turn 级统一守卫尝试上下文压缩。成功时更新 messages/system_prompt/tools_json。
    /// 返回 true 表示进行了压缩。
    pub(super) async fn try_compact(
        &mut self,
        trigger: &str,
        messages: &mut Vec<serde_json::Value>,
        system_prompt: &mut String,
        tools_json: &mut Vec<serde_json::Value>,
    ) -> Result<bool> {
        let model_name = self.model_name.clone();
        let model_alias = self.model_alias.clone();
        let compacted = self
            .compactor
            .maybe_compact(
                trigger,
                messages,
                system_prompt,
                tools_json,
                LlmModelTarget::new(&model_name, model_alias.as_deref()),
            )
            .await?;
        if compacted {
            self.reconcile_todo_state(messages).await?;
        }
        Ok(compacted)
    }

    /// Phase 1: 发送 LLM 请求并流式读取响应，返回 `StreamOutput`。
    /// 网络/协议错误通过 `bail!` 传播，由调用方转为 `TurnDecision::Failed`。
    pub(super) async fn stream_llm_response(
        &mut self,
        messages: &[serde_json::Value],
        system_prompt: &str,
        tools_json: &[serde_json::Value],
        current_context_tokens: usize,
    ) -> anyhow::Result<StreamOutput> {
        let mut stream = match crate::llm::client::stream_backend(
            &self.llm_backend,
            &self.ctx,
            &self.model_name,
            self.model_alias.as_deref(),
            messages,
            tools_json,
            system_prompt,
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) if is_context_overflow_message(&error.to_string()) => {
                return Err(anyhow::Error::new(ContextOverflowError {
                    message: error.to_string(),
                }));
            }
            Err(error) => return Err(error),
        };

        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCallEvent> = Vec::new();
        let mut stop = String::new();
        let mut usage: Option<UsageEvent> = None;
        let mut saw_stop = false;
        let mut saw_any_event = false;
        let mut saw_visible_output = false;
        let stream_started = Instant::now();
        let mut last_event_at = stream_started;
        let mut last_heartbeat_at = stream_started;
        let first_event_timeout = positive_duration(self.ctx.config.llm_first_event_timeout_secs);
        let idle_timeout = positive_duration(self.ctx.config.llm_idle_timeout_secs);
        let heartbeat = positive_duration(self.ctx.config.llm_wait_heartbeat_secs);

        loop {
            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx.log_event(crate::events::EventLog::Stop {
                    reason: "interrupted".into(),
                });
                stop = "interrupted".into();
                saw_stop = true;
                break;
            }

            let result = tokio::select! {
                result = stream.next() => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {
                    self.check_llm_wait_timeout(
                        saw_any_event,
                        stream_started,
                        last_event_at,
                        first_event_timeout,
                        idle_timeout,
                    )?;
                    self.maybe_render_llm_wait_heartbeat(
                        saw_any_event,
                        stream_started,
                        last_event_at,
                        &mut last_heartbeat_at,
                        heartbeat,
                    );
                    if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                        self.ctx.log_event(crate::events::EventLog::Stop {
                            reason: "interrupted".into(),
                        });
                        stop = "interrupted".into();
                        saw_stop = true;
                        break;
                    }
                    continue;
                }
            };
            let Some(result) = result else {
                break;
            };
            let evt = result?;
            saw_any_event = true;
            last_event_at = Instant::now();

            match evt {
                Event::Thinking(t) => {
                    saw_visible_output = true;
                    self.ctx.log_event(crate::events::EventLog::Thinking {
                        version: None,
                        content: t.content.clone(),
                    });
                    self.ctx.display.render_thinking(&t.content);
                    thinking.push_str(&t.content);
                }
                Event::Text(t) => {
                    saw_visible_output = true;
                    self.ctx.log_event(crate::events::EventLog::Text {
                        version: None,
                        content: t.content.clone(),
                    });
                    self.ctx.display.render_text(&t.content);
                    text.push_str(&t.content);
                }
                Event::ToolCall(call) => {
                    saw_visible_output = true;
                    self.ctx.log_event(crate::events::EventLog::ToolCall {
                        version: None,
                        name: call.name.clone(),
                        id: call.id.clone(),
                        input: call.input_json.clone(),
                    });
                    let summary = build_tool_call_summary(&call.name, &call.fields);
                    self.ctx
                        .display
                        .render_tool_call(&crate::ui::ToolCallDisplay {
                            tool_use_id: &call.id,
                            tool_name: &call.name,
                            summary: &summary,
                            input: Some(&call.input_json),
                        });
                    calls.push(call);
                }
                Event::Usage(u) => {
                    self.ctx.log_event(crate::events::EventLog::Usage {
                        version: None,
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        cache_read_input_tokens: u.cache_read_input_tokens,
                        cache_creation_input_tokens: u.cache_creation_input_tokens,
                        context_tokens: Some(current_context_tokens),
                        max_context: Some(self.ctx.config.max_context_tokens),
                        kind: "agent".into(),
                    });
                    usage = Some(u);
                }
                Event::UsageUnavailable => {}
                Event::Stop(s) => {
                    self.ctx.log_event(crate::events::EventLog::Stop {
                        reason: s.reason.clone(),
                    });
                    stop = s.reason;
                    saw_stop = true;
                    break;
                }
                Event::Error(e) => {
                    self.ctx.log_event(crate::events::EventLog::Error {
                        version: None,
                        message: e.message.clone(),
                    });
                    if !saw_visible_output && is_context_overflow_message(&e.message) {
                        return Err(anyhow::Error::new(ContextOverflowError {
                            message: e.message,
                        }));
                    }
                    anyhow::bail!("{}", e.message);
                }
                Event::Retry(_) => {
                    self.ctx.log_event(crate::events::EventLog::Retry);
                    text.clear();
                    thinking.clear();
                    calls.clear();
                    stop.clear();
                    usage = None;
                    saw_stop = false;
                    self.ctx.display.render_retry();
                }
            }
        }

        drop(stream);
        if !saw_stop {
            anyhow::bail!("stream ended without stop event");
        }
        Ok(StreamOutput {
            text,
            thinking,
            calls,
            stop,
            usage,
        })
    }

    fn check_llm_wait_timeout(
        &self,
        saw_any_event: bool,
        stream_started: Instant,
        last_event_at: Instant,
        first_event_timeout: Option<Duration>,
        idle_timeout: Option<Duration>,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        if !saw_any_event {
            if let Some(timeout) = first_event_timeout
                && now.duration_since(stream_started) >= timeout
            {
                let message = format!(
                    "LLM stream first event timeout after {} seconds",
                    timeout.as_secs()
                );
                self.ctx.log_event(crate::events::EventLog::TurnError {
                    error: message.clone(),
                    category: "llm_first_event_timeout".into(),
                    severity: None,
                    belief: None,
                    model: None,
                    elapsed_ms: Some(now.duration_since(stream_started).as_millis() as u64),
                    idle_ms: None,
                });
                anyhow::bail!(message);
            }
            return Ok(());
        }

        if let Some(timeout) = idle_timeout
            && now.duration_since(last_event_at) >= timeout
        {
            let message = format!(
                "LLM stream idle timeout after {} seconds without events",
                timeout.as_secs()
            );
            self.ctx.log_event(crate::events::EventLog::TurnError {
                error: message.clone(),
                category: "llm_idle_timeout".into(),
                severity: None,
                belief: None,
                model: None,
                elapsed_ms: None,
                idle_ms: Some(now.duration_since(last_event_at).as_millis() as u64),
            });
            anyhow::bail!(message);
        }
        Ok(())
    }

    fn maybe_render_llm_wait_heartbeat(
        &self,
        saw_any_event: bool,
        stream_started: Instant,
        last_event_at: Instant,
        last_heartbeat_at: &mut Instant,
        heartbeat: Option<Duration>,
    ) {
        let Some(heartbeat) = heartbeat else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(*last_heartbeat_at) < heartbeat {
            return;
        }
        *last_heartbeat_at = now;
        let phase = if saw_any_event { "idle" } else { "first_event" };
        let elapsed = now.duration_since(stream_started).as_secs();
        let idle = now.duration_since(last_event_at).as_secs();
        self.ctx.log_event(crate::events::EventLog::LlmWait {
            phase: phase.into(),
            elapsed_secs: elapsed,
            idle_secs: idle,
        });
        self.ctx.display.render_info(&format!(
            "Waiting for model response... elapsed={}s idle={}s",
            elapsed, idle
        ));
    }
}
