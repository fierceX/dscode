use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};

use super::bash;
use super::file;
use super::metadata::{
    ApprovalTier, ToolBlocker, ToolFailureKind, ToolMetadata, ToolResultKind, ToolStatus,
};
use super::plan::{PlanClearTool, PlanCommand, PlanConfirmTool, PlanDraftTool};
use super::python;
#[cfg(feature = "python-sandbox")]
use super::sandbox_python;
use super::search;
use super::todo::{TodoAdvanceTool, TodoReadTool, TodoWriteTool};
use crate::context::ToolContext;
use crate::guard::storm::{StormBreaker, StormDecision};
use crate::protocol::ToolCallEvent;
use crate::ui::{ArtifactDisplay, ToolPresentation};

#[derive(Debug)]
/// A read whose memo entry should be recorded by the runner *after* the
/// complete composed output (including the summary line) is known to fit the
/// budget. The executor cannot know the final size, so it only offers the
/// candidate; the runner decides.
pub struct MemoCandidate {
    pub path: std::path::PathBuf,
    pub raw: bool,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

#[derive(Debug)]
pub struct ToolOutcome {
    pub content: String,
    pub conversation_content: String,
    pub is_bash: bool,
    pub exit_code: Option<i32>,
    pub success: bool,
    /// True when the call changed nothing on disk (e.g. an idempotent edit);
    /// used to skip mutation-epoch bumps so read memos stay valid.
    pub no_mutation: bool,
    /// Set by Read when a successful read may be memoized; the runner records
    /// it only when the final composed output fits `tool_result_max_bytes`.
    pub memo_candidate: Option<MemoCandidate>,
    pub diagnostics: Vec<String>,
    pub plan_command: Option<PlanCommand>,
    pub state_metadata: Option<serde_json::Value>,
    pub presentation: Option<ToolPresentation>,
}

impl ToolOutcome {
    pub fn text(content: String) -> Self {
        Self {
            content,
            conversation_content: String::new(),
            is_bash: false,
            exit_code: None,
            success: true,
            no_mutation: false,
            memo_candidate: None,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            presentation: None,
        }
    }

    pub fn plan(command: PlanCommand, content: &str) -> Self {
        let mut outcome = Self::text(content.to_string());
        outcome.plan_command = Some(command);
        outcome
    }
}

/// ToolExec defines the execution contract for a tool.
/// Each tool registers itself via `tool_registry()` and is dispatched
/// without a central match block.
pub trait ToolExec: Send + Sync {
    fn metadata(&self) -> ToolMetadata;

    fn execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> anyhow::Result<ToolOutcome>;
}

/// Built-in tool table, initialized once at first access.
static TOOL_REGISTRY: LazyLock<Vec<Box<dyn ToolExec>>> = LazyLock::new(|| {
    vec![
        Box::new(file::ReadTool),
        Box::new(file::WriteTool),
        Box::new(file::EditTool),
        Box::new(bash::BashTool),
        Box::new(search::GlobTool),
        Box::new(search::GrepTool),
        Box::new(TodoReadTool),
        Box::new(TodoWriteTool),
        Box::new(TodoAdvanceTool),
        Box::new(PlanDraftTool),
        Box::new(PlanConfirmTool),
        Box::new(PlanClearTool),
        Box::new(python::PythonTool),
        #[cfg(feature = "python-sandbox")]
        Box::new(sandbox_python::PythonSandboxTool),
        Box::new(SubAgentTool),
    ]
});

/// Return a reference to the global tool registry.
pub fn tool_registry() -> &'static [Box<dyn ToolExec>] {
    &TOOL_REGISTRY
}

// --- Runner ---

/// ToolRunner dispatches tool calls to their implementations.
pub struct ToolRunner {
    ctx: Arc<ToolContext>,
    storm: Mutex<StormBreaker>,
    tools: &'static [Box<dyn ToolExec>],
}

/// Result of executing a single tool call.
pub struct ToolExecution {
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_args: BTreeMap<String, String>,
    pub content: String,
    pub conv_content: String,
    pub spawns_sub_agent: bool,
    pub sub_agent_prompt: Option<String>,
    pub sub_agent_fork: bool,
    pub exit_code: Option<i32>,
    pub status: ToolStatus,
    pub result_kind: ToolResultKind,
    pub presentation: Option<ToolPresentation>,
    pub artifacts: Vec<ArtifactDisplay>,
    pub signals: Vec<crate::guard::collector::Signal>,
    pub(crate) plan_command: Option<PlanCommand>,
    pub(crate) needs_finalization: bool,
    pub(crate) state_metadata: Option<serde_json::Value>,
}

