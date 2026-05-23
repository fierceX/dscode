use crate::agent::turn::{TurnDecision, TurnExecutor};
use crate::context::AgentSharedContext;
use crate::llm::client::{AsyncLlClient, LlmClient};
use crate::session::stats::Stats;
use crate::session::store::ConversationStore;
use crate::ui::Display;
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
    tx: std::sync::mpsc::Sender<crate::tui::TuiSignal>,
    session_id: String,
    thinking: Mutex<String>,
    text: Mutex<String>,
}

impl CaptureDisplay {
    fn new(tx: std::sync::mpsc::Sender<crate::tui::TuiSignal>, session_id: &str) -> Self {
        Self {
            tx,
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
        let _ = self.tx.send(crate::tui::TuiSignal::SubAgentStream {
            session_id: self.session_id.clone(),
            kind: crate::tui::SubAgentStreamKind::Thinking,
            content: c.to_string(),
        });
    }
    fn render_text(&self, c: &str) {
        self.text.lock().unwrap().push_str(c);
        let _ = self.tx.send(crate::tui::TuiSignal::SubAgentStream {
            session_id: self.session_id.clone(),
            kind: crate::tui::SubAgentStreamKind::Text,
            content: c.to_string(),
        });
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
    #[cfg(test)]
    llm_override: Option<Arc<dyn LlmClient>>,
}

impl SubAgentExecutor {
    pub async fn new(
        parent_ctx: Arc<AgentSharedContext>,
        session_id: String,
        fork: bool,
    ) -> Result<Self> {
        let paths =
            crate::session::paths::paths_for(&parent_ctx.home, &parent_ctx.cwd, &session_id);
        crate::session::paths::ensure_dir(&paths.session_dir).await?;

        // 共享初始化：创建文件、store、stats
        let (child_store, child_stats) =
            crate::session::init::init_session_base(&parent_ctx.home, &parent_ctx.cwd, &session_id)
                .await?;

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

        let child_compaction = Arc::new(crate::session::compaction::CompactionEngine::new(
            child_store.clone(),
            paths.summary.clone(),
            paths.plan.clone(),
            paths.plan_draft.clone(),
            parent_ctx.cwd.clone(),
            parent_ctx.home.clone(),
            parent_ctx.config.skills.clone(),
            parent_ctx.api_url.clone(),
            &parent_ctx.config,
            child_stats.clone(),
            reqwest::Client::new(),
        ));

        // 用 CaptureDisplay 拦截子代理输出，并实时转发到父 TUI
        let tui_tx = parent_ctx.sub_stream_tx.as_ref().and_then(|a| {
            a.downcast_ref::<std::sync::mpsc::Sender<crate::tui::TuiSignal>>()
                .cloned()
        });
        let capture = Arc::new(CaptureDisplay::new(
            tui_tx.unwrap_or_else(|| {
                // REPL mode: create a dummy channel
                let (tx, _rx) = std::sync::mpsc::channel();
                tx
            }),
            &session_id,
        ));
        let parent_display = parent_ctx.display.clone();

        let child_ctx = Arc::new(AgentSharedContext {
            config: parent_ctx.config.clone(),
            cwd: parent_ctx.cwd.clone(),
            home: parent_ctx.home.clone(),
            api_url: parent_ctx.api_url.clone(),
            store: child_store.clone(),
            stats: child_stats,
            compaction: child_compaction,
            cancel: parent_ctx.cancel.child_token(),
            display: capture.clone(), // ← CaptureDisplay，阻断实时输出
            sub_stream_tx: None,
            tool_config: parent_ctx.tool_config.clone(),
            events_path: paths.events.clone(),
            summary_path: paths.summary.clone(),
            plan_path: paths.plan.clone(),
            plan_draft_path: paths.plan_draft.clone(),
            immutable_prefix: Mutex::new(None),
            is_sub_agent: true,
            interrupt: parent_ctx.interrupt.clone(),
        });

        Ok(Self {
            child_store,
            child_ctx,
            capture,
            parent_display,
            session_id,
            #[cfg(test)]
            llm_override: None,
        })
    }

    #[cfg(test)]
    pub(crate) async fn new_with_llm(
        parent_ctx: Arc<AgentSharedContext>,
        session_id: String,
        fork: bool,
        llm: Arc<dyn LlmClient>,
    ) -> Result<Self> {
        let mut executor = Self::new(parent_ctx, session_id, fork).await?;
        executor.llm_override = Some(llm);
        Ok(executor)
    }

    /// Execute the sub-agent with the given prompt.
    pub async fn execute(self, prompt: String) -> SubAgentResult {
        // 在 self 被 move 进 run_impl 之前，提取所有权字段
        let capture = self.capture.clone();
        let parent_display = self.parent_display.clone();
        let session_id = self.session_id.clone();
        let child_ctx = self.child_ctx.clone();

        let result = self.run_impl(prompt).await;

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
        let model_name = crate::config::resolve_model_name(&self.child_ctx.config.model);
        let api_url = &self.child_ctx.api_url;
        #[cfg(test)]
        let llm: Arc<dyn LlmClient> = if let Some(llm) = self.llm_override.clone() {
            llm
        } else {
            Arc::new(AsyncLlClient::new(
                model_name,
                &self.child_ctx.config.api_key,
                api_url,
            )?)
        };
        #[cfg(not(test))]
        let llm: Arc<dyn LlmClient> = Arc::new(AsyncLlClient::new(
            model_name,
            &self.child_ctx.config.api_key,
            api_url,
        )?);
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
            TurnDecision::Failed(msg) => Err(anyhow::anyhow!(msg)),
        }
    }
}
