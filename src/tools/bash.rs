use crate::safety;
use anyhow::{Result, anyhow, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

static ADAPTIVE_HISTORY: LazyLock<Mutex<VecDeque<Duration>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static RE_BASH_READ_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(cat|head|tail|less|more)\b").expect("regex"));
static RE_BASH_SEARCH_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(rg|grep|ag|ack)\b").expect("regex"));
static RE_BASH_FIND_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(find|fd|ls|tree)\b").expect("regex"));
static RE_BASH_RG_FILES_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*rg\s+--files\b").expect("regex"));

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

    let start = Instant::now();

    // Use tokio async path if running inside a tokio runtime, otherwise sync fallback.
    let (output_bytes, stderr_bytes, exit_code): (Vec<u8>, Vec<u8>, Option<i32>) =
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let result = handle.block_on(async {
                    let child = tokio::process::Command::new("bash")
                        .arg("-lc")
                        .arg(command)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()?;

                    let output_fut = child.wait_with_output();
                    tokio::pin!(output_fut);

                    let mut interrupt_check = tokio::time::interval(Duration::from_millis(100));

                    let output = tokio::time::timeout(timeout, async {
                        loop {
                            tokio::select! {
                                result = &mut output_fut => {
                                    return result.map_err(|e| anyhow!("process error: {e}"));
                                }
                                _ = interrupt_check.tick() => {
                                    if let Some(flag) = interrupt
                                        && flag.load(Ordering::SeqCst)
                                    {
                                        return Err(anyhow!("interrupted"));
                                    }
                                }
                            }
                        }
                    })
                    .await;

                    match output {
                        Ok(Ok(output)) => Ok((output.stdout, output.stderr, output.status.code())),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(anyhow!("timed out")),
                    }
                });

                match result {
                    Ok(ok) => ok,
                    Err(e) => {
                        let msg = e.to_string();
                        if msg == "timed out" {
                            return Ok((
                                format!(
                                    "[... truncated, command timed out after {} seconds ...]",
                                    timeout.as_secs()
                                ),
                                None,
                            ));
                        } else if msg == "interrupted" {
                            return Ok(("[... command interrupted ...]".to_string(), Some(130)));
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
            Err(_) => {
                let sync = execute_sync_fallback(command, timeout, interrupt)?;
                (sync.stdout, sync.stderr, sync.code)
            }
        };

    let elapsed = start.elapsed();
    record_execution_time(elapsed);

    let mut out = String::from_utf8_lossy(&output_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&stderr);
    }
    if let Some(code) = exit_code
        && code != 0
    {
        out.push_str(&format!("\n\nProcess completed with exit code {}.", code));
    }
    Ok((out, exit_code))
}

/// Fallback synchronous execution when no tokio runtime is available (e.g. in tests).
fn execute_sync_fallback(
    command: &str,
    timeout: Duration,
    interrupt: Option<&AtomicBool>,
) -> Result<SyncOutput> {
    let mut child = std::process::Command::new("bash")
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

    let start_sync = Instant::now();
    let mut timed_out = false;
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
                    exit_code = Some(130);
                    break;
                }
                if start_sync.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => bail!("Error: failed to wait on child process: {e}"),
        }
    }

    // Allow reader threads to drain pipes before we lock the buffers.
    std::thread::sleep(Duration::from_millis(30));

    let mut out = stdout_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_out = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !stderr_out.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&stderr_out);
    }

    if timed_out {
        out.push_str(&format!(
            "\n[... truncated, command timed out after {} seconds ...]",
            timeout.as_secs()
        ));
    } else if exit_code == Some(130) {
        out.push_str("\n[... command interrupted ...]");
    }

    Ok(SyncOutput {
        stdout: out.into_bytes(),
        stderr: Vec::new(),
        code: exit_code,
    })
}

struct SyncOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
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
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Bash",
            "Execute a shell command.",
            super::metadata::ApprovalTier::Exec,
            super::metadata::ToolResultKind::Command,
        )
        .mutating()
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
        if let Some(reason) = bash_misuse_reason(&args.command) {
            bail!("Error: {reason}");
        }
        Self::execute_with_context(&args.command, args.timeout, ctx)
    }
}

fn bash_misuse_reason(command: &str) -> Option<&'static str> {
    let trimmed = command.trim();
    if RE_BASH_READ_MISUSE.is_match(trimmed) {
        return Some(
            "Bash command looks like file reading. Use Read with an optional line selector instead.",
        );
    }
    if RE_BASH_RG_FILES_MISUSE.is_match(trimmed) || RE_BASH_FIND_MISUSE.is_match(trimmed) {
        return Some("Bash command looks like file discovery. Use Glob instead.");
    }
    if RE_BASH_SEARCH_MISUSE.is_match(trimmed) {
        return Some("Bash command looks like content search. Use Grep instead.");
    }
    None
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
    fn misuse_detector_routes_file_reading_to_read() {
        let reason = bash_misuse_reason("cat src/main.rs").unwrap();
        assert!(reason.contains("Read"));
    }

    #[test]
    fn misuse_detector_routes_search_to_grep() {
        let reason = bash_misuse_reason("rg TODO src").unwrap();
        assert!(reason.contains("Grep"));
    }

    #[test]
    fn misuse_detector_routes_discovery_to_glob() {
        let reason = bash_misuse_reason("find . -name '*.rs'").unwrap();
        assert!(reason.contains("Glob"));
        let reason = bash_misuse_reason("rg --files").unwrap();
        assert!(reason.contains("Glob"));
    }

    #[test]
    fn misuse_detector_allows_build_commands() {
        assert!(bash_misuse_reason("cargo test").is_none());
        assert!(bash_misuse_reason("git status --short").is_none());
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
        ADAPTIVE_HISTORY.lock().unwrap().clear();
        let t = adaptive_timeout(Duration::from_secs(30));
        assert_eq!(t, Duration::from_secs(30));
    }

    #[test]
    fn adaptive_timeout_respects_default_when_no_history() {
        ADAPTIVE_HISTORY.lock().unwrap().clear();
        let t = adaptive_timeout(Duration::from_secs(10));
        assert_eq!(t, Duration::from_secs(10));
    }
}
