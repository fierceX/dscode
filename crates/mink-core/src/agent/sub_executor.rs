use crate::agent::turn::{TurnDecision, TurnExecutor};
use crate::context::AgentSharedContext;
use crate::llm::client::{BackendLlmClient, LlmClient};
use crate::runtime::context_build::{AgentContextBuild, build_agent_context};
use crate::session::stats::Stats;
use crate::session::store::ConversationStore;
use crate::ui::{Display, SubAgentStreamKind, SubAgentStreamSink};
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// Result of a sub-agent execution.
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub status: String,
    pub thinking: String,
    pub text: String,
    pub usage: Stats,
}

/// CaptureDisplay silently buffers sub-agent output instead of forwarding to the parent TUI.
/// This prevents sub-agent thinking/text from polluting the main conversation view.
struct CaptureDisplay {
    stream_sink: Option<Arc<dyn SubAgentStreamSink>>,
    session_id: String,
    thinking: Mutex<String>,
    text: Mutex<String>,
}

impl CaptureDisplay {
    fn new(stream_sink: Option<Arc<dyn SubAgentStreamSink>>, session_id: &str) -> Self {
        Self {
            stream_sink,
            session_id: session_id.to_string(),
            thinking: Mutex::new(String::new()),
            text: Mutex::new(String::new()),
        }
    }
    fn take_thinking(&self) -> String {
        std::mem::take(&mut *self.thinking.lock().unwrap())
    }
    fn take_text(&self) -> String {
        std::mem::take(&mut *self.text.lock().unwrap())
    }
}

impl Display for CaptureDisplay {
    fn render_thinking(&self, c: &str) {
        self.thinking.lock().unwrap().push_str(c);
        if let Some(sink) = &self.stream_sink {
            sink.render_sub_agent_stream(&self.session_id, SubAgentStreamKind::Thinking, c);
        }
    }
    fn render_text(&self, c: &str) {
        self.text.lock().unwrap().push_str(c);
        if let Some(sink) = &self.stream_sink {
            sink.render_sub_agent_stream(&self.session_id, SubAgentStreamKind::Text, c);
        }
    }
    fn render_tool_call(&self, _n: &str, _s: &str) {}
    fn render_tool_result(&self, _n: &str, _c: &str) {}
    fn render_stop(&self) {}
    fn render_error(&self, _m: &str) {}
    fn render_retry(&self) {}
    fn render_info(&self, _m: &str) {}
    fn render_title_update(&self, _m: &str, _s: &crate::ui::StatsSnapshot) {}
    fn render_sub_agent_status(&self, _sid: &str, _st: &str, _it: u64, _ot: u64) {}
    fn render_prompt(&self) {}
    fn render_clear_line(&self) {}
}

/// SubAgentExecutor runs a child agent in an isolated context.
pub struct SubAgentExecutor {
    child_store: Arc<ConversationStore>,
    child_ctx: Arc<AgentSharedContext>,
    capture: Arc<CaptureDisplay>,
    parent_display: Arc<dyn Display>,
    session_id: String,
}

impl SubAgentExecutor {
    pub async fn new(
        parent_ctx: Arc<AgentSharedContext>,
        session_id: String,
        fork: bool,
    ) -> Result<Self> {
        let cancel = parent_ctx.cancel.linked_child_token();
        Self::new_with_cancel(parent_ctx, session_id, fork, cancel).await
    }

    pub(crate) async fn new_with_cancel(
        parent_ctx: Arc<AgentSharedContext>,
        session_id: String,
        fork: bool,
        cancel: crate::cancel::CancellationToken,
    ) -> Result<Self> {
        let capture = Arc::new(CaptureDisplay::new(
            parent_ctx.sub_stream_tx.clone(),
            &session_id,
        ));
        let parent_display = parent_ctx.display.clone();
        let built = build_agent_context(AgentContextBuild {
            config: parent_ctx.config.clone(),
            home: parent_ctx.home.clone(),
            cwd: parent_ctx.cwd.clone(),
            session_id: session_id.clone(),
            session_layout: parent_ctx.session_layout,
            api_url: parent_ctx.api_url.clone(),
            display: capture.clone(),
            sub_stream_tx: None,
            cancel,
            interrupt: parent_ctx.interrupt.clone(),
            is_sub_agent: true,
            usage_journal: Some(parent_ctx.usage.clone()),
            read_only_fs: parent_ctx.read_only_fs.clone(),
            resource_session_id: parent_ctx.vfs_scope.resource_session_id.clone(),
            resource_handlers: Vec::new(),
            skill_providers: Vec::new(),
            runtime_skills: Vec::new(),
            skill_discovery_policy: crate::capabilities::SkillDiscoveryPolicy::Defaults,
            llm_backend: parent_ctx.llm_backend.clone(),
            resource_router: Some(parent_ctx.resource_router.clone()),
            capability_snapshot: Some(parent_ctx.capability_snapshot.clone()),
        })
        .await?;
        let child_ctx = built.ctx;
        let paths = built.paths;
        let child_store = child_ctx.store.clone();

        if fork {
            // Copy parent conversation, summary, plan to child session (ignore errors)
            let parent_conv = parent_ctx.store.path();
            if parent_conv.exists() {
                let _ = tokio::fs::copy(parent_conv, &paths.conversation).await;
            }
            if parent_ctx.summary_path.exists() {
                let _ = tokio::fs::copy(&parent_ctx.summary_path, &paths.summary).await;
            }
            if parent_ctx.plan_path.exists() {
                let _ = tokio::fs::copy(&parent_ctx.plan_path, &paths.plan).await;
            }
        }

        Ok(Self {
            child_store,
            child_ctx,
            capture,
            parent_display,
            session_id,
        })
    }

