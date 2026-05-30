use crate::safety;
use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

static ADAPTIVE_HISTORY: LazyLock<Mutex<VecDeque<Duration>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Compute adaptive timeout: median of last 10 executions × 3, min 5s, max 600s.
pub fn adaptive_timeout(default_timeout: Duration) -> Duration {
    let history = ADAPTIVE_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if history.len() < 5 {
        return default_timeout.min(Duration::from_secs(600));
    }
    let mut sorted: Vec<Duration> = history.iter().copied().collect();
    sorted.sort();
    let median = sorted[sorted.len() / 2];
    let timeout = median * 3;
    timeout
        .max(Duration::from_secs(5))
        .min(Duration::from_secs(600))
}

fn record_execution_time(elapsed: Duration) {
    let mut history = ADAPTIVE_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    history.push_front(elapsed);
    history.truncate(10);
}

pub fn execute(
    command: &str,
    timeout_secs: Option<u64>,
    default_timeout: i32,
) -> Result<(String, Option<i32>)> {
    execute_with_interrupt(command, timeout_secs, default_timeout, None)
}

pub fn execute_with_interrupt(
    command: &str,
    timeout_secs: Option<u64>,
    default_timeout: i32,
    interrupt: Option<&AtomicBool>,
) -> Result<(String, Option<i32>)> {
    if command.trim().is_empty() {
        bail!("Error: no command provided");
    }
    if let Some(reason) = safety::deny_bash_command_reason(command) {
        bail!("Error: command blocked by bash safety policy ({reason})");
    }

    let timeout = match timeout_secs {
        Some(t) if t > 0 => Duration::from_secs(t),
        _ => adaptive_timeout(Duration::from_secs(if default_timeout > 0 {
            default_timeout as u64
        } else {
            600
        })),
    };

    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let stderr_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    if let Some(stdout) = child.stdout.take() {
        let buf = stdout_buf.clone();
        std::thread::spawn(move || stream_reader(stdout, buf));
    }
    if let Some(stderr) = child.stderr.take() {
        let buf = stderr_buf.clone();
        std::thread::spawn(move || stream_reader(stderr, buf));
    }

    let start = Instant::now();
    let mut timed_out = false;
    let mut interrupted = false;
    let mut exit_code: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if interrupt.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                    let _ = child.kill();
                    let _ = child.wait();
                    interrupted = true;
                    exit_code = Some(130);
                    break;
                }
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => bail!("Error: failed to wait on child process: {e}"),
        }
    }

    std::thread::sleep(Duration::from_millis(30));
    let elapsed = start.elapsed();
    record_execution_time(elapsed);

    let mut out = stdout_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&stderr);
    }
    if timed_out {
        out.push_str(&format!(
            "\n[... truncated, command timed out after {} seconds ...]",
            timeout.as_secs()
        ));
    }
    if interrupted {
        out.push_str("\n[... command interrupted ...]");
    }
    if let Some(code) = exit_code
        && code != 0
    {
        out.push_str(&format!("\n\nProcess completed with exit code {}.", code));
    }
    Ok((out, exit_code))
}

fn stream_reader<R: std::io::Read>(mut pipe: R, buf: Arc<Mutex<String>>) {
    let mut data = Vec::new();
    let _ = pipe.read_to_end(&mut data);
    if let Ok(mut guard) = buf.lock() {
        guard.push_str(&String::from_utf8_lossy(&data));
    }
}

pub struct BashTool;

impl super::runner::ToolExec for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }
    fn mutating(&self) -> bool {
        true
    }
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        if ctx.tool_config.tool_disable.disable_bash {
            return Ok(super::runner::ToolOutcome::text(
                "Error: Bash tool is disabled by configuration.".into(),
            ));
        }
        #[derive(serde::Deserialize)]
        struct Args {
            command: String,
            #[serde(default)]
            timeout: Option<u64>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        Self::execute_with_context(&args.command, args.timeout, ctx)
    }
}

impl BashTool {
    fn execute_with_context(
        command: &str,
        timeout: Option<u64>,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        execute_with_interrupt(
            command,
            timeout,
            ctx.tool_config.tool_timeout_secs,
            Some(ctx.interrupt.as_ref()),
        )
        .map(|(s, code)| super::runner::ToolOutcome {
            content: s,
            conversation_content: String::new(),
            is_bash: true,
            exit_code: code,
            success: code.unwrap_or(0) == 0,
            diagnostics: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_command_error() {
        assert!(execute("", None, 600).is_err());
        assert!(execute("   ", None, 600).is_err());
    }

    #[test]
    fn blocked_command_error() {
        assert!(execute("sudo rm /tmp/foo", None, 600).is_err());
    }

    #[test]
    fn simple_echo_works() {
        let (result, _) = execute("echo hello", None, 600).unwrap();
        assert!(result.contains("hello"));
    }

    #[test]
    fn timeout_kills_long_command() {
        let (result, _) = execute("sleep 10; echo done", Some(1), 600).unwrap();
        assert!(result.contains("timed out"));
        assert!(!result.contains("done"));
    }

    #[test]
    fn interrupt_kills_long_command() {
        let interrupt = AtomicBool::new(true);
        let (result, code) =
            execute_with_interrupt("sleep 10; echo done", Some(30), 600, Some(&interrupt)).unwrap();
        assert_eq!(code, Some(130));
        assert!(result.contains("interrupted"));
        assert!(!result.contains("done"));
    }

    #[test]
    fn adaptive_timeout_with_few_samples_returns_default() {
        let t = adaptive_timeout(Duration::from_secs(30));
        assert_eq!(t, Duration::from_secs(30));
    }

    #[test]
    fn adaptive_timeout_respects_default_when_no_history() {
        let t = adaptive_timeout(Duration::from_secs(10));
        assert_eq!(t, Duration::from_secs(10));
    }
}
