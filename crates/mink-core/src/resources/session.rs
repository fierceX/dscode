use crate::context::ToolContext;
use crate::resources::router::{Resource, ResourceHandler, ResourceRequest};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct SessionResourceHandler;

impl ResourceHandler for SessionResourceHandler {
    fn scheme(&self) -> &'static str {
        "session"
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource> {
        let content = read_session_resource(&req.resource_url, ctx)?;
        Ok(Resource {
            canonical_url: req.resource_url.clone(),
            content,
        })
    }
}

pub(crate) fn read_session_resource(url: &str, ctx: &ToolContext) -> Result<String> {
    let rest = url
        .strip_prefix("session://")
        .ok_or_else(|| anyhow!("Error: invalid session resource: {url}"))?
        .trim_end_matches('/');
    match rest {
        "current" => format_session_current(ctx),
        "current/stats" => format_session_stats(ctx),
        "current/messages" => format_session_messages(ctx, 40),
        "current/messages/all" => format_session_messages(ctx, usize::MAX),
        "current/history" => format_session_history(ctx),
        "current/artifacts" => format_session_artifacts(ctx),
        // 透明委托：与 TodoRead / Plan 工具同源读取（只读快照，无引导提示）
        "current/todo" => format_session_todo(ctx),
        "current/plan" => format_session_plan(&session_dir(ctx)?),
        _ => bail!("Error: unsupported session resource: {url}"),
    }
}

fn session_dir(ctx: &ToolContext) -> Result<PathBuf> {
    ctx.store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("Error: session path has no parent"))
}

