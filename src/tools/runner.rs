use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::file;
use super::bash;
use super::search;
use super::web;
use super::{is_tool_mutating, is_storm_exempt};

use crate::guard::storm::{StormBreaker, StormDecision};
use crate::context::ToolContext;
use crate::protocol::ToolCallEvent;

/// ToolExec defines the execution contract for a tool.
/// Each tool registers itself via `tool_table()` and is dispatched
/// without a central match block.
pub trait ToolExec: Send + Sync {
    fn name(&self) -> &'static str;

    /// Execute the tool. Returns (output_text, is_bash, conv_content).
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<(String, bool, String)>;
}

/// Built-in tool table.
fn tool_table() -> Vec<Box<dyn ToolExec>> {
    vec![
        Box::new(file::ReadTool),
        Box::new(file::WriteTool),
        Box::new(file::EditTool),
        Box::new(bash::BashTool),
        Box::new(search::GlobTool),
        Box::new(search::GrepTool),
        Box::new(web::WebSearchTool),
        Box::new(web::WebFetchTool),
    ]
}

// --- Runner ---

/// ToolRunner dispatches tool calls to their implementations.
pub struct ToolRunner {
    ctx: Arc<ToolContext>,
    storm: Mutex<StormBreaker>,
    tools: Vec<Box<dyn ToolExec>>,
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
    pub signals: Vec<crate::guard::collector::Signal>,
}

#[derive(serde::Deserialize)]
struct TodoArg {
    content: String,
    status: String,
}

impl ToolRunner {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self {
            ctx,
            storm: Mutex::new(StormBreaker::new(6, 3)),
            tools: tool_table(),
        }
    }

    /// Find a tool by name.
    fn find_tool(&self, name: &str) -> Option<&dyn ToolExec> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    /// Reset storm breaker window — call at the start of each user turn.
    pub fn reset_storm(&self) {
        self.storm.lock().unwrap_or_else(|e| e.into_inner()).reset();
    }

    pub async fn execute_all(
        &self,
        calls: Vec<ToolCallEvent>,
    ) -> Result<Vec<ToolRunResult>> {
        let mut handles = Vec::new();
        for call in calls {
            // Storm check: suppress repeated identical calls
            if !is_storm_exempt(&call.name) && self.find_tool(&call.name).is_some() {
                let args_json = serde_json::to_string(&call.input_json).unwrap_or_default();
                let decision = {
                    let mut storm = self.storm.lock().unwrap_or_else(|e| e.into_inner());
                    storm.check(&call.name, &args_json, is_tool_mutating(&call.name))
                };
                if let StormDecision::Suppress(reason) = decision {
                    let name = call.name.clone();
                    let id = call.id.clone();
                    let args = call.fields.clone();
                    handles.push(tokio::task::spawn_blocking(move || {
                        Ok(ToolRunResult {
                            tool_use_id: id,
                            tool_name: name,
                            tool_args: args,
                            content: format!("Error: {reason}"),
                            conv_content: String::new(),
                            spawns_sub_agent: false,
                            sub_agent_prompt: None,
                            sub_agent_description: None,
                            sub_agent_fork: false,
                            signals: Vec::new(),
                        })
                    }));
                    continue;
                }
            }

            let ctx = self.ctx.clone();
            // Pre-execution: repair truncated argument JSON
            let mut call = call;
            {
                let args_str = serde_json::to_string(&call.input_json).unwrap_or_default();
                let result = crate::repair::repair_truncated_json(&args_str);
                if result.changed && !result.fallback
                    && let Ok(repaired_val) = serde_json::from_str::<serde_json::Value>(&result.repaired) {
                        call.input_json = repaired_val;
                    }
            }
            // Pass tool name for lookup inside spawn_blocking
            let tool_name = call.name.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let tool = tool_table().into_iter().find(|t| t.name() == tool_name);
                Self::execute_one_sync(&ctx, &call, tool.as_deref())
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await??);
        }
        Ok(results)
    }

    fn execute_one_sync(
        ctx: &ToolContext,
        call: &ToolCallEvent,
        tool_fn: Option<&dyn ToolExec>,
    ) -> Result<ToolRunResult> {
        let start = std::time::Instant::now();
        // Dispatch via ToolExec if available, otherwise handle built-in tools
        let (output, is_bash, mut conv_content) = if let Some(t) = tool_fn {
            let result = t.execute(&call.input_json, ctx);
            match result {
                Ok((text, is_b, conv)) => (Ok(text), is_b, conv),
                Err(e) => (Err(e), false, String::new()),
            }
        } else {
            match call.name.as_str() {
                "TodoWrite" => {
                    #[derive(serde::Deserialize)]
                    struct Args { todos: Vec<TodoArg> }
                    let args: Args = serde_json::from_value(call.input_json.clone())?;
                    (todo_write_tool(&args.todos), false, String::new())
                }
                "Skill" => {
                    #[derive(serde::Deserialize)]
                    struct Args { name: String }
                    let args: Args = serde_json::from_value(call.input_json.clone())?;
                    (skill_tool(ctx, &args.name), false, String::new())
                }
                "SubAgent" | "PlanConfirm" | "PlanClear" => {
                    // Handled in TurnExecutor layer
                    (Ok(String::new()), false, String::new())
                }
                _ => (Err(anyhow::anyhow!("unknown tool: {}", call.name)), false, String::new()),
            }
        };

        let output = match output {
            Ok(v) => v,
            Err(e) => format!("Error: tool execution failed: {e}"),
        };

        let output = format_tool_result(&output, ctx.tool_result_max_bytes);

        let output = if is_bash {
            filter_bash_noise(&output)
        } else {
            output
        };

        if call.name == "Edit" && conv_content.is_empty() {
            conv_content = crate::session::store::first_line(&output).to_string();
        }

        let final_output = if (call.name == "Read" || call.name == "Write") && !is_bash {
            let path_str = call.fields.get("path").map(|s| s.as_str()).unwrap_or("");
            let kind = call.name.as_str();
            let summary = file_tool_result_summary_sync(kind, path_str);
            format!("{}\n{}", summary, output)
        } else {
            output
        };

        // Collect signals (error detection, slow execution, large output)
        use crate::guard::collector::SignalCollector;
        let signals = {
            let elapsed = start.elapsed().as_millis() as u64;
            let bytes = final_output.len();
            let collector = SignalCollector::new();
            collector.collect(&call.name, elapsed, bytes, &final_output)
        };

        let spawns_sub_agent = call.name == "SubAgent";
        let sub_agent_prompt = if spawns_sub_agent { call.fields.get("prompt").cloned() } else { None };
        let sub_agent_description = if spawns_sub_agent { call.fields.get("description").cloned() } else { None };
        let sub_agent_fork = spawns_sub_agent
            && call.fields.get("fork").map(|s| s == "true" || s == "1").unwrap_or(false);

        Ok(ToolRunResult {
            tool_use_id: call.id.clone(),
            tool_name: call.name.clone(),
            tool_args: call.fields.clone(),
            content: final_output,
            conv_content,
            spawns_sub_agent,
            sub_agent_prompt,
            sub_agent_description,
            sub_agent_fork,
            signals,
        })
    }
}

