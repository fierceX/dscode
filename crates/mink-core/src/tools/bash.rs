use crate::safety;
use anyhow::{Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

static RE_BASH_READ_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(cat|head|tail|less|more)\b").expect("regex"));
static RE_BASH_SEARCH_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(rg|grep|ag|ack)\b").expect("regex"));
static RE_BASH_FIND_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*(find|fd|ls|tree)\b").expect("regex"));
static RE_BASH_RG_FILES_MISUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^\s*rg\s+--files\b").expect("regex"));

#[cfg(test)]
pub fn execute(
    command: &str,
    timeout_secs: Option<u64>,
    default_timeout: i32,
) -> Result<(String, Option<i32>)> {
    execute_with_interrupt(command, timeout_secs, default_timeout, None)
}

#[cfg(test)]
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
    crate::tools::process::configure_child_process_group(&mut cmd);
    let mut child = cmd.spawn()?;

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

    let completion = crate::tools::process::wait_child_with_output(
        &mut child,
        readers,
        timeout,
        interrupt,
        "failed to wait on child process",
    )?;

    let mut out = stdout_buf.to_string_lossy("stdout");
    let stderr_out = stderr_buf.to_string_lossy("stderr");
    if !stderr_out.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&stderr_out);
    }

    if completion.timed_out {
        out.push_str(&format!(
            "\n[... truncated, command timed out after {} seconds ...]",
            timeout.as_secs()
        ));
    } else if completion.exit_code == Some(130) {
        out.push_str("\n[... command interrupted ...]");
    }

    Ok(SyncOutput {
        stdout: out.into_bytes(),
        stderr: Vec::new(),
        code: completion.exit_code,
    })
}

struct SyncOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
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
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            command: String,
            #[serde(default)]
            timeout: Option<u64>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        if let Some(guidance) =
            bash_misuse_guidance(&args.command, &ctx.tool_surface, &ctx.tool_capabilities)
        {
            guidance.validate(&ctx.tool_surface)?;
            bail!("Error: {}", guidance.content);
        }
        Self::execute_with_context(&args.command, args.timeout, ctx)
    }
}

fn bash_misuse_guidance(
    command: &str,
    surface: &crate::tools::surface::ModelToolSurface,
    capabilities: &crate::tools::semantic_capabilities::ResolvedToolCapabilities,
) -> Option<crate::tools::runtime_guidance::RenderedRuntimeGuidance> {
    use crate::tools::semantic_capabilities::ToolSemanticCapability;
    let capability = bash_misuse_capability(command)?;
    let binding = capabilities.binding(capability)?;
    if binding.primary.tool == "Bash"
        || !binding
            .alternatives
            .iter()
            .any(|provider| provider.tool == "Bash")
    {
        return None;
    }
    let primary = &binding.primary.tool;
    let purpose = match capability {
        ToolSemanticCapability::PathRead => "file reading",
        ToolSemanticCapability::ContentSearch => "content search",
        ToolSemanticCapability::PathDiscovery => "file discovery",
        _ => return None,
    };
    let guidance = crate::tools::runtime_guidance::RenderedRuntimeGuidance {
        content: format!(
            "Bash command looks like {purpose}. Use {primary}, the active specialized provider, instead."
        ),
        referenced_tools: [primary.clone()].into_iter().collect(),
    };
    guidance.validate(surface).ok()?;
    Some(guidance)
}

fn bash_misuse_capability(
    command: &str,
) -> Option<crate::tools::semantic_capabilities::ToolSemanticCapability> {
    use crate::tools::semantic_capabilities::ToolSemanticCapability;
    let trimmed = command.trim();
    if RE_BASH_READ_MISUSE.is_match(trimmed) {
        return Some(ToolSemanticCapability::PathRead);
    }
    if RE_BASH_RG_FILES_MISUSE.is_match(trimmed) || RE_BASH_FIND_MISUSE.is_match(trimmed) {
        return Some(ToolSemanticCapability::PathDiscovery);
    }
    if RE_BASH_SEARCH_MISUSE.is_match(trimmed) {
        return Some(ToolSemanticCapability::ContentSearch);
    }
    None
}

pub(crate) fn is_focused_verification_command(command: &str) -> bool {
    let trimmed = command.trim();
    if crate::safety::deny_bash_command_reason(trimmed).is_some()
        || [";", "&&", "||", "\n", ">", "<", "`", "$(", "&"]
            .iter()
            .any(|operator| trimmed.contains(operator))
    {
        return false;
    }
    let words: Vec<&str> = trimmed.split_ascii_whitespace().collect();
    match words.as_slice() {
        ["cargo", action, ..] => matches!(*action, "build" | "check" | "test" | "clippy" | "fmt"),
        ["git", action, ..] => matches!(*action, "status" | "diff" | "show" | "log"),
        ["make", action, ..] => {
            let action = action.to_ascii_lowercase();
            action.contains("test")
                || action.contains("check")
                || action.contains("build")
                || action.contains("lint")
        }
        ["npm" | "pnpm" | "yarn", "test", ..] => true,
        ["npm" | "pnpm" | "yarn", "run", action, ..] => {
            matches!(*action, "test" | "check" | "build" | "lint")
        }
        ["go", "test", ..] => true,
        ["pytest" | "ruff", ..] => true,
        _ => false,
    }
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
            success: code == Some(0),
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
#[path = "bash_tests.rs"]
mod tests;
