use crate::agent::sub_executor::SubAgentExecutor;
use crate::session::stats::Stats;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc};

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

    /// Launch a sub-agent. Returns the session_id immediately.
    /// The sub-agent runs in a tokio task; results are sent via result_tx.
    pub async fn launch(
        &self,
        ctx: Arc<super::super::context::AgentSharedContext>,
        prompt: String,
        _description: String,
        fork: bool,
    ) -> Result<String, tokio::sync::AcquireError> {
        let permit = self.semaphore.clone().acquire_owned().await?;
        self.active.fetch_add(1, Ordering::SeqCst);

        let session_id = format!("sub_{}", crate::session::paths::chrono_session_id());
        let id = session_id.clone();
        let tx = self.result_tx.clone();
        let active = self.active.clone();

        tokio::spawn(async move {
            let _permit = permit;

            let executor = match SubAgentExecutor::new(&ctx, &id, fork).await {
                Ok(e) => e,
                Err(err) => {
                    let _ = tx.send(SubAgentReport {
                        session_id: id,
                        status: "failed".into(),
                        thinking: String::new(),
                        text: format!("Failed to create sub-agent: {err}"),
                        usage: Stats::default(),
                    });
                    active.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
            };
            let result = executor.execute(&prompt).await;

            let _ = tx.send(SubAgentReport {
                session_id: id,
                status: result.status,
                thinking: result.thinking,
                text: result.text,
                usage: result.usage,
            });

            active.fetch_sub(1, Ordering::SeqCst);
        });

        Ok(session_id)
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