fn todo_write_tool(todos: &[TodoArg]) -> Result<String> {
    let mut lines = Vec::new();
    let mut in_progress = 0;
    for t in todos {
        if t.content.is_empty() { bail!("Error: todo item content is required"); }
        match t.status.as_str() {
            "pending" => lines.push(format!("- [ ] {}", t.content)),
            "in_progress" => { in_progress += 1; lines.push(format!("- [ ] {}", t.content)); }
            "completed" => lines.push(format!("- [x] {}", t.content)),
            _ => bail!("Error: invalid todo status: {}", t.status),
        }
    }
    if in_progress > 1 { bail!("Error: todo_write allows at most one in_progress item"); }
    Ok(lines.join("\n"))
}

fn skill_tool(ctx: &ToolContext, name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() { bail!("Error: no skill name provided"); }

    // Check embedded skills first (built into binary)
    if let Some(skill) = crate::assets::embedded_skills::find(name) {
        let expanded = skill.content.replace("${DSCODE_SKILL_DIR}", "<built-in>");
        return Ok(format!("Skill: {}\nBase directory: <built-in>\n\n{}", skill.name, expanded));
    }

    // Fallback to file system
    let Some(skill_file) = crate::prompt::resolve_skill_file(&ctx.cwd, &ctx.home, name) else {
        bail!("Error: skill not found: {name}");
    };
    let base_dir = skill_file.parent().unwrap_or(std::path::Path::new(""));
    let content = std::fs::read_to_string(&skill_file)?
        .replace("${DSCODE_SKILL_DIR}", &base_dir.display().to_string());
    Ok(format!("Skill: {name}\nBase directory: {}\n\n{content}", base_dir.display()))
}

fn file_tool_result_summary_sync(kind: &str, path: &str) -> String {
    if path.is_empty() { return kind.to_string(); }
    match std::fs::read(path) {
        Ok(data) => format!("{}({}) [{} lines, {} bytes]", kind, path, line_count(&data), data.len()),
        Err(_) => format!("{}({})", kind, path),
    }
}

fn line_count(s: &[u8]) -> usize {
    if s.is_empty() { 0 } else { s.iter().filter(|&&c| c == b'\n').count() + 1 }
}

pub fn format_tool_result(s: &str, max: usize) -> String {
    if s.len() <= max { return s.to_string(); }
    let size = s.len();
    let marker = format!("\n\n[... truncated: showing first/last portions of {} bytes ...]\n\n", size);
    let tail_lines = 5;
    let tail_text = last_n_lines(s, tail_lines);
    let marker_len = marker.len() + 20;
    let tail_len = tail_text.len();
    let mut head_len = max.saturating_sub(marker_len + tail_len);
    if head_len == 0 { head_len = max / 2; }
    let head_text = utf8_prefix_by_bytes(s, head_len);
    format!("{head_text}{marker}{tail_text}")
}

fn last_n_lines(s: &str, n: usize) -> &str {
    let mut count = 0;
    for (i, ch) in s.char_indices().rev() {
        if ch == '\n' { count += 1; if count >= n { return &s[i + ch.len_utf8()..]; } }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_write_outputs_checklist() {
        let todos = vec![
            TodoArg { content: "task 1".into(), status: "pending".into() },
            TodoArg { content: "task 2".into(), status: "in_progress".into() },
            TodoArg { content: "task 3".into(), status: "completed".into() },
        ];
        let result = todo_write_tool(&todos).unwrap();
        assert!(result.contains("- [ ] task 1"));
        assert!(result.contains("- [ ] task 2"));
        assert!(result.contains("- [x] task 3"));
    }

    #[test]
    fn todo_write_rejects_multiple_in_progress() {
        let todos = vec![
            TodoArg { content: "a".into(), status: "in_progress".into() },
            TodoArg { content: "b".into(), status: "in_progress".into() },
        ];
        assert!(todo_write_tool(&todos).is_err());
    }

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
}

fn utf8_prefix_by_bytes(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() { return s; }
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let next = i + ch.len_utf8();
        if next > max_bytes { break; }
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