pub struct SubAgentRequest {
    pub prompt: String,
    pub fork: bool,
}

impl ToolExecution {
    pub fn succeeded(&self) -> bool {
        self.status.is_success()
    }

    pub(crate) fn take_sub_agent_request(&mut self) -> Option<SubAgentRequest> {
        if !self.spawns_sub_agent {
            return None;
        }
        let prompt = self.sub_agent_prompt.take()?;
        Some(SubAgentRequest {
            prompt,
            fork: self.sub_agent_fork,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_result(
        tool_use_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            tool_name: tool_name.into(),
            tool_args: BTreeMap::new(),
            content: content.into(),
            conv_content: String::new(),
            spawns_sub_agent: false,
            sub_agent_prompt: None,
            sub_agent_fork: false,
            exit_code: None,
            status: ToolStatus::Succeeded,
            result_kind: ToolResultKind::Text,
            presentation: None,
            artifacts: Vec::new(),
            signals: Vec::new(),
            plan_command: None,
            needs_finalization: false,
            state_metadata: None,
        }
    }
}

// Immediate results are short-lived by construction and already owned by the
// caller; boxing them only to make the enum smaller added an unnecessary
// allocation on the policy-block hot path.
#[allow(clippy::large_enum_variant)]
enum PreparedCall {
    Execute(ToolCallEvent),
    Immediate(ToolExecution),
}

struct ToolPolicyGate<'a> {
    surface: &'a crate::tools::surface::ModelToolSurface,
    storm: &'a Mutex<StormBreaker>,
}

struct ToolExecOutput {
    content: String,
    is_bash: bool,
    conv_content: String,
    exit_code: Option<i32>,
    wall_ms: Option<u128>,
    no_mutation: bool,
    memo_candidate: Option<MemoCandidate>,
    spawns_sub_agent: bool,
    status: ToolStatus,
    diagnostics: Vec<String>,
    plan_command: Option<PlanCommand>,
    state_metadata: Option<serde_json::Value>,
    result_kind: ToolResultKind,
    presentation: Option<ToolPresentation>,
}

impl ToolRunner {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self {
            ctx,
            storm: Mutex::new(StormBreaker::new(6, 3)),
            tools: tool_registry(),
        }
    }

    /// Find a tool by name.
    fn find_tool(&self, name: &str) -> Option<&dyn ToolExec> {
        self.tools
            .iter()
            .find(|t| t.metadata().name == name)
            .map(|t| t.as_ref())
    }

    fn find_custom_tool(&self, name: &str) -> Option<&crate::runtime::RegisteredCustomTool> {
        self.ctx
            .custom_tools
            .iter()
            .find(|tool| tool.definition.name == name)
    }

    fn metadata_for(&self, name: &str) -> Option<ToolMetadata> {
        if let Some(tool) = self.find_tool(name) {
            return Some(tool.metadata());
        }
        self.ctx
            .tool_surface
            .get(name)
            .map(|tool| tool.metadata.clone())
    }

    /// Reset storm breaker window — call at the start of each user turn.
    pub fn reset_storm(&self) {
        self.storm.lock().unwrap_or_else(|e| e.into_inner()).reset();
    }

    pub async fn execute_all(&self, calls: Vec<ToolCallEvent>) -> Result<Vec<ToolExecution>> {
        let mut results = Vec::new();
        let mut read_batch = Vec::new();

        for call in calls {
            let metadata = self.metadata_for(&call.name);
            let custom_sequential = self.find_custom_tool(&call.name).is_some_and(|tool| {
                tool.definition.execution == crate::runtime::ToolExecutionMode::Sequential
            });
            if custom_sequential || requires_sequential_execution(metadata) {
                results.extend(
                    self.execute_read_batch(std::mem::take(&mut read_batch))
                        .await?,
                );
                results.push(self.execute_prepared_call(call).await?);
            } else {
                read_batch.push(call);
            }
        }

        results.extend(self.execute_read_batch(read_batch).await?);
        Ok(results)
    }

