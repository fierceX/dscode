use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// CancellationToken provides cooperative cancellation across async tasks.
/// Mirrors Go's context.WithCancel pattern.
#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Cancel the token, waking all waiters.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Returns true if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Wait until cancelled, then return.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        if !self.is_cancelled() {
            notified.await;
        }
    }

    /// Create a child token from this parent. When parent is cancelled,
    /// the child is also cancelled (via shared AtomicBool).
    pub fn child_token(&self) -> Self {
        Self {
            cancelled: self.cancelled.clone(),
            notify: self.notify.clone(),
        }
    }

    /// Create a linked child token. Parent cancellation propagates to the
    /// child, but cancelling the child does not cancel the parent.
    pub fn linked_child_token(&self) -> Self {
        let child = Self::new();
        let parent = self.clone();
        let child_from_parent = child.clone();
        let child_done = child.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = parent.cancelled() => child_from_parent.cancel(),
                _ = child_done.cancelled() => {}
            }
        });
        child
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
