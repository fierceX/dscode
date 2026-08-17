use anyhow::{Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Execute a Python script and return (stdout, stderr, exit_code).
/// Test-only convenience wrapper; the tool executes through
/// [`execute_script_with_interrupt_in_dir`].
#[cfg(test)]
pub fn execute_script(
    script: &str,
    timeout_secs: Option<u64>,
) -> Result<(String, String, Option<i32>)> {
    execute_script_with_interrupt(script, timeout_secs, None)
}

#[cfg(test)]
pub fn execute_script_with_interrupt(
    script: &str,
    timeout_secs: Option<u64>,
    interrupt: Option<&AtomicBool>,
) -> Result<(String, String, Option<i32>)> {
    execute_script_with_interrupt_in_dir(script, timeout_secs, interrupt, None)
}

fn execute_script_with_interrupt_in_dir(
    script: &str,
    timeout_secs: Option<u64>,
    interrupt: Option<&AtomicBool>,
    cwd: Option<&Path>,
) -> Result<(String, String, Option<i32>)> {
    if script.trim().is_empty() {
        bail!("Error: no Python script provided");
    }

    // 安全策略已交给 OS 进程沙箱处理

    let timeout = Duration::from_secs(timeout_secs.unwrap_or(30).clamp(5, 300));

    let mut cmd = Command::new("python3");
    cmd.arg("-B") // don't write .pyc
        .arg("-W") // warning control
        .arg("ignore") // suppress warnings
        .arg("-c")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    crate::tools::process::configure_child_process_group(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to start python3: {e}"))?;

    let stdout_buf = crate::tools::process::ProcessOutputBuffer::default();
    let stderr_buf = crate::tools::process::ProcessOutputBuffer::default();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(crate::tools::process::spawn_output_reader(
            stdout,
            stdout_buf.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(crate::tools::process::spawn_output_reader(
            stderr,
            stderr_buf.clone(),
        ));
    }

    // Read output with timeout
    let completion = crate::tools::process::wait_child_with_output(
        &mut child,
        readers,
        timeout,
        interrupt,
        "failed to wait for python3",
    )?;
    let mut stdout = stdout_buf.to_string_lossy("stdout");
    let stderr = stderr_buf.to_string_lossy("stderr");

    if completion.timed_out {
        if !stdout.is_empty() {
            stdout.push('\n');
        }
        stdout.push_str(&format!(
            "[... truncated, Python script timed out after {} seconds ...]",
            timeout.as_secs()
        ));
    }
    if completion.interrupted {
        if !stdout.is_empty() {
            stdout.push('\n');
        }
        stdout.push_str("[... Python script interrupted ...]");
    }

    Ok((stdout, stderr, completion.exit_code))
}

pub struct PythonTool;

impl super::runner::ToolExec for PythonTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Python",
            "Execute restricted Python code.",
            super::metadata::ApprovalTier::Exec,
            super::metadata::ToolResultKind::Command,
        )
        .storm_exempt()
        .discoverable()
        // host Python 可执行任意文件写代码：成功后必须 bump mutation
        // epoch，使 read memo 失效（与 Bash/PythonSandbox 一致）。
        .mutating()
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            script: Option<String>,
            #[serde(default)]
            script_file: Option<String>,
            #[serde(default)]
            timeout: Option<u64>,
        }

        let args: Args = serde_json::from_value(input.clone())?;

        let script = match (args.script, args.script_file) {
            (Some(s), None) => s,
            (None, Some(path)) => {
                let full_path = if std::path::Path::new(&path).is_absolute() {
                    path
                } else {
                    format!("{}/{}", ctx.cwd.display(), path)
                };
                std::fs::read_to_string(&full_path)
                    .map_err(|e| anyhow::anyhow!("Failed to read script file {full_path}: {e}"))?
            }
            (Some(_), Some(_)) => {
                bail!("Error: provide either 'script' or 'script_file', not both");
            }
            (None, None) => {
                bail!("Error: provide either 'script' or 'script_file'");
            }
        };

        let (stdout, stderr, exit_code) = execute_script_with_interrupt_in_dir(
            &script,
            args.timeout,
            Some(ctx.interrupt.as_ref()),
            Some(&ctx.cwd),
        )?;

        let mut content = stdout;
        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&stderr);
        }
        if let Some(code) = exit_code
            && code != 0
        {
            content.push_str(&format!("\n\nPython script exited with code {code}."));
        }

        Ok(super::runner::ToolOutcome {
            content,
            conversation_content: String::new(),
            is_bash: false,
            exit_code,
            success: exit_code == Some(0),
            no_mutation: false,
            memo_candidate: None,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            presentation: None,
        })
    }
}

#[cfg(test)]
#[path = "python_tests.rs"]
mod tests;