    pub fn finalize_deferred_results(&self, results: &mut [ToolExecution]) {
        for result in results {
            if !result.needs_finalization {
                continue;
            }
            let formatted = format_tool_result_with_artifact(
                &result.tool_name,
                &result.content,
                self.ctx.tool_config.tool_result_max_bytes,
                &self.ctx,
            );
            result.content = formatted.content;
            result.artifacts.extend(formatted.artifacts);
            result.needs_finalization = false;
        }
    }

    async fn execute_read_batch(&self, calls: Vec<ToolCallEvent>) -> Result<Vec<ToolExecution>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let mut handles = Vec::new();
        for call in calls {
            match self.prepare_call(call) {
                PreparedCall::Immediate(result) => {
                    handles.push(tokio::spawn(async move { Ok(result) }));
                }
                PreparedCall::Execute(call) => {
                    if let Some(tool) = self.find_custom_tool(&call.name).cloned() {
                        let ctx = self.ctx.clone();
                        handles.push(tokio::spawn(async move {
                            execute_custom(&ctx, &call, tool).await
                        }));
                        continue;
                    }
                    let ctx = self.ctx.clone();
                    let tool_name = call.name.clone();
                    handles.push(tokio::spawn(async move {
                        tokio::task::spawn_blocking(move || {
                            let tool = tool_registry()
                                .iter()
                                .find(|t| t.metadata().name == tool_name);
                            Self::execute_one_sync(&ctx, &call, tool.map(|t| t.as_ref()))
                        })
                        .await?
                    }));
                }
            }
        }

        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await??);
        }
        Ok(results)
    }

    async fn execute_prepared_call(&self, call: ToolCallEvent) -> Result<ToolExecution> {
        match self.prepare_call(call) {
            PreparedCall::Immediate(result) => Ok(result),
            PreparedCall::Execute(call) => {
                if let Some(tool) = self.find_custom_tool(&call.name).cloned() {
                    return execute_custom(&self.ctx, &call, tool).await;
                }
                let ctx = self.ctx.clone();
                let tool_name = call.name.clone();
                tokio::task::spawn_blocking(move || {
                    let tool = tool_registry()
                        .iter()
                        .find(|t| t.metadata().name == tool_name);
                    Self::execute_one_sync(&ctx, &call, tool.map(|t| t.as_ref()))
                })
                .await?
            }
        }
    }

    fn prepare_call(&self, call: ToolCallEvent) -> PreparedCall {
        let tool_metadata = self.metadata_for(&call.name);
        let policy = ToolPolicyGate {
            surface: &self.ctx.tool_surface,
            storm: &self.storm,
        };
        if let Some(blocked) = policy.evaluate(&call, tool_metadata) {
            return PreparedCall::Immediate(blocked);
        }

        PreparedCall::Execute(call)
    }

    fn execute_one_sync(
        ctx: &ToolContext,
        call: &ToolCallEvent,
        tool_fn: Option<&dyn ToolExec>,
    ) -> Result<ToolExecution> {
        let raw = dispatch_tool(ctx, call, tool_fn);
        Ok(format_dispatched_result(ctx, call, raw))
    }
}

async fn execute_custom(
    ctx: &ToolContext,
    call: &ToolCallEvent,
    tool: crate::runtime::RegisteredCustomTool,
) -> Result<ToolExecution> {
    let definition = &tool.definition;
    let started = std::time::Instant::now();
    let timeout_secs = if ctx.tool_config.tool_timeout_secs > 0 {
        ctx.tool_config.tool_timeout_secs.clamp(5, 600) as u64
    } else {
        600
    };
    let execution = tool.executor.execute(
        call.input_json.clone(),
        crate::runtime::ToolExecutionContext::new(ctx.cwd.clone(), ctx.interrupt.clone()),
    );
    tokio::pin!(execution);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs));
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            result = &mut execution => break result,
            _ = &mut deadline => {
                break Err(crate::runtime::ToolError::new(format!(
                    "custom tool {} timed out after {timeout_secs}s",
                    definition.name
                )));
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                if ctx.interrupt.load(std::sync::atomic::Ordering::SeqCst) {
                    break Err(crate::runtime::ToolError::new(format!(
                        "custom tool {} interrupted",
                        definition.name
                    )));
                }
            }
        }
    };
    let raw = match result {
        Ok(output) => {
            let status = if output.success {
                ToolStatus::Succeeded
            } else {
                failed_status(&output.content, output.exit_code)
            };
            ToolExecOutput {
                content: output.content,
                is_bash: false,
                conv_content: output.conversation_content.unwrap_or_default(),
                exit_code: output.exit_code,
                wall_ms: Some(started.elapsed().as_millis()),
                no_mutation: false,
                memo_candidate: None,
                spawns_sub_agent: false,
                status,
                diagnostics: Vec::new(),
                plan_command: None,
                state_metadata: None,
                result_kind: definition.result_kind,
                presentation: None,
            }
        }
        Err(error) => {
            let detail = error.to_string();
            ToolExecOutput {
                content: format!("Error: tool execution failed: {detail}"),
                is_bash: false,
                conv_content: String::new(),
                exit_code: None,
                wall_ms: Some(started.elapsed().as_millis()),
                no_mutation: false,
                memo_candidate: None,
                spawns_sub_agent: false,
                status: failed_status(&detail, None),
                diagnostics: Vec::new(),
                plan_command: None,
                state_metadata: None,
                result_kind: definition.result_kind,
                presentation: None,
            }
        }
    };
    let formatted = format_dispatched_result(ctx, call, raw);
    if definition.mutating && formatted.succeeded() {
        ctx.bump_mutation();
    }
    Ok(formatted)
}

