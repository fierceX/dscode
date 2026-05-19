use crate::agent::sub_executor::SubAgentExecutor;
use crate::session::stats::Stats;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc, oneshot};

/// Report sent back to the orchestrator when a sub-agent completes.
#[derive(Debug, Clone)]
pub struct SubAgentReport {
    pub session_id: String,
    pub status: String,
    pub thinking: String,
    pub text: String,
    pub usage: Stats,
}

/// SubAgentPool limits concurrent sub-agent execution using a Semaphore.
pub struct SubAgentPool {
    semaphore: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    result_tx: mpsc::UnboundedSender<SubAgentReport>,
}

impl SubAgentPool {
    pub fn new(max_concurrent: usize, result_tx: mpsc::UnboundedSender<SubAgentReport>) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            active: Arc::new(AtomicUsize::new(0)),
            result_tx,
        }
    }

    /// Launch a sub-agent. Returns (session_id, wait_rx) immediately.
    /// `wait_rx` resolves when the sub-agent completes (for sync waiting).
    pub async fn launch(
        &self,
        ctx: Arc<super::super::context::AgentSharedContext>,
        prompt: String,
        _description: String,
        fork: bool,
        session_id: String,
    ) -> Result<(String, oneshot::Receiver<SubAgentReport>), tokio::sync::AcquireError> {
        let permit = self.semaphore.clone().acquire_owned().await?;
        self.active.fetch_add(1, Ordering::SeqCst);

        let session_id = if session_id.is_empty() {
            format!("sub_{}", crate::session::paths::chrono_session_id())
        } else {
            session_id
        };
        let id = session_id.clone();
        let tx = self.result_tx.clone();
        let active = self.active.clone();
        let (wait_tx, wait_rx) = oneshot::channel();

        tokio::spawn(async move {
            let _permit = permit;

            let executor = match SubAgentExecutor::new(&ctx, &id, fork).await {
                Ok(e) => e,
                Err(err) => {
                    let report = SubAgentReport {
                        session_id: id,
                        status: "failed".into(),
                        thinking: String::new(),
                        text: format!("Failed to create sub-agent: {err}"),
                        usage: Stats::default(),
                    };
                    let _ = tx.send(report.clone());
                    let _ = wait_tx.send(report);
                    active.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
            };
            let result = executor.execute(&prompt).await;

            let report = SubAgentReport {
                session_id: id,
                status: result.status,
                thinking: result.thinking,
                text: result.text,
                usage: result.usage,
            };
            let _ = tx.send(report.clone());
            let _ = wait_tx.send(report);

            active.fetch_sub(1, Ordering::SeqCst);
        });

        Ok((session_id, wait_rx))
    }

    /// Returns the number of currently active sub-agents.
    pub fn active_count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Wait for all sub-agents to complete.
    pub async fn drain(&self) {
        while self.active.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
