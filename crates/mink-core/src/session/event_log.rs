use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use tokio::sync::oneshot;

const EVENT_LOG_QUEUE_CAPACITY: usize = 1024;

enum EventLogCmd {
    Append(String),
    Flush {
        done: oneshot::Sender<io::Result<()>>,
    },
}

/// Serializes event-log writes for one session.
///
/// `log_event()` stays synchronous, but instead of opening the append file on
/// every event it enqueues onto a bounded channel drained by a dedicated OS
/// thread. When the queue is full, `send()` intentionally blocks: this is the
/// backpressure path and bounds memory, matching the old synchronous writer's
/// never-silently-drop behavior while keeping the common case off the tokio
/// worker.
#[derive(Clone)]
pub(crate) struct EventLogWriter {
    tx: SyncSender<EventLogCmd>,
    warned: Arc<AtomicBool>,
}

impl EventLogWriter {
    pub(crate) fn start(path: PathBuf) -> Self {
        let (tx, rx) = sync_channel::<EventLogCmd>(EVENT_LOG_QUEUE_CAPACITY);
        let warned = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let thread_warned = warned.clone();
        let thread_failure = failure.clone();

        let _ = std::thread::Builder::new()
            .name("mink-event-log".to_string())
            .spawn(move || run_writer(path, rx, &thread_warned, &thread_failure));

        Self { tx, warned }
    }

    /// Enqueue one JSON line. Blocks only when the bounded queue is full.
    pub(crate) fn send(&self, line: String) -> bool {
        if self.tx.send(EventLogCmd::Append(line)).is_ok() {
            true
        } else {
            warn_once(&self.warned, "event log writer is closed");
            false
        }
    }

    pub(crate) async fn flush(&self) -> io::Result<()> {
        let (done, done_rx) = oneshot::channel();
        if self.tx.send(EventLogCmd::Flush { done }).is_err() {
            return Err(io::Error::other("event log writer is closed"));
        }
        match done_rx.await {
            Ok(result) => result,
            Err(_) => Err(io::Error::other("event log writer dropped flush ack")),
        }
    }
}

fn run_writer(
    path: PathBuf,
    rx: Receiver<EventLogCmd>,
    warned: &AtomicBool,
    failure: &Mutex<Option<String>>,
) {
    let mut file = None;

    while let Ok(command) = rx.recv() {
        match command {
            EventLogCmd::Append(line) => {
                if file.is_none() {
                    file = open_append(&path, warned, failure);
                }
                if let Some(handle) = file.as_mut()
                    && let Err(error) = writeln!(handle, "{line}")
                {
                    let message = format!("failed to write event log {}: {error}", path.display());
                    warn_once(warned, &message);
                    set_failure(failure, message);
                    // Drop the handle and retry opening on the next event
                    // instead of treating one failed write as permanent.
                    file = None;
                }
            }
            EventLogCmd::Flush { done } => {
                let result = match file.as_mut() {
                    Some(file) => file.flush(),
                    None => match failure
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                    {
                        Some(message) => Err(io::Error::other(message)),
                        // No events have been written yet: flushing an empty
                        // session is a no-op, not an error.
                        None => Ok(()),
                    },
                };
                let _ = done.send(result);
            }
        }
    }

    if let Some(file) = file.as_mut() {
        let _ = file.flush();
    }
}

fn open_append(
    path: &std::path::Path,
    warned: &AtomicBool,
    failure: &Mutex<Option<String>>,
) -> Option<std::fs::File> {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => {
            clear_failure(failure);
            Some(file)
        }
        Err(error) => {
            let message = format!("failed to open event log {}: {error}", path.display());
            warn_once(warned, &message);
            set_failure(failure, message);
            None
        }
    }
}

fn set_failure(failure: &Mutex<Option<String>>, message: String) {
    *failure.lock().unwrap_or_else(|error| error.into_inner()) = Some(message);
}

fn clear_failure(failure: &Mutex<Option<String>>) {
    *failure.lock().unwrap_or_else(|error| error.into_inner()) = None;
}

pub(crate) fn warn_once(warned: &AtomicBool, message: &str) {
    if !warned.swap(true, Ordering::SeqCst) {
        eprintln!("[mink] Warning: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_preserves_order_and_flushes() {
        let dir = std::env::temp_dir().join(format!(
            "mink-event-log-test-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("events.jsonl");
        let writer = EventLogWriter::start(path.clone());

        for index in 0..100 {
            assert!(writer.send(format!("{{\"index\":{index}}}")));
        }
        writer.flush().await.unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 100);
        assert_eq!(lines[0], r#"{"index":0}"#);
        assert_eq!(lines[99], r#"{"index":99}"#);

        drop(writer);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn bounded_queue_never_drops_events() {
        let dir = std::env::temp_dir().join(format!(
            "mink-event-log-bound-test-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("events.jsonl");
        let writer = EventLogWriter::start(path.clone());

        let event_count = EVENT_LOG_QUEUE_CAPACITY * 4;
        for index in 0..event_count {
            assert!(writer.send(format!("{{\"index\":{index}}}")));
        }
        writer.flush().await.unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents.lines().count(), event_count);
        drop(writer);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn flush_without_any_events_is_a_noop() {
        let path = std::env::temp_dir().join(format!(
            "mink-event-log-empty-{}-{}.jsonl",
            std::process::id(),
            uuid_like()
        ));
        let writer = EventLogWriter::start(path.clone());
        writer.flush().await.unwrap();
        drop(writer);
    }

    #[tokio::test]
    async fn open_failure_is_reported_and_recovers_on_retry() {
        let root = std::env::temp_dir().join(format!(
            "mink-event-log-open-test-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        let missing_dir = root.join("missing");
        let path = missing_dir.join("events.jsonl");
        let writer = EventLogWriter::start(path.clone());

        // The parent directory does not exist yet: these events cannot be
        // written, but the writer must keep retrying instead of closing.
        assert!(writer.send(r#"{"index":"before-open"}"#.to_string()));
        let error = writer.flush().await.unwrap_err();
        assert!(error.to_string().contains("failed to open"), "{error}");

        tokio::fs::create_dir_all(&missing_dir).await.unwrap();
        assert!(writer.send(r#"{"index":"after-open"}"#.to_string()));
        writer.flush().await.unwrap();

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!contents.contains("before-open"), "{contents}");
        assert!(contents.contains("after-open"), "{contents}");

        drop(writer);
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    fn uuid_like() -> String {
        format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
}