impl ToolPolicyGate<'_> {
    fn evaluate(
        &self,
        call: &ToolCallEvent,
        metadata: Option<ToolMetadata>,
    ) -> Option<ToolExecution> {
        let metadata = metadata?;
        if !self.surface.has(&call.name) {
            let reason = self
                .surface
                .hidden()
                .get(&call.name)
                .map(|reason| format!(" ({reason:?})"))
                .unwrap_or_default();
            return Some(blocked_tool_result(
                call.id.clone(),
                call.name.clone(),
                call.fields.clone(),
                format!(
                    "Tool '{}' is unavailable in the resolved model tool surface{reason}.",
                    call.name
                ),
                ToolBlocker::ToolSurface,
            ));
        }
        if metadata.storm_exempt {
            return None;
        }
        let args_json = serde_json::to_string(&call.input_json).unwrap_or_default();
        let decision = {
            let mut storm = self.storm.lock().unwrap_or_else(|e| e.into_inner());
            storm.check(&call.name, &args_json)
        };
        match decision {
            StormDecision::Allow => None,
            StormDecision::Suppress(reason) => Some(blocked_tool_result(
                call.id.clone(),
                call.name.clone(),
                call.fields.clone(),
                reason,
                ToolBlocker::StormBreaker,
            )),
        }
    }
}

fn failed_status(content: &str, exit_code: Option<i32>) -> ToolStatus {
    let kind = crate::tools::metadata::classify_failure_kind(content, exit_code);
    if kind == ToolFailureKind::Aborted && content.to_lowercase().contains("interrupt") {
        ToolStatus::Interrupted
    } else {
        ToolStatus::Failed(kind)
    }
}

fn dispatch_tool(
    ctx: &ToolContext,
    call: &ToolCallEvent,
    tool_fn: Option<&dyn ToolExec>,
) -> ToolExecOutput {
    if let Some(t) = tool_fn {
        let metadata = t.metadata();
        let started = std::time::Instant::now();
        match t.execute(&call.input_json, ctx) {
            Ok(outcome) => {
                let status = if outcome.success {
                    ToolStatus::Succeeded
                } else {
                    failed_status(&outcome.content, outcome.exit_code)
                };
                ToolExecOutput {
                    content: outcome.content,
                    is_bash: outcome.is_bash,
                    conv_content: outcome.conversation_content,
                    exit_code: outcome.exit_code,
                    wall_ms: Some(started.elapsed().as_millis()),
                    no_mutation: outcome.no_mutation,
                    memo_candidate: outcome.memo_candidate,
                    spawns_sub_agent: metadata.spawns_sub_agent,
                    status,
                    diagnostics: outcome.diagnostics,
                    plan_command: outcome.plan_command,
                    state_metadata: outcome.state_metadata,
                    result_kind: metadata.result_kind,
                    presentation: outcome.presentation,
                }
            }
            Err(e) => {
                let detail = e.to_string();
                ToolExecOutput {
                    content: format!("Error: tool execution failed: {detail}"),
                    is_bash: false,
                    conv_content: String::new(),
                    exit_code: None,
                    wall_ms: None,
                    no_mutation: false,
                    memo_candidate: None,
                    // A failed execute must never mark the call as spawning a
                    // sub-agent: the coordinator would launch a child with raw
                    // fields even though the executor rejected the input.
                    spawns_sub_agent: false,
                    status: failed_status(&detail, None),
                    diagnostics: Vec::new(),
                    plan_command: None,
                    state_metadata: None,
                    result_kind: metadata.result_kind,
                    presentation: None,
                }
            }
        }
    } else {
        ToolExecOutput {
            content: format!("Error: tool execution failed: unknown tool: {}", call.name),
            is_bash: false,
            conv_content: String::new(),
            exit_code: None,
            wall_ms: None,
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            status: ToolStatus::Failed(ToolFailureKind::Unknown),
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            result_kind: ToolResultKind::Text,
            presentation: None,
        }
    }
}

