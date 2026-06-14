use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::cancel::CancellationToken;
use crate::context::AgentSharedContext;
use crate::tools::runner::ToolRunResult;
use crate::util::truncate_str;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) type SubAgentRunner = Arc<
    dyn Fn(Arc<AgentSharedContext>, String, String, bool, CancellationToken) -> SubAgentResult
        + Send
        + Sync,
>;

pub struct SubAgentCoordinator {
    ctx: Arc<AgentSharedContext>,
}

struct SubAgentLaunch {
    idx: usize,
    session_id: String,
    cancel: CancellationToken,
}

impl SubAgentCoordinator {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self { ctx }
    }

    pub async fn process(&self, results: Vec<ToolRunResult>) -> Vec<ToolRunResult> {
        self.process_with_runner(results, default_sub_agent_runner())
            .await
    }

    pub(crate) async fn process_with_runner(
        &self,
        results: Vec<ToolRunResult>,
        runner: SubAgentRunner,
    ) -> Vec<ToolRunResult> {
        let mut processed_results = Vec::new();
        let (sub_result_tx, sub_result_rx) =
            tokio::sync::mpsc::unbounded_channel::<(usize, String, SubAgentResult)>();
        let mut launches = Vec::new();
        let sub_semaphore = Arc::new(tokio::sync::Semaphore::new(8));

        for mut result in results {
            if result.spawns_sub_agent
                && let Some(prompt) = result.sub_agent_prompt.take()
            {
                if self.ctx.is_sub_agent {
                    self.ctx.display.render_info(
                        "Sub-agent recursion blocked: sub-agent cannot spawn sub-agents.",
                    );
                    result.content =
                        "Error: sub-agent recursion blocked: sub-agent cannot spawn sub-agents."
                            .to_string();
                    result.spawns_sub_agent = false;
                    processed_results.push(result);
                    continue;
                }
                let session_id = format!("sub_{}", crate::session::paths::chrono_session_id());
                let fork = result.sub_agent_fork;

                self.ctx
                    .display
                    .render_sub_agent_status(&session_id, "launched", 0, 0);
                self.ctx.log_event(serde_json::json!({
                    "type": "sub_agent",
                    "session_id": session_id.clone(),
                    "status": "launched",
                }));

                let sub_idx = processed_results.len();
                processed_results.push(result);

                let tx = sub_result_tx.clone();
                let ctx = self.ctx.clone();
                let sid = session_id.clone();
                let cancel = self.ctx.cancel.linked_child_token();
                let sub_semaphore = sub_semaphore.clone();
                let runner = runner.clone();
                launches.push(SubAgentLaunch {
                    idx: sub_idx,
                    session_id: session_id.clone(),
                    cancel: cancel.clone(),
                });
                std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let permit_rt = tokio::runtime::Builder::new_current_thread()
                            .enable_time()
                            .build()
                            .expect("sub-agent semaphore runtime");
                        let _permit = permit_rt
                            .block_on(sub_semaphore.acquire_owned())
                            .expect("sub-agent semaphore never closed");
                        runner(ctx, sid, prompt, fork, cancel)
                    }));
                    let sa = match result {
                        Ok(sa) => sa,
                        Err(panic_info) => {
                            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                                s.to_string()
                            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                "sub-agent thread panicked".to_string()
                            };
                            SubAgentResult {
                                status: "failed".into(),
                                thinking: String::new(),
                                text: format!("Sub-agent thread panicked: {msg}"),
                                usage: Default::default(),
                            }
                        }
                    };
                    let _ = tx.send((sub_idx, session_id, sa));
                });
            } else {
                processed_results.push(result);
            }
        }

        self.collect_results(processed_results, sub_result_rx, launches)
            .await
    }

    async fn collect_results(
        &self,
        mut processed_results: Vec<ToolRunResult>,
        mut sub_result_rx: tokio::sync::mpsc::UnboundedReceiver<(usize, String, SubAgentResult)>,
        launches: Vec<SubAgentLaunch>,
    ) -> Vec<ToolRunResult> {
        let timeout = self.ctx.tool_config.sub_agent_timeout_secs;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout as u64);
        let mut sub_completed = 0usize;
        let sub_expected = launches.len();
        let mut completed_indices = BTreeSet::new();
        let mut incomplete_reason: Option<&'static str> = None;
        while sub_completed < sub_expected {
            if self.ctx.cancel.is_cancelled() || self.ctx.interrupt.load(Ordering::SeqCst) {
                self.ctx
                    .display
                    .render_info("Sub-agent collection cancelled.");
                incomplete_reason = Some("cancelled");
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                self.ctx
                    .display
                    .render_error(&format!("Sub-agent batch timed out after {}s.", timeout));
                incomplete_reason = Some("timed_out");
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match tokio::time::timeout(remaining, sub_result_rx.recv()).await {
                Ok(Some((idx, session_id, sa))) => {
                    sub_completed += 1;
                    completed_indices.insert(idx);
                    if let Some(ref mut pr) = processed_results.get_mut(idx) {
                        pr.content = format!(
                            "[sub-agent {}] {} (in={}, out={})\nThinking: {}\nText: {}",
                            session_id,
                            sa.status,
                            sa.usage.total_input_tokens,
                            sa.usage.total_output_tokens,
                            sa.thinking,
                            sa.text
                        );
                        let preview = truncate_str(&sa.thinking, 60);
                        if sa.status != "ok" {
                            self.ctx.display.render_error(&format!(
                                "[sub-agent {}] failed: {}",
                                session_id, preview
                            ));
                        }
                        self.ctx.display.render_sub_agent_status(
                            &session_id,
                            &sa.status,
                            sa.usage.total_input_tokens,
                            sa.usage.total_output_tokens,
                        );
                        self.ctx.log_event(serde_json::json!({
                            "type": "sub_agent",
                            "session_id": session_id,
                            "status": sa.status,
                            "input_tokens": sa.usage.total_input_tokens,
                            "output_tokens": sa.usage.total_output_tokens,
                        }));
                        self.ctx
                            .stats
                            .record_sub_agent(
                                sa.usage.agent_request_count,
                                sa.usage.total_input_tokens,
                                sa.usage.total_output_tokens,
                                sa.usage.total_cache_read_tokens,
                                sa.usage.total_cache_creation_tokens,
                            )
                            .await;
                    }
                }
                Ok(None) => {
                    incomplete_reason = Some("channel_closed");
                    break;
                }
                Err(_) => continue,
            }
        }

        if let Some(reason) = incomplete_reason {
            for launch in &launches {
                if completed_indices.contains(&launch.idx) {
                    continue;
                }
                launch.cancel.cancel();
                self.ctx
                    .display
                    .render_sub_agent_status(&launch.session_id, reason, 0, 0);
                self.ctx.log_event(serde_json::json!({
                    "type": "sub_agent",
                    "session_id": launch.session_id,
                    "status": reason,
                }));
                if let Some(pr) = processed_results.get_mut(launch.idx)
                    && pr.content.is_empty()
                {
                    pr.content = match reason {
                        "timed_out" => format!("Sub-agent timed out after {timeout}s."),
                        "cancelled" => "Sub-agent cancelled before completion.".into(),
                        _ => "Sub-agent did not complete.".into(),
                    };
                }
            }
        }

        for pr in &mut processed_results {
            if pr.spawns_sub_agent && pr.content.is_empty() {
                pr.content = "Sub-agent did not complete.".into();
            }
        }
        processed_results
    }
}

fn default_sub_agent_runner() -> SubAgentRunner {
    Arc::new(|ctx, sid, prompt, fork, cancel| {
        let rt = tokio::runtime::Runtime::new().expect("sub-agent runtime");
        rt.block_on(async move {
            match SubAgentExecutor::new_with_cancel(ctx, sid, fork, cancel).await {
                Ok(executor) => executor.execute(prompt).await,
                Err(e) => SubAgentResult {
                    status: "failed".into(),
                    thinking: String::new(),
                    text: format!("Failed to create sub-agent: {e}"),
                    usage: Default::default(),
                },
            }
        })
    })
}
