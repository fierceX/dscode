use crate::context::ToolContext;
use crate::resources::router::{
    Resource, ResourceContentType, ResourceHandler, ResourceMetadata, ResourceRequest,
};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
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
            content_type: ResourceContentType::PlainText,
            immutable: Some(true),
            metadata: ResourceMetadata::default(),
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
        "current/artifacts" => format_session_artifacts(ctx),
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
- session://current/artifacts\n",
        ctx.cwd.display(),
        ctx.home.display(),
        dir.display()
    ))
}

fn format_session_stats(ctx: &ToolContext) -> Result<String> {
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
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| anyhow!("Error: invalid conversation JSONL at line {}: {e}", idx + 1))?;
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