fn format_dispatched_result(
    ctx: &ToolContext,
    call: &ToolCallEvent,
    raw: ToolExecOutput,
) -> ToolExecution {
    let ToolExecOutput {
        content: mut output,
        is_bash,
        mut conv_content,
        exit_code,
        wall_ms,
        no_mutation,
        memo_candidate,
        spawns_sub_agent,
        status,
        diagnostics,
        plan_command,
        state_metadata,
        result_kind,
        presentation,
    } = raw;
    let success = status.is_success();
    if !success && exit_code.is_none() {
        output = format!("Error: {output}");
    }
    if !diagnostics.is_empty() {
        output.push_str("\nDiagnostics:");
        for diagnostic in diagnostics {
            output.push('\n');
            output.push_str(&diagnostic);
        }
    }
    if let Some(code) = exit_code {
        let mut header = format!("Exit code: {code}\n");
        if let Some(ms) = wall_ms {
            header.push_str(&format!("Wall time: {:.1}s\n", ms as f64 / 1000.0));
        }
        output = format!("{header}{output}");
    }
    // Compose the complete pre-truncation output first: exec header (already
    // in `output`), Read/Write summary line, and the JSON validity note. The
    // whole thing then goes through the unified formatter so truncation and
    // artifact spill can never be bypassed by later appends.
    let summary = if (call.name == "Read" || call.name == "Write") && !is_bash {
        let path_str = call.fields.get("path").map(|s| s.as_str()).unwrap_or("");
        let kind = call.name.as_str();
        let summary_path = resolve_summary_path(&ctx.cwd, path_str);
        Some(file_tool_result_summary_sync(
            kind,
            path_str,
            &summary_path.display().to_string(),
        ))
    } else {
        None
    };
    // notices immediately when an edit or write broke JSON syntax.
    let json_note = if success && (call.name == "Edit" || call.name == "Write") {
        json_validity_note(ctx, call)
    } else {
        None
    };
    let mut composed = output;
    if let Some(summary) = summary {
        composed = format!("{summary}\n{composed}");
    }
    if let Some(note) = json_note {
        composed.push_str(&note);
    }
    // Record the read memo only when the *final* composed output (content +
    // summary line + JSON note) fits the budget, so a later hit can never ask
    // the model to reuse content that was truncated or spilled.
    if let Some(candidate) = memo_candidate
        && composed.len() <= ctx.tool_config.tool_result_max_bytes
    {
        ctx.memo_record(
            &candidate.path,
            candidate.raw,
            candidate.start_line,
            candidate.end_line,
        );
    }
    let needs_finalization = plan_command.is_some() || spawns_sub_agent;
    let formatted = if needs_finalization {
        FormattedToolOutput {
            content: composed,
            artifacts: Vec::new(),
        }
    } else {
        format_tool_result_with_artifact(
            &call.name,
            &composed,
            ctx.tool_config.tool_result_max_bytes,
            ctx,
        )
    };

    let final_output = if is_bash {
        filter_bash_noise(&formatted.content)
    } else {
        formatted.content
    };

    // Edit diagnostics are continuation state, not a terminal-only summary:
    // the next model request needs new tags, changed lines, warnings and diffs.
    // Use the already size-protected/artifact-spilled output for both success
    // and failure so conversation and UI cannot diverge or bypass the limit.
    if call.name == "Edit" {
        conv_content = final_output.clone();
    }
    // A successful Write/Edit invalidates read memos: changed files must be
    // re-read before they are used again (engine-internal; no prompt impact).
    let mutating = crate::tools::runner::tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == call.name)
        .is_some_and(|tool| tool.metadata().mutating);
    if mutating && success && !no_mutation {
        ctx.bump_mutation();
    }

    // Signals collected later by TurnExecutor (needs shared SignalCollector for EditLoop)
    let signals = Vec::new();

    let sub_agent_prompt = if spawns_sub_agent && success {
        call.fields.get("prompt").cloned()
    } else {
        None
    };
    let sub_agent_fork = spawns_sub_agent
        && success
        && call
            .fields
            .get("fork")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);

    ToolExecution {
        tool_use_id: call.id.clone(),
        tool_name: call.name.clone(),
        tool_args: call.fields.clone(),
        content: final_output,
        conv_content,
        spawns_sub_agent,
        sub_agent_prompt,
        sub_agent_fork,
        exit_code,
        status,
        result_kind,
        presentation,
        artifacts: formatted.artifacts,
        signals,

        plan_command,
        needs_finalization,
        state_metadata,
    }
}
fn hashline_target_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim_start();
        if let Some(path) = crate::tools::hashline::section_header_path(line) {
            paths.push(path);
        } else if trimmed.starts_with("MV") && trimmed[2..].starts_with(char::is_whitespace) {
            // A moved file lands at the destination: validate the target so
            // `MV source.json -> destination.json` is still covered.
            let destination = trimmed[2..].trim();
            let destination = destination
                .strip_prefix(['\'', '"'])
                .and_then(|rest| rest.strip_suffix(['\'', '"']))
                .unwrap_or(destination);
            if !destination.is_empty() {
                paths.push(destination.to_string());
            }
        }
    }
    paths
}