    /// Execute the sub-agent with the given prompt.
    pub async fn execute(self, prompt: String) -> SubAgentResult {
        // 在 self 被 move 进 run_impl 之前，提取所有权字段
        let capture = self.capture.clone();
        let parent_display = self.parent_display.clone();
        let session_id = self.session_id.clone();
        let child_ctx = self.child_ctx.clone();

        let timeout_secs = child_ctx.tool_config.sub_agent_timeout_secs.max(1) as u64;
        let cancel = child_ctx.cancel.clone();
        let result = tokio::select! {
            result = self.run_impl(prompt) => result,
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
                cancel.cancel();
                Err(anyhow::anyhow!("Sub-agent timed out after {timeout_secs}s"))
            }
        };

        let captured_thinking = capture.take_thinking();
        let captured_text = capture.take_text();

        let stats = child_ctx.stats.snapshot().await;

        // 向父 Display 发送完整子代理输出（TUI 用户可点击查看）
        parent_display.render_sub_agent_output(
            &session_id,
            match &result {
                Ok(_) => "ok",
                Err(_) => "failed",
            },
            &captured_thinking,
            &captured_text,
            stats.total_input_tokens,
            stats.total_output_tokens,
        );

        match result {
            Ok((thinking, text)) => SubAgentResult {
                status: "ok".into(),
                thinking: if thinking.is_empty() {
                    captured_thinking
                } else {
                    thinking
                },
                text: if text.is_empty() { captured_text } else { text },
                usage: stats,
            },
            Err(e) => SubAgentResult {
                status: "failed".into(),
                thinking: captured_thinking,
                text: if captured_text.is_empty() {
                    format!("Sub-agent failed: {e}")
                } else {
                    captured_text
                },
                usage: stats,
            },
        }
    }

    async fn run_impl(self, prompt: String) -> Result<(String, String)> {
        let resolved = crate::config::model_resolver(&self.child_ctx.config)
            .resolve(&self.child_ctx.config.model);
        let llm: Arc<dyn LlmClient> = Arc::new(BackendLlmClient::new(
            self.child_ctx.llm_backend.clone(),
            resolved.actual,
            resolved.alias,
        ));
        // 子代理内部也可调用 SubAgent，用独立池（容量1，结果丢弃）
        let mut executor = TurnExecutor::new(self.child_ctx.clone(), llm);
        let (decision, _effects) = Box::pin(executor.execute(&prompt, None)).await?;

        match decision {
            TurnDecision::Stop | TurnDecision::Continue => {
                // Read back the assistant text from the CHILD store
                let lines = self.child_store.lines().await?;
                let mut result_thinking = String::new();
                let mut result_text = String::new();
                for line in lines.iter().rev() {
                    if line.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                        if let Some(content) = line.get("content").and_then(|v| v.as_array()) {
                            for b in content {
                                match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                                    "thinking" => {
                                        if result_thinking.is_empty()
                                            && let Some(t) =
                                                b.get("thinking").and_then(|v| v.as_str())
                                        {
                                            result_thinking = t.to_string();
                                        }
                                    }
                                    "text" => {
                                        if result_text.is_empty()
                                            && let Some(t) = b.get("text").and_then(|v| v.as_str())
                                        {
                                            result_text = t.to_string();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        break;
                    }
                }
                Ok((result_thinking, result_text))
            }
            TurnDecision::Interrupted => Ok((String::new(), "Sub-agent interrupted.".into())),
            TurnDecision::MaxTurnsExceeded => Err(anyhow::anyhow!(
                "sub-agent max_turns exhausted before end_turn"
            )),
            TurnDecision::Failed(msg) => Err(anyhow::anyhow!(msg)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn child_inherits_resource_session_but_has_own_agent_session() {
        let mut parent = crate::regression::test_context_for_agent("sub-vfs-scope")
            .await
            .unwrap();
        Arc::get_mut(&mut parent)
            .expect("test context should be uniquely owned")
            .vfs_scope
            .resource_session_id = "tenant-knowledge".into();

        let parent_snapshot = parent.capability_snapshot.clone();
        let child = SubAgentExecutor::new(parent, "sub-agent-session".into(), false)
            .await
            .unwrap();
        assert_eq!(
            child.child_ctx.vfs_scope.resource_session_id,
            "tenant-knowledge"
        );
        assert_eq!(
            child.child_ctx.vfs_scope.agent_session_id,
            "sub-agent-session"
        );
        assert!(Arc::ptr_eq(
            &child.child_ctx.capability_snapshot,
            &parent_snapshot
        ));
    }
}
