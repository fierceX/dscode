use crate::agent::turn::{TurnDecision, TurnExecutor};
use crate::config::ResolvedConfig as Config;
use crate::context::AgentSharedContext;
use crate::runtime::context_build::{AgentContextBuild, build_agent_context};
use crate::session::metadata::SessionSeed;
use crate::session::paths::SessionLayout;
use crate::session::stats::Stats;
use crate::session::store::ConversationStore;
use crate::ui::{Display, SubAgentStreamKind, SubAgentStreamSink};
use anyhow::{Result, bail};
use std::path::{Component, Path};
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
    fn render_tool_call(&self, _call: &crate::ui::ToolCallDisplay<'_>) {}
    fn render_tool_result(&self, _result: &crate::ui::PresentedToolResultDisplay<'_>) {}
    fn render_stop(&self, _reason: &str) {}
    fn render_signal(&self, _kind: &str, _severity: f64, _message: &str) {}
    fn render_error(&self, _m: &str) {}
    fn render_retry(&self) {}
    fn render_info(&self, _m: &str) {}
    fn render_title_update(&self, _m: &str, _s: &crate::ui::StatsSnapshot) {}
    fn render_sub_agent_status(&self, _sid: &str, _st: &str, _it: u64, _ot: u64) {}
    fn render_sub_agent_output(
        &self,
        _sid: &str,
        _status: &str,
        _thinking: &str,
        _text: &str,
        _in_tokens: u64,
        _out_tokens: u64,
    ) {
    }
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
        config: Config,
    ) -> Result<Self> {
        let cancel = parent_ctx.cancel.linked_child_token();
        Self::new_with_cancel(parent_ctx, session_id, fork, config, cancel).await
    }

    pub(crate) async fn new_with_cancel(
        parent_ctx: Arc<AgentSharedContext>,
        session_id: String,
        fork: bool,
        config: Config,
        cancel: crate::cancel::CancellationToken,
    ) -> Result<Self> {
        let mut components = Path::new(&session_id).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            bail!("invalid sub-agent session id: {session_id}");
        }
        let capture = Arc::new(CaptureDisplay::new(
            parent_ctx.sub_stream_tx.clone(),
            &session_id,
        ));
        let parent_display = parent_ctx.display.clone();
        let parent_session_dir = parent_ctx
            .store
            .path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("parent conversation has no session directory"))?;
        let child_home = parent_session_dir.join("subagents").join(&session_id);
        prepare_child_home(parent_session_dir, &child_home, fork).await?;

        let built = build_agent_context(AgentContextBuild {
            config,
            home: child_home,
            cwd: parent_ctx.cwd.clone(),
            session_id: session_id.clone(),
            session_layout: SessionLayout::Isolated,
            resolved_paths: None,
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
            custom_tools: parent_ctx.custom_tools.as_ref().clone(),
            prefix_source: parent_ctx.prefix_source.clone(),
        })
        .await?;
        let child_ctx = built.ctx;
        crate::session::metadata::ensure_metadata(
            &built.paths,
            &parent_ctx.cwd,
            SessionSeed {
                alias: None,
                title: Some(format!("Sub-agent {session_id}")),
                first_prompt: None,
            },
        )
        .await?;
        let child_store = child_ctx.store.clone();

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
        let executor =
            TurnExecutor::new(self.child_ctx.clone(), self.child_ctx.llm_backend.clone())
                .with_model_target(resolved.actual, resolved.alias);
        let mut executor = executor;
        let (decision, _effects) = Box::pin(executor.execute(&prompt, None)).await?;

        match decision {
            TurnDecision::Stop => {
                // Read back the assistant text from the CHILD store
                let mut result_thinking = String::new();
                let mut result_text = String::new();
                if let Some(line) = self.child_store.last_assistant_message().await?
                    && let Some(content) = line.get("content").and_then(|v| v.as_array())
                {
                    for b in content {
                        match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "thinking" => {
                                if result_thinking.is_empty()
                                    && let Some(t) = b.get("thinking").and_then(|v| v.as_str())
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

async fn prepare_child_home(parent: &Path, child: &Path, fork: bool) -> Result<()> {
    if child.exists() {
        bail!("sub-agent home already exists: {}", child.display());
    }
    let child_parent = child
        .parent()
        .ok_or_else(|| anyhow::anyhow!("sub-agent home has no parent"))?;
    ensure_real_directory(child_parent).await?;
    if fork {
        let parent = parent.to_path_buf();
        let destination = child.to_path_buf();
        let copy_destination = destination.clone();
        tokio::task::spawn_blocking(move || clone_session_dir(&parent, &copy_destination))
            .await??;
        for name in ["events.jsonl", "stats.json", "usage.jsonl", "session.json"] {
            let path = destination.join(name);
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&destination).await;
                    return Err(error.into());
                }
            }
        }
    } else {
        tokio::fs::create_dir(child).await?;
    }
    Ok(())
}

async fn ensure_real_directory(path: &Path) -> Result<()> {
    match tokio::fs::create_dir(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }

    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "sub-agent directory is not a real directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn clone_session_dir(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir(destination)?;
    if let Err(error) = copy_session_entries(source, destination, true) {
        let _ = std::fs::remove_dir_all(destination);
        return Err(error);
    }
    Ok(())
}

fn copy_session_entries(source: &Path, destination: &Path, root: bool) -> Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if root && entry.file_name().to_string_lossy() == "subagents" {
            continue;
        }
        // Exclude the image cache tree: child agents own an isolated cache
        // and must never inherit parent objects (v7 §5.1).
        if entry.file_name().to_string_lossy() == "cache" {
            continue;
        }
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir(&target)?;
            copy_session_entries(&entry.path(), &target, false)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            bail!(
                "unsupported entry in session fork: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "sub_executor_tests.rs"]
mod tests;