/// Parse check for one JSON-ish file. `.jsonl` is validated line by line
/// (each line is an independent JSON value); `.json` is validated as a whole.
/// Returns Ok(()) or Err(line_number).
fn json_parse_check(path: &Path, content: &str) -> std::result::Result<(), usize> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".jsonl"))
    {
        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(line).is_err() {
                return Err(index + 1);
            }
        }
        Ok(())
    } else {
        match serde_json::from_str::<serde_json::Value>(content) {
            Ok(_) => Ok(()),
            Err(error) => Err(error.line()),
        }
    }
}

/// For a successful Edit/Write on `.json`/`.jsonl` targets, verify every
/// touched file parses and return short note lines (or None when not
/// applicable). Edit inputs may contain several `[path#TAG]` sections and MV
/// destinations; each JSON target is checked. The note is capped so a large
/// batch cannot push the result past the configured limit.
fn json_validity_note(ctx: &ToolContext, call: &ToolCallEvent) -> Option<String> {
    let paths: Vec<String> = if let Some(path) = call.fields.get("path") {
        vec![path.clone()]
    } else {
        call.fields
            .get("input")
            .map(|input| hashline_target_paths(input.as_str()))
            .unwrap_or_default()
    };
    let mut notes = Vec::new();
    for path_str in paths {
        if !(path_str.ends_with(".json") || path_str.ends_with(".jsonl")) {
            continue;
        }
        let path = resolve_summary_path(&ctx.cwd, &path_str);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        match json_parse_check(&path, &content) {
            Ok(()) => notes.push(format!("JSON parse: ok ({path_str})")),
            Err(line) => notes.push(format!("JSON parse failed at line {line} ({path_str})")),
        }
    }
    if notes.is_empty() {
        return None;
    }
    let joined = notes.join("\n");
    // Keep the note within the result budget: many-file batches collapse to
    // one summary line instead of pushing the output past the limit.
    let note = if joined.len() <= ctx.tool_config.tool_result_max_bytes / 4 {
        format!("\n{joined}")
    } else {
        let ok_count = notes
            .iter()
            .filter(|n| n.starts_with("JSON parse: ok"))
            .count();
        let failed_count = notes.len() - ok_count;
        format!("\nJSON parse: {ok_count} ok, {failed_count} failed")
    };
    Some(note)
}

fn requires_sequential_execution(metadata: Option<ToolMetadata>) -> bool {
    let Some(metadata) = metadata else {
        return false;
    };
    metadata.mutating
        || metadata.spawns_sub_agent
        || matches!(metadata.approval, ApprovalTier::Write | ApprovalTier::Exec)
        || matches!(
            metadata.result_kind,
            ToolResultKind::FileWrite
                | ToolResultKind::Edit
                | ToolResultKind::Command
                | ToolResultKind::Control
                | ToolResultKind::SubAgent
        )
}