fn format_session_current(ctx: &ToolContext) -> Result<String> {
    let dir = session_dir(ctx)?;
    let session_id = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let metadata = read_optional_file(&dir.join("session.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let alias = metadata
        .as_ref()
        .and_then(|value| value.get("alias"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = metadata
        .as_ref()
        .and_then(|value| value.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stats = read_optional_file(&dir.join("stats.json"))?;
    let stats_summary = serde_json::from_str::<crate::session::stats::Stats>(&stats)
        .map(format_stats_summary)
        .unwrap_or_else(|_| "stats: unavailable".to_string());
    let conversation_count = count_nonempty_lines(&dir.join("conversation.jsonl"));
    let artifact_count = count_nonempty_lines(&dir.join("artifacts/index.jsonl"));

    Ok(format!(
        "# session://current\n\
session_id: {session_id}\n\
alias: {alias}\n\
title: {title}\n\
cwd: {}\n\
home: {}\n\
session_dir: {}\n\
conversation_messages: {conversation_count}\n\
artifacts: {artifact_count}\n\
{stats_summary}\n\n\
Resources:\n\
- session://current/stats\n\
- session://current/messages\n\
- session://current/messages/all\n\
- session://current/history\n\
- session://current/artifacts\n\
- session://current/todo\n\
- session://current/plan\n",
        ctx.cwd.display(),
        ctx.home.display(),
        dir.display()
    ))
}

fn format_session_stats(ctx: &ToolContext) -> Result<String> {
    // stats.json 由编排器在每轮结束后 flush：轮内读取反映上一轮的
    // 快照（todo/plan 走内存 store、messages 走逐条 flush 的
    // conversation.jsonl，三者时点语义不同属已知设计）。
    let dir = session_dir(ctx)?;
    let raw = read_optional_file(&dir.join("stats.json"))?;
    if raw.trim().is_empty() {
        return Ok("{}".to_string());
    }
    let value: Value = serde_json::from_str(&raw)?;
    Ok(serde_json::to_string_pretty(&value)?)
}

fn format_session_messages(ctx: &ToolContext, keep_last: usize) -> Result<String> {
    let dir = session_dir(ctx)?;
    let raw = read_optional_file(&dir.join("conversation.jsonl"))?;
    let mut rows = Vec::new();
    for (idx, value) in parse_conversation_rows(&raw)? {
        rows.push(format!("{} {}", idx + 1, summarize_message(&value)));
    }

    let omitted = rows.len().saturating_sub(keep_last);
    let visible = if keep_last == usize::MAX || keep_last >= rows.len() {
        rows.as_slice()
    } else {
        &rows[omitted..]
    };

    let mut out = String::from("# session://current/messages\n");
    if omitted > 0 {
        out.push_str(&format!("... omitted {omitted} older messages\n"));
    }
    for row in visible {
        out.push_str(row);
        out.push('\n');
    }
    Ok(out)
}

fn format_session_history(ctx: &ToolContext) -> Result<String> {
    let dir = session_dir(ctx)?;
    let raw = read_optional_file(&dir.join("conversation.jsonl"))?;
    let messages = parse_conversation_rows(&raw)?
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();

    let mut results = HashMap::<String, String>::new();
    for message in &messages {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_result")
                && let Some(id) = block.get("tool_use_id").and_then(Value::as_str)
            {
                results.insert(
                    id.to_string(),
                    block
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                );
            }
        }
    }

    let mut consumed_results = HashSet::new();
    let mut out = String::from("# session://current/history\n\n");
    for message in &messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        match (role, message.get("content")) {
            ("user", Some(Value::String(text))) if !text.trim().is_empty() => {
                out.push_str("## user\n\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            ("assistant", Some(content)) => {
                let mut body = Vec::new();
                match content {
                    Value::String(text) if !text.trim().is_empty() => body.push(text.to_string()),
                    Value::Array(blocks) => {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    let text =
                                        block.get("text").and_then(Value::as_str).unwrap_or("");
                                    if !text.trim().is_empty() {
                                        body.push(text.to_string());
                                    }
                                }
                                Some("tool_use") => {
                                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                    let name =
                                        block.get("name").and_then(Value::as_str).unwrap_or("tool");
                                    let result = results.get(id).map(String::as_str);
                                    if result.is_some() {
                                        consumed_results.insert(id.to_string());
                                    }
                                    body.push(format_tool_exchange(
                                        name,
                                        block.get("input"),
                                        result,
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if !body.is_empty() {
                    out.push_str("## assistant\n\n");
                    out.push_str(&body.join("\n"));
                    out.push_str("\n\n");
                }
            }
            ("user", Some(Value::Array(blocks))) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let id = block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("<unknown>");
                            if !consumed_results.contains(id) {
                                let content =
                                    block.get("content").and_then(Value::as_str).unwrap_or("");
                                out.push_str(&format!(
                                    "## tool result\n\n{}\n\n",
                                    format_tool_result_status(id, content)
                                ));
                            }
                        }
                        Some("tool_attachment") => {
                            let url =
                                block.get("url").and_then(Value::as_str).unwrap_or("image://?");
                            out.push_str(&format!("## image\n\n{url}\n\n"));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn parse_conversation_rows(raw: &str) -> Result<Vec<(usize, Value)>> {
    let ends_with_newline = raw.ends_with('\n');
    let lines = raw.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(value) => rows.push((idx, value)),
            Err(_) if idx + 1 == lines.len() && !ends_with_newline => {}
            Err(error) => {
                bail!(
                    "Error: invalid conversation JSONL at line {}: {error}",
                    idx + 1
                )
            }
        }
    }
    Ok(rows)
}

fn format_tool_exchange(name: &str, input: Option<&Value>, result: Option<&str>) -> String {
    let arg = primary_tool_arg(input);
    let head = format!("-> {name}({arg})");
    match result {
        Some(content) => format!("{head} => {}", format_result_summary(content)),
        None => format!("{head} => pending"),
    }
}

fn format_tool_result_status(id: &str, content: &str) -> String {
    format!("-> {id} => {}", format_result_summary(content))
}

fn format_result_summary(content: &str) -> String {
    let line_count = if content.is_empty() {
        0
    } else {
        content.lines().count().max(1)
    };
    format!("{line_count} lines")
}

fn primary_tool_arg(input: Option<&Value>) -> String {
    const KEYS: &[&str] = &[
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "prompt",
        "description",
        "name",
        "id",
    ];
    let Some(input) = input.and_then(Value::as_object) else {
        return String::new();
    };
    for key in KEYS {
        if let Some(value) = input.get(*key).and_then(Value::as_str)
            && !value.is_empty()
        {
            return one_line(value, 120);
        }
    }
    one_line(&Value::Object(input.clone()).to_string(), 120)
}

fn one_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut out = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        out.pop();
        out.push_str("...");
    }
    out
}

fn format_session_artifacts(ctx: &ToolContext) -> Result<String> {
    let dir = session_dir(ctx)?;
    let raw = read_optional_file(&dir.join("artifacts/index.jsonl"))?;
    let mut out = String::from("# session://current/artifacts\n");
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| anyhow!("Error: invalid artifact index at line {}: {e}", idx + 1))?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let tool = value
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("<tool>");
        let bytes = value.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        let description = value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push_str(&format!(
            "- artifact://{id} {tool} {bytes} bytes {description}\n"
        ));
    }
    Ok(out)
}

fn format_session_todo(ctx: &ToolContext) -> Result<String> {
    // 与 TodoRead 完全同源：会话内 todo_store.snapshot() + render_snapshot（同一数据路径）
    let body = crate::tools::todo::render_snapshot(&ctx.todo_store.snapshot(), true);
    Ok(format!("# session://current/todo\n{body}"))
}

fn format_session_plan(dir: &Path) -> Result<String> {
    let confirmed = read_optional_file(&dir.join("plan.md"))?;
    let draft = read_optional_file(&dir.join("plan.draft"))?;
    let mut out = String::from("# session://current/plan\n");
    if !confirmed.trim().is_empty() {
        out.push_str("Status: confirmed\n\n");
        out.push_str(confirmed.trim());
    } else if !draft.trim().is_empty() {
        out.push_str("Status: draft\n\n");
        out.push_str(draft.trim());
    } else {
        out.push_str("Status: none");
    }
    Ok(out)
}

fn read_optional_file(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

fn count_nonempty_lines(path: &Path) -> usize {
    read_optional_file(path)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

fn format_stats_summary(stats: crate::session::stats::Stats) -> String {
    format!(
        "turns: {}\nrequests: agent={} compact={} sub_agent={}\ntokens: input={} output={} cache_read={} cache_create={} context={}",
        stats.current_turn_count,
        stats.agent_request_count,
        stats.compact_request_count,
        stats.sub_agent_request_count,
        stats.total_input_tokens,
        stats.total_output_tokens,
        stats.total_cache_read_tokens,
        stats.total_cache_creation_tokens,
        stats.current_context_tokens
    )
}

fn summarize_message(value: &Value) -> String {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let content = value.get("content").unwrap_or(&Value::Null);
    format!("{role}: {}", summarize_content(content))
}

fn summarize_content(content: &Value) -> String {
    match content {
        Value::String(text) => truncate_for_summary(text),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(kind) = item.get("type").and_then(Value::as_str) {
                    match kind {
                        "text" => {
                            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.trim().is_empty() {
                                parts.push(format!("text {:?}", truncate_for_summary(text)));
                            }
                        }
                        "thinking" => {
                            let len = item
                                .get("thinking")
                                .and_then(Value::as_str)
                                .map(str::len)
                                .unwrap_or(0);
                            if len > 0 {
                                parts.push(format!("thinking {len} bytes"));
                            }
                        }
                        "tool_use" => {
                            let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                            parts.push(format!("tool_use {name}"));
                        }
                        "tool_result" => {
                            let id = item
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("<id>");
                            let len = item
                                .get("content")
                                .and_then(Value::as_str)
                                .map(str::len)
                                .unwrap_or(0);
                            parts.push(format!("tool_result {id} {len} bytes"));
                        }
                        "tool_attachment" => {
                            let url =
                                item.get("url").and_then(Value::as_str).unwrap_or("?");
                            let width = item.get("width").and_then(Value::as_u64).unwrap_or(0);
                            let height = item.get("height").and_then(Value::as_u64).unwrap_or(0);
                            parts.push(format!("image {url} ({width}x{height})"));
                        }
                        other => parts.push(other.to_string()),
                    }
                }
            }
            if parts.is_empty() {
                "<empty>".to_string()
            } else {
                parts.join("; ")
            }
        }
        Value::Null => "<null>".to_string(),
        other => truncate_for_summary(&other.to_string()),
    }
}

fn truncate_for_summary(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in normalized.chars().take(160) {
        out.push(ch);
    }
    if normalized.chars().count() > 160 {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
