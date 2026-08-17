use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// CancellationToken provides cooperative cancellation across async tasks.
/// Mirrors Go's context.WithCancel pattern.
///
/// Child tokens are registered with their parent through weak references, so
/// dropping a child (with or without an explicit `cancel()`) does not leak a
/// Tokio task or keep the parent alive indefinitely.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
    children: Mutex<Vec<Weak<Inner>>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
                children: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Cancel the token, waking all waiters and propagating to live children.
    pub fn cancel(&self) {
        if self.is_cancelled() {
            return;
        }
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
        let live_children = {
            let mut children = self
                .inner
                .children
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut live = Vec::new();
            children.retain(|child| match child.upgrade() {
                Some(child) => {
                    live.push(Self { inner: child });
                    true
                }
                None => false,
            });
            live
        };
        for child in live_children {
            child.cancel();
        }
    }

    /// Returns true if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Wait until cancelled, then return.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.inner.notify.notified();
        if !self.is_cancelled() {
            notified.await;
        }
    }

    /// Create a linked child token. Parent cancellation propagates to the
    /// child, but cancelling the child does not cancel the parent.
    ///
    /// This is implemented with a weak registration list instead of a spawned
    /// Tokio task, so every child token is free of task-leak and parent-retention
    /// costs even when callers drop it without calling `cancel()`.
    pub fn linked_child_token(&self) -> Self {
        let child_inner = Arc::new(Inner {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
            children: Mutex::new(Vec::new()),
        });
        {
            let mut children = self
                .inner
                .children
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            children.retain(|child| child.upgrade().is_some());
            children.push(Arc::downgrade(&child_inner));
        }
        let child = Self { inner: child_inner };
        if self.is_cancelled() {
            child.cancel();
        }
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