pub(crate) fn blocked_tool_result(
    id: String,
    name: String,
    args: BTreeMap<String, String>,
    reason: String,
    blocker: ToolBlocker,
) -> ToolExecution {
    ToolExecution {
        tool_use_id: id,
        tool_name: name,
        tool_args: args,
        content: format!("Error: {reason}"),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_fork: false,
        exit_code: None,
        status: ToolStatus::Blocked(blocker),
        result_kind: ToolResultKind::Text,
        presentation: None,
        artifacts: Vec::new(),
        signals: Vec::new(),
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
    }
}

pub(crate) fn failed_tool_result(
    id: String,
    name: String,
    args: BTreeMap<String, String>,
    reason: String,
) -> ToolExecution {
    let mut result = blocked_tool_result(id, name, args, reason.clone(), ToolBlocker::ToolSurface);
    result.status =
        ToolStatus::Failed(crate::tools::metadata::classify_failure_kind(&reason, None));
    result
}

fn resolve_summary_path(cwd: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub struct SubAgentTool;

impl ToolExec for SubAgentTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new("SubAgent", ApprovalTier::Exec, ToolResultKind::SubAgent)
            .storm_exempt()
            .spawns_sub_agent()
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolOutcome> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            prompt: String,
            description: Option<String>,
            fork: Option<bool>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        if args.prompt.trim().is_empty() {
            bail!("Error: sub-agent prompt is required");
        }
        // Description/fork are declared in the schema; accept them so valid
        // calls are not rejected by the runtime contract.
        let _ = args.description;
        let _ = args.fork;
        Ok(ToolOutcome::text(String::new()))
    }
}

fn file_tool_result_summary_sync(kind: &str, display_path: &str, read_path: &str) -> String {
    if display_path.is_empty() {
        return kind.to_string();
    }
    // Use metadata to check file size without reading content.
    let meta = match std::fs::metadata(read_path) {
        Ok(m) => m,
        Err(_) => return format!("{}({})", kind, display_path),
    };
    // For large files, show size only — avoids re-reading the entire file just for a summary.
    if meta.len() > 1_048_576 {
        return format!("{}({}) [{} bytes]", kind, display_path, meta.len());
    }
    match std::fs::read(read_path) {
        Ok(data) => format!(
            "{}({}) [{} lines, {} bytes]",
            kind,
            display_path,
            line_count(&data),
            data.len()
        ),
        Err(_) => format!("{}({})", kind, display_path),
    }
}

