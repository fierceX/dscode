//! Child-process supervision shared by execution tools.

use std::io::Read;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PROCESS_OUTPUT_CAPTURE_LIMIT: usize = 1_000_000;

/// Shared argument parsing for the host Python and CPython WASI sandbox tools.
/// Both tools accept the same `script` / `script_file` / `timeout` surface;
/// keeping the dispatch here prevents their validation and path resolution
/// rules from drifting apart.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PythonScriptArgs {
    pub script: Option<String>,
    #[serde(default)]
    pub script_file: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

impl PythonScriptArgs {
    pub(crate) fn parse(input: &serde_json::Value) -> anyhow::Result<Self> {
        serde_json::from_value(input.clone()).map_err(Into::into)
    }

    /// Resolve the script body from the mutually exclusive arguments.
    pub(crate) fn resolve_script(&self, cwd: &std::path::Path) -> anyhow::Result<String> {
        match (&self.script, &self.script_file) {
            (Some(script), None) => Ok(script.clone()),
            (None, Some(path)) => {
                let full_path = if std::path::Path::new(path).is_absolute() {
                    std::path::PathBuf::from(path)
                } else {
                    cwd.join(path)
                };
                std::fs::read_to_string(&full_path).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to read script file {}: {error}",
                        full_path.display()
                    )
                })
            }
            (Some(_), Some(_)) => {
                anyhow::bail!("Error: provide either 'script' or 'script_file', not both");
            }
            (None, None) => {
                anyhow::bail!("Error: provide either 'script' or 'script_file'");
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ProcessOutputBuffer {
    inner: Arc<Mutex<ProcessOutputBufferInner>>,
}

#[derive(Default)]
struct ProcessOutputBufferInner {
    bytes: Vec<u8>,
    truncated: bool,
}

impl ProcessOutputBuffer {
    pub(crate) fn append(&self, data: &[u8]) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let remaining = PROCESS_OUTPUT_CAPTURE_LIMIT.saturating_sub(guard.bytes.len());
        if data.len() > remaining {
            guard.bytes.extend_from_slice(&data[..remaining]);
            guard.truncated = true;
        } else {
            guard.bytes.extend_from_slice(data);
        }
    }

    pub(crate) fn to_string_lossy(&self, stream_name: &str) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = String::from_utf8_lossy(&guard.bytes).to_string();
        if guard.truncated {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!(
                "[... truncated {stream_name} after {PROCESS_OUTPUT_CAPTURE_LIMIT} bytes ...]"
            ));
        }
        out
    }
}

pub(crate) fn spawn_output_reader<R>(mut pipe: R, buffer: ProcessOutputBuffer) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buffer.append(&chunk[..n]),
            }
        }
    })
}

/// Join output readers with a bounded grace period. A background grandchild
/// that inherited the stdout/stderr pipe can keep `read()` blocked after the
/// child exits; joining without a deadline would hang the tool call and leak
/// a blocking-pool thread. Blocked readers are detached (dropping the
/// JoinHandle detaches the thread): they exit on their own when the pipe
/// finally closes (grandchild exit). The rare case of a daemon that never
/// exits leaks one plain OS thread; accepted tradeoff vs. hanging the turn.
pub(crate) fn join_output_readers_bounded(readers: Vec<JoinHandle<()>>) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while readers.iter().any(|r| !r.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Outcome of waiting for a child process with timeout/interrupt enforcement.
pub(crate) struct ChildCompletion {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub interrupted: bool,
}

/// Wait for the child to exit (killing the process tree on timeout or
/// interrupt), then join output readers with a bounded grace period.
/// Shared by the Bash and Python tools so their wait semantics stay
/// identical; the caller supplies a label for the wait-error message.
pub(crate) fn wait_child_with_output(
    child: &mut Child,
    readers: Vec<JoinHandle<()>>,
    timeout: Duration,
    interrupt: Option<&std::sync::atomic::AtomicBool>,
    wait_error_label: &str,
) -> anyhow::Result<ChildCompletion> {
    let start = Instant::now();
    let mut timed_out = false;
    let mut interrupted = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if interrupt.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
                    terminate_child_process_tree(child);
                    interrupted = true;
                    break Some(130);
                }
                if start.elapsed() >= timeout {
                    terminate_child_process_tree(child);
                    timed_out = true;
                    break Some(124);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(anyhow::anyhow!("Error: {wait_error_label}: {e}")),
        }
    };
    join_output_readers_bounded(readers);
    Ok(ChildCompletion {
        exit_code,
        timed_out,
        interrupted,
    })
}

/// Put spawned Unix children in their own process group so timeout/cancel can
/// clean up grandchildren that keep pipes or files open.
#[cfg(unix)]
pub(crate) fn configure_child_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub(crate) fn configure_child_process_group(_cmd: &mut Command) {}

pub(crate) fn terminate_child_process_tree(child: &mut Child) {
    terminate_child_process_tree_with_grace(child, Duration::from_millis(250));
}

pub(crate) fn terminate_child_process_tree_with_grace(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        let pgid = -(child.id() as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        let start = Instant::now();
        while start.elapsed() < grace {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
        let _ = child.wait();
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}
