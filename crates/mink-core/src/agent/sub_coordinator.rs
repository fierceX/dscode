use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::agent::text::truncate_str;
use crate::cancel::CancellationToken;
use crate::config::ResolvedConfig as Config;
use crate::context::AgentSharedContext;
use crate::tools::metadata::{ToolFailureKind, ToolStatus};
use crate::tools::runner::ToolExecution;
use futures::FutureExt;
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SUB_AGENT_ABORT_GRACE_MS: u64 = 250;

pub(crate) type SubAgentRunner = Arc<
    dyn Fn(
            Arc<AgentSharedContext>,
            String,
            String,
            bool,
            CancellationToken,
        ) -> Pin<Box<dyn Future<Output = SubAgentResult> + Send>>
        + Send
        + Sync,
>;

pub struct SubAgentCoordinator {
    ctx: Arc<AgentSharedContext>,
    sub_agent_config: Config,
}

struct SubAgentLaunch {
    idx: usize,
    session_id: String,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl SubAgentCoordinator {
    pub fn new(ctx: Arc<AgentSharedContext>, sub_agent_config: Config) -> Self {
        Self {
            ctx,
            sub_agent_config,
        }
    }

    pub async fn process(&self, results: Vec<ToolExecution>) -> Vec<ToolExecution> {
        self.process_with_runner(
            results,
            default_sub_agent_runner(self.sub_agent_config.clone()),
        )
        .await
    }

    pub(crate) async fn process_with_runner(
        &self,
        results: Vec<ToolExecution>,
        runner: SubAgentRunner,
    ) -> Vec<ToolExecution> {
        let mut processed_results = Vec::new();
        let (sub_result_tx, sub_result_rx) =
            tokio::sync::mpsc::unbounded_channel::<(usize, String, SubAgentResult)>();
        let mut launches = Vec::new();
        let sub_semaphore = Arc::new(tokio::sync::Semaphore::new(8));

        for mut result in results {
            if let Some(request) = result.take_sub_agent_request() {
                if self.ctx.is_sub_agent {
                    self.ctx.display.render_info(
                        "Sub-agent recursion blocked: sub-agent cannot spawn sub-agents.",
                    );
                    result.content =
                        "Error: sub-agent recursion blocked: sub-agent cannot spawn sub-agents."
                            .to_string();
                    result.status = ToolStatus::Failed(ToolFailureKind::SafetyBlocked);
                    result.spawns_sub_agent = false;
                    processed_results.push(result);
                    continue;
                }
                let session_id = format!("sub_{}", crate::session::paths::chrono_session_id());
                let fork = request.fork;
                let prompt = request.prompt;

                self.ctx
                    .display
                    .render_sub_agent_status(&session_id, "launched", 0, 0);
                self.ctx.log_event(crate::events::EventLog::SubAgent {
                    session_id: session_id.clone(),
                    status: "launched".into(),
                    input_tokens: None,
                    output_tokens: None,
                });

                let sub_idx = processed_results.len();
                processed_results.push(result);

                let tx = sub_result_tx.clone();
                let ctx = self.ctx.clone();
                let sid = session_id.clone();
                let cancel = self.ctx.cancel.linked_child_token();
                let sub_semaphore = sub_semaphore.clone();
                let runner = runner.clone();
                let launch_cancel = cancel.clone();
                let launch_session_id = session_id.clone();
                let handle = tokio::spawn(async move {
                    let Some(permit) =
                        acquire_sub_agent_permit(sub_semaphore, &cancel, sub_idx, &session_id, &tx)
                            .await
                    else {
                        return;
                    };
                    let sa = run_sub_agent_runner(runner, ctx, sid, prompt, fork, cancel).await;
                    drop(permit);
                    let _ = tx.send((sub_idx, session_id, sa));
                });
                launches.push(SubAgentLaunch {
                    idx: sub_idx,
                    session_id: launch_session_id,
                    cancel: launch_cancel,
                    handle,
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
        mut processed_results: Vec<ToolExecution>,
        mut sub_result_rx: tokio::sync::mpsc::UnboundedReceiver<(usize, String, SubAgentResult)>,
        launches: Vec<SubAgentLaunch>,
    ) -> Vec<ToolExecution> {
        let timeout = self.ctx.tool_config.sub_agent_timeout_secs.max(0);
        let deadline = Instant::now() + Duration::from_secs(timeout as u64);
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
            let now = Instant::now();
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
                    if let Some(launch) = launches.iter().find(|launch| launch.idx == idx) {
                        launch.cancel.cancel();
                    }
                    if let Some(ref mut pr) = processed_results.get_mut(idx) {
                        pr.status = if sa.status == "ok" {
                            ToolStatus::Succeeded
                        } else {
                            ToolStatus::Failed(ToolFailureKind::Unknown)
                        };
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
                        self.ctx.log_event(crate::events::EventLog::SubAgent {
                            session_id,
                            status: sa.status,
                            input_tokens: Some(sa.usage.total_input_tokens),
                            output_tokens: Some(sa.usage.total_output_tokens),
                        });
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
            let mut pending_handles = Vec::new();
            for launch in launches {
                if completed_indices.contains(&launch.idx) {
                    continue;
                }
                launch.cancel.cancel();
                self.ctx
                    .display
                    .render_sub_agent_status(&launch.session_id, reason, 0, 0);
                self.ctx.log_event(crate::events::EventLog::SubAgent {
                    session_id: launch.session_id.clone(),
                    status: reason.into(),
                    input_tokens: None,
                    output_tokens: None,
                });
                if let Some(pr) = processed_results.get_mut(launch.idx) {
                    pr.status = if reason == "cancelled" {
                        ToolStatus::Interrupted
                    } else if reason == "timed_out" {
                        ToolStatus::Failed(ToolFailureKind::Timeout)
                    } else {
                        ToolStatus::Failed(ToolFailureKind::Unknown)
                    };
                    if pr.content.is_empty() {
                        pr.content = match reason {
                            "timed_out" => format!("Sub-agent timed out after {timeout}s."),
                            "cancelled" => "Sub-agent cancelled before completion.".into(),
                            _ => "Sub-agent did not complete.".into(),
                        };
                    }
                }
                pending_handles.push(launch.handle);
            }
            // A zero-second deadline is used as an immediate-cancellation
            // policy. Do not add a cleanup grace period after that deadline;
            // abort and join the tasks so the caller can rely on the timeout.
            let shutdown_grace = if timeout == 0 {
                Duration::ZERO
            } else {
                Duration::from_millis(SUB_AGENT_ABORT_GRACE_MS)
            };
            await_cooperative_sub_agent_shutdown(pending_handles, shutdown_grace).await;
        }

        for pr in &mut processed_results {
            if pr.spawns_sub_agent && pr.content.is_empty() {
                pr.content = "Sub-agent did not complete.".into();
                pr.status = ToolStatus::Failed(ToolFailureKind::Unknown);
            }
        }
        processed_results
    }
}

async fn await_cooperative_sub_agent_shutdown(
    handles: Vec<tokio::task::JoinHandle<()>>,
    grace: Duration,
) {
    if handles.is_empty() {
        return;
    }
    let deadline = Instant::now() + grace;
    while handles.iter().any(|handle| !handle.is_finished()) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep(std::cmp::min(
            deadline.saturating_duration_since(now),
            Duration::from_millis(10),
        ))
        .await;
    }
    for handle in handles {
        if handle.is_finished() {
            let _ = handle.await;
        } else {
            handle.abort();
        }
    }
}

async fn acquire_sub_agent_permit(
    sub_semaphore: Arc<tokio::sync::Semaphore>,
    cancel: &CancellationToken,
    sub_idx: usize,
    session_id: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<(usize, String, SubAgentResult)>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    tokio::select! {
        permit = sub_semaphore.acquire_owned() => match permit {
            Ok(permit) => Some(permit),
            Err(_) => {
                let _ = tx.send((
                    sub_idx,
                    session_id.to_string(),
                    SubAgentResult {
                        status: "failed".into(),
                        thinking: String::new(),
                        text: "Sub-agent semaphore closed.".into(),
                        usage: Default::default(),
                    },
                ));
                None
            }
        },
        _ = cancel.cancelled() => {
            let _ = tx.send((
                sub_idx,
                session_id.to_string(),
                SubAgentResult {
                    status: "cancelled".into(),
                    thinking: String::new(),
                    text: "Sub-agent cancelled before execution.".into(),
                    usage: Default::default(),
                },
            ));
            None
        }
    }
}

async fn run_sub_agent_runner(
    runner: SubAgentRunner,
    ctx: Arc<AgentSharedContext>,
    sid: String,
    prompt: String,
    fork: bool,
    cancel: CancellationToken,
) -> SubAgentResult {
    let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runner(ctx, sid, prompt, fork, cancel)
    })) {
        Ok(future) => future,
        Err(panic_info) => {
            return SubAgentResult {
                status: "failed".into(),
                thinking: String::new(),
                text: format!("Sub-agent task panicked: {}", panic_message(panic_info)),
                usage: Default::default(),
            };
        }
    };
    match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(sa) => sa,
        Err(panic_info) => SubAgentResult {
            status: "failed".into(),
            thinking: String::new(),
            text: format!("Sub-agent task panicked: {}", panic_message(panic_info)),
            usage: Default::default(),
        },
    }
}

fn default_sub_agent_runner(config: Config) -> SubAgentRunner {
    Arc::new(move |ctx, sid, prompt, fork, cancel| {
        let config = config.clone();
        Box::pin(async move {
            match SubAgentExecutor::new_with_cancel(ctx, sid, fork, config, cancel).await {
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

fn panic_message(panic_info: Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(s) = panic_info.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_info.downcast_ref::<String>() {
        s.clone()
    } else {
        "sub-agent thread panicked".to_string()
    }
}