fn line_count(s: &[u8]) -> usize {
    if s.is_empty() {
        0
    } else {
        s.iter().filter(|&&c| c == b'\n').count() + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationPolicy {
    Bytes(usize),
    #[cfg(test)]
    Tokens(usize),
}

impl TruncationPolicy {
    /// Approximate byte budget. Tokens use a 4-bytes-per-token estimate so the
    /// policy composes with the rest of mink's explicit token budgets.
    fn byte_budget(&self) -> usize {
        match *self {
            Self::Bytes(bytes) => bytes,
            #[cfg(test)]
            Self::Tokens(tokens) => tokens.saturating_mul(4),
        }
    }
}

/// Approximate token count (bytes / 4). Used only for truncation markers.
pub fn approx_token_count(s: &str) -> usize {
    s.len().div_ceil(4)
}

pub fn format_tool_result(s: &str, max: usize) -> String {
    format_tool_result_policy(s, TruncationPolicy::Bytes(max))
}

pub fn format_tool_result_policy(s: &str, policy: TruncationPolicy) -> String {
    let budget = policy.byte_budget();
    if s.len() <= budget {
        return s.to_string();
    }
    let size = s.len();
    let tokens = approx_token_count(s);
    let marker = format!(
        "\n\n[... truncated: original token count: {tokens} ({} bytes); showing first/last portions ...]\n\n",
        size
    );
    // The tail is byte-budgeted as well: a few short lines or a char-safe
    // suffix of a single over-long line, never the whole original output.
    let tail_text = tail_within_budget(s, budget / 4);
    let marker_len = marker.len() + 20;
    let tail_len = tail_text.len();
    let head_len = budget.saturating_sub(marker_len + tail_len);
    let mut head_text = utf8_prefix_by_bytes(s, head_len);
    // Cut at a complete line boundary so the model never sees a half line.
    if let Some(boundary) = head_text.rfind('\n') {
        head_text = &head_text[..boundary + 1];
    }
    format!("{head_text}{marker}{tail_text}")
}

/// Last whole lines of `s` within `max_bytes`; a single over-long line
/// degrades to a char-boundary byte suffix so the tail never exceeds the
/// budget and never duplicates the whole output.
fn tail_within_budget(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len();
    let mut used = 0usize;
    for (idx, ch) in s.char_indices().rev() {
        if ch == '\n' {
            let segment_len = start - (idx + 1);
            if used + segment_len > max_bytes {
                break;
            }
            used += segment_len;
            start = idx + 1;
        }
    }
    // Nothing fit (single line longer than the budget, or no newline):
    // fall back to a char-boundary byte suffix of the output.
    if s.len() - start > max_bytes || start == s.len() {
        let mut cut = s.len();
        let mut len = 0usize;
        for (idx, ch) in s.char_indices().rev() {
            if len + ch.len_utf8() > max_bytes {
                break;
            }
            len += ch.len_utf8();
            cut = idx;
        }
        start = cut;
    }
    &s[start..]
}

struct FormattedToolOutput {
    content: String,
    artifacts: Vec<ArtifactDisplay>,
}

fn format_tool_result_with_artifact(
    tool_name: &str,
    output: &str,
    max: usize,
    ctx: &ToolContext,
) -> FormattedToolOutput {
    if output.len() <= max {
        return FormattedToolOutput {
            content: output.to_string(),
            artifacts: Vec::new(),
        };
    }
    let truncated = format_tool_result(output, max);
    match ctx
        .artifacts
        .write_text(tool_name, "full tool output", output)
    {
        Ok(record) => FormattedToolOutput {
            content: format!("{truncated}\n\n[Full output: artifact://{}]", record.id),
            artifacts: vec![ArtifactDisplay {
                id: record.id,
                tool: record.tool,
                bytes: record.bytes,
                description: record.description,
            }],
        },
        Err(_) => FormattedToolOutput {
            content: truncated,
            artifacts: Vec::new(),
        },
    }
}

#[test]
fn format_tool_result_policy_token_mode_and_line_boundaries() {
    let s = "line0\n".repeat(500);
    let by_tokens = format_tool_result_policy(&s, TruncationPolicy::Tokens(25));
    assert!(by_tokens.len() <= 25 * 4 + 100);
    assert!(by_tokens.contains("original token count"));
    let head = by_tokens.split("[... truncated").next().unwrap();
    assert!(
        head.is_empty() || head.ends_with("\n\n"),
        "head must end at a line boundary: {head:?}"
    );
    let by_bytes = format_tool_result(&s, 100);
    assert!(by_bytes.contains("original token count"));
}

#[test]
fn truncation_single_overlong_line_stays_within_budget() {
    // 外部审查 #5：少于 5 个换行的输出此前会把整个原文当作 tail，
    // 单超长行导致结果超过预算。现在 head/tail 都受字节预算约束。
    let single_line = format!("{}\n", "x".repeat(10_000));
    let result = format_tool_result(&single_line, 100);
    assert!(
        result.len() <= 100 + 100,
        "truncated output {} bytes exceeds budget",
        result.len()
    );
    let no_newline = "y".repeat(8_000);
    let result = format_tool_result(&no_newline, 100);
    assert!(result.len() <= 100 + 100, "{} bytes", result.len());
}
fn utf8_prefix_by_bytes(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let next = i + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &s[..end]
}

static RE_ANSI_ESCAPE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new("\x1b\\[[0-9;]*[a-zA-Z]").expect("valid ANSI regex")
});

fn filter_bash_noise(s: &str) -> String {
    // Strip ANSI escape sequences
    let no_ansi = RE_ANSI_ESCAPE.replace_all(s, "");

    // Compress consecutive identical lines
    let lines: Vec<&str> = no_ansi.lines().collect();
    let mut out = Vec::new();
    let mut repeat_count: usize = 0;
    for i in 0..lines.len() {
        if i > 0 && lines[i] == lines[i - 1] {
            repeat_count += 1;
        } else {
            if repeat_count > 0 {
                out.push(format!("  [previous line repeated {} times]", repeat_count));
                repeat_count = 0;
            }
            out.push(lines[i].to_string());
        }
    }
    if repeat_count > 0 {
        out.push(format!("  [previous line repeated {} times]", repeat_count));
    }
    out.join("\n")
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
