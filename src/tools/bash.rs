use crate::safety;
use anyhow::{Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static RE_BASH_READ_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(cat|head|tail|less|more)\b").expect("regex"));
static RE_BASH_SEARCH_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(rg|grep|ag|ack)\b").expect("regex"));
static RE_BASH_FIND_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(find|fd|ls|tree)\b").expect("regex"));
static RE_BASH_RG_FILES_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*rg\s+--files\b").expect("regex"));

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
    execute_with_interrupt_in_dir(command, timeout_secs, default_timeout, interrupt, None)
}

fn execute_with_interrupt_in_dir(
    command: &str,
    timeout_secs: Option<u64>,
    default_timeout: i32,
    interrupt: Option<&AtomicBool>,
    cwd: Option<&Path>,
) -> Result<(String, Option<i32>)> {
    if command.trim().is_empty() {
        bail!("Error: no command provided");
    }
    if let Some(reason) = safety::deny_bash_command_reason(command) {
        bail!("Error: command blocked by bash safety policy ({reason})");
    }

    let timeout = Duration::from_secs(match timeout_secs {
        Some(t) if t > 0 => t,
        _ if default_timeout > 0 => (default_timeout as u64).clamp(5, 600),
        _ => 600,
    });

    let sync = execute_sync(command, timeout, interrupt, cwd)?;
    let (output_bytes, stderr_bytes, exit_code) = (sync.stdout, sync.stderr, sync.code);

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

fn execute_sync(
    command: &str,
    timeout: Duration,
    interrupt: Option<&AtomicBool>,
    cwd: Option<&Path>,
) -> Result<SyncOutput> {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-lc")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    crate::util::configure_child_process_group(&mut cmd);
    let mut child = cmd.spawn()?;

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
                    crate::util::terminate_child_process_tree(&mut child);
                    exit_code = Some(130);
                    break;
                }
                if start_sync.elapsed() >= timeout {
                    crate::util::terminate_child_process_tree(&mut child);
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
        execute_with_interrupt_in_dir(
            command,
            timeout,
            ctx.tool_config.tool_timeout_secs,
            Some(ctx.interrupt.as_ref()),
            Some(&ctx.cwd),
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{nanos}", std::process::id()))
    }

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

    #[tokio::test]
    async fn execute_works_inside_tokio_runtime() {
        let (result, code) = execute_with_interrupt("echo async-ok", None, 600, None).unwrap();
        assert_eq!(code, Some(0));
        assert!(result.contains("async-ok"));
    }

    #[test]
    fn execute_in_dir_uses_requested_cwd() {
        let dir = temp_dir("mink-bash-cwd");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("marker.txt"), "ok").unwrap();

        let (result, code) = execute_with_interrupt_in_dir(
            "test -f marker.txt && echo found",
            None,
            600,
            None,
            Some(&dir),
        )
        .unwrap();

        assert_eq!(code, Some(0));
        assert!(result.contains("found"));
        fs::remove_dir_all(&dir).ok();
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
    fn default_timeout_is_stable_without_execution_history() {
        let (result, _) = execute("sleep 1; echo done", None, 5).unwrap();
        assert!(result.contains("done"));
    }
}
