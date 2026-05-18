use crate::context::AgentSharedContext;
use crate::agent::turn::{TurnExecutor, TurnDecision};
use crate::llm::client::{AsyncLlClient, LlmClient};
use crate::session::stats::Stats;
use crate::session::store::ConversationStore;
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

/// SubAgentExecutor runs a child agent in an isolated context.
pub struct SubAgentExecutor {
    child_store: Arc<ConversationStore>,
    child_ctx: Arc<AgentSharedContext>,
    session_id: String,
}

impl SubAgentExecutor {
    pub async fn new(
        parent_ctx: &Arc<AgentSharedContext>,
        session_id: &str,
        fork: bool,
    ) -> Result<Self> {
        let paths = crate::session::paths::paths_for(
            &parent_ctx.home, &parent_ctx.cwd, session_id,
        );
        crate::session::paths::ensure_dir(&paths.session_dir).await?;

        // 共享初始化：创建文件、store、stats
        let (child_store, child_stats) = crate::session::init::init_session_base(
            &parent_ctx.home, &parent_ctx.cwd, session_id,
        ).await?;

        if fork {
            // Copy parent conversation, summary, plan to child session (ignore errors)
            let parent_conv = parent_ctx.store.path();
            if parent_conv.exists() { let _ = tokio::fs::copy(parent_conv, &paths.conversation).await; }
            if parent_ctx.summary_path.exists() { let _ = tokio::fs::copy(&parent_ctx.summary_path, &paths.summary).await; }
            if parent_ctx.plan_path.exists() { let _ = tokio::fs::copy(&parent_ctx.plan_path, &paths.plan).await; }
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

        let child_ctx = Arc::new(AgentSharedContext {
            config: parent_ctx.config.clone(),
            cwd: parent_ctx.cwd.clone(),
            home: parent_ctx.home.clone(),
            api_url: parent_ctx.api_url.clone(),
            store: child_store.clone(),
            stats: child_stats,
            compaction: child_compaction,
            cancel: parent_ctx.cancel.child_token(),
            display: parent_ctx.display.clone(),
            tool_timeout_secs: parent_ctx.tool_timeout_secs,
            tool_result_max_bytes: parent_ctx.tool_result_max_bytes,
            file_write_max_bytes: parent_ctx.file_write_max_bytes,
            events_path: paths.events.clone(),
            summary_path: paths.summary.clone(),
            plan_path: paths.plan.clone(),
            plan_draft_path: paths.plan_draft.clone(),
            immutable_prefix: Mutex::new(None),
        });

        Ok(Self {
            child_store,
            child_ctx,
            session_id: session_id.to_string(),
        })
    }

    /// Execute the sub-agent with the given prompt.
    pub async fn execute(&self, prompt: &str) -> SubAgentResult {
        self.child_ctx.display.render_sub_agent_status(
            &self.session_id, "running", 0, 0,
        );
        let result = self.run_impl(prompt).await;

        let stats = self.child_ctx.stats.snapshot().await;

        match result {
            Ok((thinking, text)) => SubAgentResult {
                status: "ok".into(),
                thinking,
                text,
                usage: stats,
            },
            Err(e) => SubAgentResult {
                status: "failed".into(),
                thinking: String::new(),
                text: format!("Sub-agent failed: {e}"),
                usage: stats,
            },
        }
    }

    async fn run_impl(&self, prompt: &str) -> Result<(String, String)> {
        let model_name = crate::config::resolve_model_name(&self.child_ctx.config.model);
        let api_url = &self.child_ctx.api_url;
        let llm: Arc<dyn LlmClient> = Arc::new(AsyncLlClient::new(
            model_name, &self.child_ctx.config.api_key, api_url,
        )?);
        let mut executor = TurnExecutor::new(self.child_ctx.clone(), llm);
        let (decision, _effects) = executor.execute(prompt).await?;

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
                                            && let Some(t) = b.get("thinking").and_then(|v| v.as_str()) {
                                                result_thinking = t.to_string();
                                            }
                                    }
                                    "text" => {
                                        if result_text.is_empty()
                                            && let Some(t) = b.get("text").and_then(|v| v.as_str()) {
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
            TurnDecision::Interrupted => {
                Ok((String::new(), "Sub-agent interrupted.".into()))
            }
            TurnDecision::Failed(msg) => {
                Err(anyhow::anyhow!(msg))
            }
        }
    }
}
