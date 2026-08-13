use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};

use super::bash;
use super::file;
use super::metadata::{ApprovalTier, ToolMetadata, ToolResultKind};
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
pub struct ToolRunResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_args: BTreeMap<String, String>,
    pub content: String,
    pub conv_content: String,
    pub spawns_sub_agent: bool,
    pub sub_agent_prompt: Option<String>,
    pub sub_agent_description: Option<String>,
    pub sub_agent_fork: bool,
    pub exit_code: Option<i32>,
    pub success: bool,
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

impl ToolRunResult {
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
}

enum PreparedCall {
    Execute(ToolCallEvent),
    Immediate(Box<ToolRunResult>),
}

struct ToolPolicyGate<'a> {
    surface: &'a crate::tools::surface::ModelToolSurface,
    storm: &'a Mutex<StormBreaker>,
}

struct RawToolResult {
    output: std::result::Result<String, anyhow::Error>,
    is_bash: bool,
    conv_content: String,
    exit_code: Option<i32>,
    wall_ms: Option<u128>,
    no_mutation: bool,
    memo_candidate: Option<MemoCandidate>,
    spawns_sub_agent: bool,
    success: bool,
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

    pub async fn execute_all(&self, calls: Vec<ToolCallEvent>) -> Result<Vec<ToolRunResult>> {
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

    pub fn finalize_deferred_results(&self, results: &mut [ToolRunResult]) {
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

    async fn execute_read_batch(&self, calls: Vec<ToolCallEvent>) -> Result<Vec<ToolRunResult>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let mut handles = Vec::new();
        for call in calls {
            match self.prepare_call(call) {
                PreparedCall::Immediate(result) => {
                    handles.push(tokio::spawn(async move { Ok(*result) }));
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

    async fn execute_prepared_call(&self, call: ToolCallEvent) -> Result<ToolRunResult> {
        match self.prepare_call(call) {
            PreparedCall::Immediate(result) => Ok(*result),
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

    fn prepare_call(&self, mut call: ToolCallEvent) -> PreparedCall {
        let tool_metadata = self.metadata_for(&call.name);
        let policy = ToolPolicyGate {
            surface: &self.ctx.tool_surface,
            storm: &self.storm,
        };
        if let Some(blocked) = policy.evaluate(&call, tool_metadata) {
            return PreparedCall::Immediate(Box::new(blocked));
        }

        repair_tool_input(&mut call);

        PreparedCall::Execute(call)
    }

    fn execute_one_sync(
        ctx: &ToolContext,
        call: &ToolCallEvent,
        tool_fn: Option<&dyn ToolExec>,
    ) -> Result<ToolRunResult> {
        let raw = dispatch_tool(ctx, call, tool_fn);
        Ok(format_dispatched_result(ctx, call, raw))
    }
}

async fn execute_custom(
    ctx: &ToolContext,
    call: &ToolCallEvent,
    tool: crate::runtime::RegisteredCustomTool,
) -> Result<ToolRunResult> {
    let definition = &tool.definition;
    let started = std::time::Instant::now();
    let result = tool
        .executor
        .execute(
            call.input_json.clone(),
            crate::runtime::ToolExecutionContext::new(ctx.cwd.clone(), ctx.interrupt.clone()),
        )
        .await;
    let raw = match result {
        Ok(output) => RawToolResult {
            output: Ok(output.content),
            is_bash: false,
            conv_content: output.conversation_content.unwrap_or_default(),
            exit_code: output.exit_code,
            wall_ms: Some(started.elapsed().as_millis()),
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            success: output.success,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            result_kind: definition.result_kind,
            presentation: None,
        },
        Err(error) => RawToolResult {
            output: Err(anyhow::anyhow!(error)),
            is_bash: false,
            conv_content: String::new(),
            exit_code: None,
            wall_ms: None,
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            success: false,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            result_kind: definition.result_kind,
            presentation: None,
        },
    };
    let formatted = format_dispatched_result(ctx, call, raw);
    if definition.mutating && formatted.success {
        ctx.bump_mutation();
    }
    Ok(formatted)
}

impl ToolPolicyGate<'_> {
    fn evaluate(
        &self,
        call: &ToolCallEvent,
        metadata: Option<ToolMetadata>,
    ) -> Option<ToolRunResult> {
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
            ));
        }
        if metadata.storm_exempt {
            return None;
        }
        let args_json = serde_json::to_string(&call.input_json).unwrap_or_default();
        let decision = {
            let mut storm = self.storm.lock().unwrap_or_else(|e| e.into_inner());
            storm.check(&call.name, &args_json, metadata.mutating)
        };
        match decision {
            StormDecision::Allow => None,
            StormDecision::Suppress(reason) => Some(blocked_tool_result(
                call.id.clone(),
                call.name.clone(),
                call.fields.clone(),
                reason,
            )),
        }
    }
}

fn repair_tool_input(call: &mut ToolCallEvent) {
    let args_str = serde_json::to_string(&call.input_json).unwrap_or_default();
    let result = crate::repair::repair_truncated_json(&args_str);
    if result.changed
        && !result.fallback
        && let Ok(repaired_val) = serde_json::from_str::<serde_json::Value>(&result.repaired)
    {
        call.input_json = repaired_val;
    }
}

fn dispatch_tool(
    ctx: &ToolContext,
    call: &ToolCallEvent,
    tool_fn: Option<&dyn ToolExec>,
) -> RawToolResult {
    if let Some(t) = tool_fn {
        let metadata = t.metadata();
        let started = std::time::Instant::now();
        match t.execute(&call.input_json, ctx) {
            Ok(outcome) => RawToolResult {
                output: Ok(outcome.content),
                is_bash: outcome.is_bash,
                conv_content: outcome.conversation_content,
                exit_code: outcome.exit_code,
                wall_ms: Some(started.elapsed().as_millis()),
                no_mutation: outcome.no_mutation,
                memo_candidate: outcome.memo_candidate,
                spawns_sub_agent: metadata.spawns_sub_agent,
                success: outcome.success,
                diagnostics: outcome.diagnostics,
                plan_command: outcome.plan_command,
                state_metadata: outcome.state_metadata,
                result_kind: metadata.result_kind,
                presentation: outcome.presentation,
            },
            Err(e) => RawToolResult {
                output: Err(e),
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
                success: false,
                diagnostics: Vec::new(),
                plan_command: None,
                state_metadata: None,
                result_kind: metadata.result_kind,
                presentation: None,
            },
        }
    } else {
        RawToolResult {
            output: Err(anyhow::anyhow!("unknown tool: {}", call.name)),
            is_bash: false,
            conv_content: String::new(),
            exit_code: None,
            wall_ms: None,
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            success: false,
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
    raw: RawToolResult,
) -> ToolRunResult {
    let RawToolResult {
        output,
        is_bash,
        mut conv_content,
        exit_code,
        wall_ms,
        no_mutation,
        memo_candidate,
        spawns_sub_agent,
        success,
        diagnostics,
        plan_command,
        state_metadata,
        result_kind,
        presentation,
    } = raw;
    let mut output = match output {
        Ok(v) => v,
        Err(e) => format!("Error: tool execution failed: {e}"),
    };
    if !success && exit_code.is_none() && !output.starts_with("Error:") {
        output = format!("Error: {output}");
    }
    if !diagnostics.is_empty() {
        output.push_str("\nDiagnostics:");
        for diagnostic in diagnostics {
            output.push('\n');
            output.push_str(&diagnostic);
        }
    }
    // P0-B: exec metadata header (Exit code / Wall time) for command tools.
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
    // A5: structured-file validity note for JSON/JSONL targets, so the model
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
    let sub_agent_description = if spawns_sub_agent && success {
        call.fields.get("description").cloned()
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

    ToolRunResult {
        tool_use_id: call.id.clone(),
        tool_name: call.name.clone(),
        tool_args: call.fields.clone(),
        content: final_output,
        conv_content,
        spawns_sub_agent,
        sub_agent_prompt,
        sub_agent_description,
        sub_agent_fork,
        exit_code,
        success,
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
        if let Some(stripped) = trimmed.strip_prefix('[') {
            if let Some(path) = stripped
                .split('#')
                .next()
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                paths.push(path.to_string());
            }
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

fn blocked_tool_result(
    id: String,
    name: String,
    args: BTreeMap<String, String>,
    reason: String,
) -> ToolRunResult {
    ToolRunResult {
        tool_use_id: id,
        tool_name: name,
        tool_args: args,
        content: format!("Error: {reason}"),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_description: None,
        sub_agent_fork: false,
        exit_code: None,
        success: false,
        result_kind: ToolResultKind::Text,
        presentation: None,
        artifacts: Vec::new(),
        signals: Vec::new(),
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
    }
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
        ToolMetadata::new(
            "SubAgent",
            "Spawn a child agent for isolated or forked work.",
            ApprovalTier::Exec,
            ToolResultKind::SubAgent,
        )
        .storm_exempt()
        .discoverable()
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
    Tokens(usize),
}

impl TruncationPolicy {
    /// Approximate byte budget. Tokens use a 4-bytes-per-token estimate so the
    /// policy composes with the rest of mink's explicit token budgets.
    fn byte_budget(&self) -> usize {
        match *self {
            Self::Bytes(bytes) => bytes,
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

fn filter_bash_noise(s: &str) -> String {
    // Strip ANSI escape sequences
    let ansi_re = regex::Regex::new("\x1b\\[[0-9;]*[a-zA-Z]").unwrap();
    let no_ansi = ansi_re.replace_all(s, "");

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
mod tests {
    use super::*;
    use crate::config::{ToolApprovalMode, ToolApprovalPolicy};
    use crate::context::ToolConfig;
    use crate::tools::approval::{ToolAuthorization, authorize_tool, denied_message};

    #[test]
    fn format_tool_result_truncates_large() {
        let s = "line0\n".repeat(500);
        let result = format_tool_result(&s, 100);
        assert!(result.len() <= 100 + 100); // head + tail + marker
        assert!(result.contains("truncated"));
    }

    #[test]
    fn format_tool_result_short_passes_through() {
        let s = "short";
        assert_eq!(format_tool_result(s, 100), "short");
    }

    #[test]
    fn filter_bash_noise_strips_ansi() {
        let input = "\x1b[32mgreen text\x1b[0m";
        let result = filter_bash_noise(input);
        assert!(!result.contains('\x1b'));
        assert!(result.contains("green text"));
    }

    #[test]
    fn filter_bash_noise_compresses_repeats() {
        let input = "line1\nline1\nline1\nline2";
        let result = filter_bash_noise(input);
        assert!(result.contains("repeated 2 times"));
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
    }

    #[test]
    fn tool_registry_matches_schema() {
        let schema: Vec<serde_json::Value> =
            serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
        let schema_names: std::collections::BTreeSet<String> = schema
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(serde_json::Value::as_str)
                    .expect("schema tool name")
                    .to_string()
            })
            .collect();
        let registry = tool_registry();
        let registry_names: std::collections::BTreeSet<String> = registry
            .iter()
            .map(|tool| tool.metadata().name.to_string())
            .collect();

        for name in &schema_names {
            if name == "PythonSandbox" && cfg!(not(feature = "python-sandbox")) {
                continue;
            }
            assert!(
                registry_names.contains(name),
                "schema tool missing executor: {name}"
            );
        }
        for tool in registry {
            assert!(
                schema_names.contains(tool.metadata().name.as_ref()),
                "registry tool missing schema: {}",
                tool.metadata().name
            );
        }
        for expected in [
            "PlanDraft",
            "PlanConfirm",
            "PlanClear",
            "TodoWrite",
            "TodoRead",
            "TodoAdvance",
            "SubAgent",
        ] {
            assert!(registry_names.contains(expected));
        }
    }

    #[test]
    fn tool_schema_order_is_stable_and_descriptions_are_self_contained() {
        let schema: Vec<serde_json::Value> =
            serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
        let names: Vec<&str> = schema
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(serde_json::Value::as_str)
                    .expect("schema tool name")
            })
            .collect();
        let pos = |name: &str| {
            names
                .iter()
                .position(|candidate| *candidate == name)
                .expect("tool should exist in schema")
        };

        assert!(pos("Glob") < pos("Bash"));
        assert!(pos("Grep") < pos("Bash"));

        for tool in &schema {
            let own_name = tool["name"].as_str().unwrap();
            let serialized = serde_json::to_string(tool).unwrap();
            for peer in &names {
                if *peer == own_name {
                    continue;
                }
                assert!(
                    !serialized.contains(&format!("`{peer}`"))
                        && !serialized.contains(&format!("use {peer}"))
                        && !serialized.contains(&format!("Use {peer}"))
                        && !serialized.contains(&format!("{peer} tool")),
                    "schema '{own_name}' contains peer-tool routing for '{peer}'"
                );
            }
        }

        let plan_draft = schema
            .iter()
            .find(|tool| tool["name"] == "PlanDraft")
            .expect("PlanDraft schema");
        assert!(
            plan_draft["description"]
                .as_str()
                .is_some_and(|description| description.contains("empty content string"))
        );
        assert!(
            plan_draft["input_schema"]["properties"]["content"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("empty string"))
        );
    }

    #[test]
    fn registry_metadata_is_complete() {
        for tool in tool_registry() {
            let meta = tool.metadata();
            assert!(
                !meta.summary.trim().is_empty(),
                "{} summary is empty",
                meta.name
            );
        }
    }

    #[test]
    fn mutating_tools_are_write_or_exec_tier() {
        for tool in tool_registry() {
            let meta = tool.metadata();
            if meta.mutating {
                assert!(
                    matches!(meta.approval, ApprovalTier::Write | ApprovalTier::Exec),
                    "{} is mutating but not write/exec tier",
                    meta.name
                );
            }
        }
    }

    #[test]
    fn expected_tool_metadata_contracts() {
        let meta = |name: &str| {
            tool_registry()
                .iter()
                .find(|tool| tool.metadata().name == name)
                .expect("tool should exist")
                .metadata()
        };

        assert_eq!(meta("Read").approval, ApprovalTier::Read);
        assert_eq!(meta("Read").result_kind, ToolResultKind::FileRead);
        assert_eq!(meta("Write").approval, ApprovalTier::Write);
        assert_eq!(meta("Write").result_kind, ToolResultKind::FileWrite);
        assert_eq!(meta("Edit").approval, ApprovalTier::Write);
        assert_eq!(meta("Edit").result_kind, ToolResultKind::Edit);
        assert_eq!(meta("Bash").approval, ApprovalTier::Exec);
        assert_eq!(meta("Bash").result_kind, ToolResultKind::Command);
        assert_eq!(meta("Glob").approval, ApprovalTier::Read);
        assert_eq!(meta("Glob").result_kind, ToolResultKind::Search);
        assert_eq!(meta("Grep").approval, ApprovalTier::Read);
        assert_eq!(meta("Grep").result_kind, ToolResultKind::Search);
        assert_eq!(meta("SubAgent").approval, ApprovalTier::Exec);
        assert_eq!(meta("SubAgent").result_kind, ToolResultKind::SubAgent);
        assert!(meta("SubAgent").spawns_sub_agent);
        assert!(meta("PlanDraft").internal);
        assert!(meta("PlanConfirm").internal);
        assert!(meta("PlanClear").internal);
        assert!(meta("Python").discoverable);
    }

    #[test]
    fn approval_yolo_allows_exec_tools() {
        let config = approval_test_config(ToolApprovalMode::Yolo, []);
        let bash = tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == "Bash")
            .unwrap()
            .metadata();
        assert_eq!(authorize_tool(&bash, &config), ToolAuthorization::Allowed);
    }

    #[test]
    fn approval_write_mode_blocks_exec_but_allows_write() {
        let config = approval_test_config(ToolApprovalMode::Write, []);
        let meta = |name: &str| {
            tool_registry()
                .iter()
                .find(|tool| tool.metadata().name == name)
                .unwrap()
                .metadata()
        };

        assert_eq!(
            authorize_tool(&meta("Read"), &config),
            ToolAuthorization::Allowed
        );
        assert_eq!(
            authorize_tool(&meta("Write"), &config),
            ToolAuthorization::Allowed
        );
        assert!(matches!(
            authorize_tool(&meta("Bash"), &config),
            ToolAuthorization::Denied { .. }
        ));
    }

    #[test]
    fn approval_per_tool_overrides_mode() {
        let config = approval_test_config(
            ToolApprovalMode::Write,
            [
                ("Bash".to_string(), ToolApprovalPolicy::Allow),
                ("Read".to_string(), ToolApprovalPolicy::Deny),
            ],
        );
        let meta = |name: &str| {
            tool_registry()
                .iter()
                .find(|tool| tool.metadata().name == name)
                .unwrap()
                .metadata()
        };

        assert_eq!(
            authorize_tool(&meta("Bash"), &config),
            ToolAuthorization::Allowed
        );
        let read = meta("Read");
        let reason = match authorize_tool(&read, &config) {
            ToolAuthorization::Denied { reason } => denied_message(&read, reason),
            ToolAuthorization::Allowed => panic!("Read should be denied"),
        };
        assert!(reason.contains("deny"), "{reason}");
    }

    #[test]
    fn policy_gate_blocks_tools_disabled_by_whitelist_before_execution() {
        let mut config = approval_test_config(ToolApprovalMode::Yolo, []);
        config.enabled_tools = Some(vec!["Read".into()]);
        let storm = Mutex::new(StormBreaker::new(6, 3));
        let resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
            crate::tools::surface::AgentRole::Primary,
            &config,
            false,
        );
        let surface = crate::tools::surface::ModelToolSurface::resolve(
            crate::tools::catalog::ToolCatalog::builtin().unwrap(),
            &config,
            &resolution,
        )
        .unwrap();
        let gate = ToolPolicyGate {
            surface: &surface,
            storm: &storm,
        };
        let call = test_call("Bash");
        let metadata = tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == "Bash")
            .map(|tool| tool.metadata());

        let blocked = gate
            .evaluate(&call, metadata)
            .expect("Bash should be blocked by enabled_tools");

        assert_eq!(blocked.tool_name, "Bash");
        assert!(blocked.content.contains("unavailable"));
    }

    #[test]
    fn policy_gate_blocks_tools_hidden_by_role_or_backend() {
        let config = approval_test_config(ToolApprovalMode::Yolo, []);
        let catalog = crate::tools::catalog::ToolCatalog::builtin().unwrap();
        let storm = Mutex::new(StormBreaker::new(6, 3));

        let sub_agent_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
            crate::tools::surface::AgentRole::SubAgent,
            &config,
            false,
        );
        let sub_agent_surface = crate::tools::surface::ModelToolSurface::resolve(
            catalog,
            &config,
            &sub_agent_resolution,
        )
        .unwrap();
        let sub_agent_gate = ToolPolicyGate {
            surface: &sub_agent_surface,
            storm: &storm,
        };
        let sub_agent_metadata = tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == "SubAgent")
            .map(|tool| tool.metadata());
        let blocked = sub_agent_gate
            .evaluate(&test_call("SubAgent"), sub_agent_metadata)
            .expect("SubAgent should be blocked outside the sub-agent surface");
        assert!(blocked.content.contains("UnavailableForRole"));

        let vfs_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
            crate::tools::surface::AgentRole::Primary,
            &config,
            true,
        );
        let vfs_surface =
            crate::tools::surface::ModelToolSurface::resolve(catalog, &config, &vfs_resolution)
                .unwrap();
        let vfs_gate = ToolPolicyGate {
            surface: &vfs_surface,
            storm: &storm,
        };
        let edit_metadata = tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == "Edit")
            .map(|tool| tool.metadata());
        let blocked = vfs_gate
            .evaluate(&test_call("Edit"), edit_metadata)
            .expect("Edit should be blocked outside the VFS surface");
        assert!(blocked.content.contains("UnavailableForBackend"));
    }

    fn test_call(name: &str) -> ToolCallEvent {
        ToolCallEvent {
            name: name.to_string(),
            id: "call_test".to_string(),
            input_json: serde_json::json!({}),
            fields: BTreeMap::new(),
            order: Vec::new(),
        }
    }

    fn approval_test_config<const N: usize>(
        mode: ToolApprovalMode,
        overrides: [(String, ToolApprovalPolicy); N],
    ) -> ToolConfig {
        ToolConfig {
            tool_timeout_secs: 600,
            sub_agent_timeout_secs: 300,
            tool_result_max_bytes: 100_000,
            file_write_max_bytes: 1_048_576,
            edit_mode: crate::config::EditMode::Hashline,
            edit_fuzzy_match: true,
            edit_fuzzy_threshold: 0.95,
            edit_enforce_seen_lines: false,
            max_search_files: 5000,
            max_search_results: 1000,
            enabled_tools: None,
            tool_approval_mode: mode,
            tool_approval: overrides.into_iter().collect(),
            sandbox_python: crate::config::SandboxPythonConfig::default(),
        }
    }

    #[test]
    fn all_tool_result_kind_variants_have_expected_coverage() {
        let kinds: std::collections::BTreeSet<&'static str> = tool_registry()
            .iter()
            .map(|tool| match tool.metadata().result_kind {
                ToolResultKind::Text => "Text",
                ToolResultKind::FileRead => "FileRead",
                ToolResultKind::FileWrite => "FileWrite",
                ToolResultKind::Edit => "Edit",
                ToolResultKind::Command => "Command",
                ToolResultKind::Search => "Search",
                ToolResultKind::Control => "Control",
                ToolResultKind::SubAgent => "SubAgent",
            })
            .collect();

        for expected in [
            "FileRead",
            "FileWrite",
            "Edit",
            "Command",
            "Search",
            "Control",
            "SubAgent",
        ] {
            assert!(kinds.contains(expected), "missing result kind {expected}");
        }
    }
}
