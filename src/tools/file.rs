use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// Threshold for switching to streaming read (bytes).
const STREAM_READ_THRESHOLD: u64 = 1_048_576; // 1MB
const EDIT_REREAD_CONTEXT_LINES: usize = 12;

fn ensure_full_read_within_limit(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: usize,
) -> Result<()> {
    if offset.is_some() || limit.is_some() {
        return Ok(());
    }
    let meta = std::fs::metadata(path)
        .map_err(|_| anyhow!("Error: file not found or unreadable: {}", path.display()))?;
    if meta.len() as u128 > max_bytes as u128 {
        bail!(
            "Error: file too large for full Read ({} bytes > {} bytes): {}. Use a line selector such as '{}:1-200' or pass offset/limit.",
            meta.len(),
            max_bytes,
            path.display(),
            path.display()
        );
    }
    Ok(())
}

pub fn read(path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    if path.is_empty() {
        bail!("Error: no path provided");
    }

    // Fast path: small file or full read — use read_to_string directly.
    let meta = std::fs::metadata(path)
        .map_err(|_| anyhow!("Error: file not found or unreadable: {path}"))?;

    if meta.len() < STREAM_READ_THRESHOLD || (offset.is_none() && limit.is_none()) {
        // Small file: existing fast path
        let data = std::fs::read_to_string(path)
            .map_err(|_| anyhow!("Error: file not found or unreadable: {path}"))?;
        if offset.is_none() && limit.is_none() {
            return Ok(data);
        }
        // Range on a small file: same logic as before
        let mut lines: Vec<&str> = data.split('\n').collect();
        if !lines.is_empty() && lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        let range = selected_line_range(lines.len(), offset, limit, path)?;
        return Ok(lines[range].join("\n"));
    }

    // Large file + range: stream — scan line boundaries, then read exact byte range.
    let start_line = offset.unwrap_or(1).max(1);
    let count = limit.filter(|count| *count > 0);

    let mut reader = BufReader::new(std::fs::File::open(path)?);
    // line_offsets[i] = byte offset where line (i+1) starts in the file.
    let mut line_offsets: Vec<u64> = Vec::with_capacity(4096);

    let target_line_count = count.and_then(|count| start_line.checked_sub(1)?.checked_add(count));
    let mut buf = Vec::new();

    loop {
        let pos_before = reader.stream_position()?;
        buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut buf)?;
        if bytes_read == 0 {
            break; // EOF
        }
        line_offsets.push(pos_before);
        if let Some(target) = target_line_count
            && line_offsets.len() > target
        {
            break; // we have the end offset for the requested range
        }
    }

    let total_lines = line_offsets.len();
    let range = selected_line_range(total_lines, Some(start_line), count, path)?;
    if range.is_empty() {
        return Ok(String::new());
    }
    let start_byte = line_offsets[range.start];

    // End byte: either the start of the next line after the range, or EOF.
    let end_byte = if range.end < line_offsets.len() {
        line_offsets[range.end]
    } else {
        // Scan to end of file to find the exact byte position
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        reader.stream_position()?
    };

    if start_byte >= end_byte {
        return Ok(String::new());
    }

    // Seek and read the exact byte range.
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start_byte))?;
    let mut content = vec![0u8; (end_byte - start_byte) as usize];
    file.read_exact(&mut content)?;

    let mut selected = String::from_utf8(content)?;
    if selected.ends_with('\n') {
        selected.pop();
        if selected.ends_with('\r') {
            selected.pop();
        }
    }
    Ok(selected)
}

fn selected_line_range(
    total_lines: usize,
    offset: Option<usize>,
    limit: Option<usize>,
    path: &str,
) -> Result<Range<usize>> {
    let start_line = offset.unwrap_or(1).max(1);
    if start_line > total_lines {
        if total_lines == 0 && start_line == 1 {
            return Ok(0..0);
        }
        bail!(
            "Error: offset {} exceeds total lines {} in {}",
            start_line,
            total_lines,
            path
        );
    }
    let start = start_line - 1;
    let end = match limit {
        Some(count) if count > 0 => start.saturating_add(count).min(total_lines),
        _ => total_lines,
    };
    Ok(start..end)
}

pub fn write(path: &str, content: &str, max_bytes: usize) -> Result<String> {
    if path.is_empty() {
        bail!("Error: no path provided");
    }
    if content.len() > max_bytes {
        bail!(
            "Error: content too large for write_file ({} bytes > {} bytes)",
            content.len(),
            max_bytes
        );
    }
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    let sz = std::fs::metadata(path)?.len();
    Ok(format!("OK: wrote {sz} bytes to {path}"))
}

/// Generate a unified diff using the `similar` crate (pure Rust, no subprocess).
fn inline_diff(path: &str, old: &str, new: &str) -> Result<(String, usize, usize)> {
    if old == new {
        return Ok((String::new(), 0, 0));
    }
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    let label = path.trim_start_matches('/');
    output.push_str(&format!("--- a/{label}\n"));
    output.push_str(&format!("+++ b/{label}\n"));

    for group in diff.grouped_ops(3).iter() {
        if group.is_empty() {
            continue;
        }
        if let Some(first) = group.first() {
            let old_range: Range<usize> = first.old_range();
            let mut total_old_len = 0usize;
            let mut total_new_len = 0usize;
            for op in group {
                total_old_len += op.old_range().len();
                total_new_len += op.new_range().len();
            }
            output.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                old_range.start + 1,
                total_old_len.max(1),
                first.new_range().start + 1,
                total_new_len.max(1)
            ));
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let (sign, ansi, reset) = match change.tag() {
                    ChangeTag::Equal => (" ", "", ""),
                    ChangeTag::Insert => {
                        added += 1;
                        ("+", "\x1b[32m", "\x1b[0m")
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        ("-", "\x1b[31m", "\x1b[0m")
                    }
                };
                let val = change.value();
                if let Some(trimmed) = val.strip_suffix('\n') {
                    output.push_str(&format!("{ansi}{sign}{reset}{ansi}{trimmed}{reset}\n"));
                } else {
                    output.push_str(&format!("{ansi}{sign}{reset}{ansi}{val}{reset}\n"));
                }
            }
        }
    }

    Ok((output, added, removed))
}

pub struct ReadTool;
pub struct WriteTool;
pub struct EditTool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPathSelection {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub raw: bool,
}

pub fn split_read_path_selection(input: &str) -> Result<ReadPathSelection> {
    let mut rest = input;
    let normalized;
    let mut raw = false;

    if let Some(stripped) = rest.strip_suffix(":raw") {
        rest = stripped;
        raw = true;
    }
    if let Some(stripped) = rest.strip_prefix("raw:") {
        rest = stripped;
        raw = true;
    }
    if let Some((base, tail)) = rest.rsplit_once(":raw:") {
        normalized = format!("{base}:{tail}");
        rest = &normalized;
        raw = true;
    }

    let mut offset = None;
    let mut limit = None;
    let mut path = rest;

    if let Some((base, suffix)) = rest.rsplit_once(':')
        && !looks_like_url_host_port(rest)
        && let Some((start, parsed_limit)) = parse_line_selector(suffix)?
    {
        path = base;
        offset = Some(start);
        limit = parsed_limit;
    }

    if path.is_empty() {
        bail!("Error: no path provided");
    }

    Ok(ReadPathSelection {
        path: path.to_string(),
        offset,
        limit,
        raw,
    })
}

fn looks_like_url_host_port(input: &str) -> bool {
    if !is_web_url(input) {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(input) else {
        return false;
    };
    url.port().is_some() && url.path() == "/" && url.query().is_none() && url.fragment().is_none()
}

fn parse_line_selector(suffix: &str) -> Result<Option<(usize, Option<usize>)>> {
    if suffix.is_empty() {
        return Ok(None);
    }
    if !suffix
        .chars()
        .all(|ch| ch.is_ascii_digit() || ch == '-' || ch == '+')
    {
        return Ok(None);
    }
    let parse_line = |raw: &str| -> Result<usize> {
        let value = raw
            .parse::<usize>()
            .map_err(|_| anyhow!("Error: invalid line selector: {suffix}"))?;
        if value == 0 {
            bail!("Error: line selectors are 1-indexed; got 0");
        }
        Ok(value)
    };

    if let Some((start_raw, count_raw)) = suffix.split_once('+') {
        let start = parse_line(start_raw)?;
        let count = count_raw
            .parse::<usize>()
            .map_err(|_| anyhow!("Error: invalid line selector: {suffix}"))?;
        if count == 0 {
            bail!("Error: line selector count must be >= 1");
        }
        return Ok(Some((start, Some(count))));
    }

    if let Some((start_raw, end_raw)) = suffix.split_once('-') {
        let start = parse_line(start_raw)?;
        if end_raw.is_empty() {
            return Ok(Some((start, None)));
        }
        let end = parse_line(end_raw)?;
        if end < start {
            bail!("Error: line selector range ends before it starts: {suffix}");
        }
        return Ok(Some((start, Some(end - start + 1))));
    }

    Ok(Some((parse_line(suffix)?, None)))
}

fn resolve_tool_path(cwd: &Path, raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        bail!("Error: no path provided");
    }
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    Ok(normalize_lexically(&joined))
}

/// Canonicalize a path that may not exist yet, by walking up
/// the directory tree to find the longest existing ancestor,
/// canonicalizing that, then appending remaining components.
fn canonicalize_partial(path: &Path) -> PathBuf {
    let normalized = normalize_lexically(path);
    let mut existing = PathBuf::new();
    let mut pending: Vec<std::path::Component> = vec![];
    for component in normalized.components() {
        let candidate = existing.join(component.as_os_str());
        if candidate.exists() {
            existing = candidate;
            pending.clear();
        } else {
            pending.push(component);
        }
    }
    if existing.as_os_str().is_empty() {
        // Nothing in the path exists; return normalized as-is
        return normalized;
    }
    let existing_canonical = existing.canonicalize().unwrap_or(existing);
    let suffix: PathBuf = pending.iter().map(|c| c.as_os_str()).collect();
    if suffix.as_os_str().is_empty() {
        existing_canonical
    } else {
        existing_canonical.join(suffix)
    }
}

fn ensure_workspace_write(cwd: &Path, path: &Path) -> Result<()> {
    let root = cwd
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(cwd));
    let target = if path.exists() {
        path.canonicalize()
            .unwrap_or_else(|_| normalize_lexically(path))
    } else {
        canonicalize_partial(path)
    };
    if !target.starts_with(&root) {
        bail!(
            "Error: write blocked by file safety policy (path outside workspace: {})",
            target.display()
        );
    }
    Ok(())
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::Prefix(_) => out.push(component.as_os_str()),
        }
    }
    out
}

impl super::runner::ToolExec for ReadTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Read",
            "Read a local file.",
            super::metadata::ApprovalTier::Read,
            super::metadata::ToolResultKind::FileRead,
        )
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            path: String,
            offset: Option<usize>,
            limit: Option<usize>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let mut selection = split_read_path_selection(&args.path)?;
        // Prefer path selector range, fall back to JSON offset/limit
        if selection.offset.is_none() && args.offset.is_some() {
            selection.offset = args.offset;
        }
        if selection.limit.is_none() && args.limit.is_some() {
            selection.limit = args.limit;
        }
        if let Some(id) = crate::session::artifacts::artifact_id_from_url(&selection.path) {
            return ctx
                .artifacts
                .read_text(id)
                .map(|text| select_text_lines(&text, selection.offset, selection.limit))
                .map(super::runner::ToolOutcome::text);
        }
        if selection.path.starts_with("skill://") {
            return read_skill_resource(&selection.path, ctx)
                .map(|text| select_text_lines(&text, selection.offset, selection.limit))
                .map(super::runner::ToolOutcome::text);
        }
        if selection.path.starts_with("session://") {
            return read_session_resource(&selection.path, ctx)
                .map(|text| select_text_lines(&text, selection.offset, selection.limit))
                .map(super::runner::ToolOutcome::text);
        }
        if is_web_url(&selection.path) {
            return read_url_resource(&selection.path, ctx)
                .map(|text| select_text_lines(&text, selection.offset, selection.limit))
                .map(super::runner::ToolOutcome::text);
        }
        let path = resolve_tool_path(&ctx.cwd, &selection.path)?;
        ensure_full_read_within_limit(
            &path,
            selection.offset,
            selection.limit,
            ctx.tool_config.tool_result_max_bytes,
        )?;
        let content = read(
            &path.display().to_string(),
            selection.offset,
            selection.limit,
        )?;
        if selection.raw {
            return Ok(super::runner::ToolOutcome::text(content));
        }
        let start_line = selection.offset.unwrap_or(1);
        let snapshot = ctx
            .snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&path, &content, start_line);
        Ok(super::runner::ToolOutcome::text(format_read_snapshot(
            &selection.path,
            &snapshot.tag,
            start_line,
            &content,
        )))
    }
}

fn select_text_lines(text: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_string();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !lines.is_empty() && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let total = lines.len();
    let start = offset.unwrap_or(1).saturating_sub(1).min(total);
    let end = limit.map_or(total, |count| (start + count).min(total));
    lines[start..end].join("\n")
}

fn is_web_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

fn read_url_resource(url: &str, ctx: &crate::context::ToolContext) -> Result<String> {
    if ctx.tool_config.tool_disable.disable_web {
        bail!("Error: Web tools are disabled by configuration.");
    }
    let normalized = crate::tools::web::normalize_fetch_url(url)?;
    if let Some(record) = ctx
        .artifacts
        .find_latest_by_source("ReadUrl", &normalized)?
        && let Ok(text) = ctx.artifacts.read_record_text(&record)
    {
        return Ok(text);
    }
    let text = crate::tools::web::web_fetch(&normalized)?;
    ctx.artifacts
        .write_text("ReadUrl", "cached URL read", Some(&normalized), &text)?;
    Ok(text)
}

fn read_skill_resource(url: &str, ctx: &crate::context::ToolContext) -> Result<String> {
    let rest = url
        .strip_prefix("skill://")
        .ok_or_else(|| anyhow!("Error: invalid skill resource: {url}"))?
        .trim_matches('/');
    if rest.is_empty() || rest == "list" || rest == "all" {
        let mut out = String::from("# Skills\n");
        for skill in crate::skills::list_available_skills(&ctx.cwd, &ctx.home) {
            let source = match skill.source {
                crate::skills::SkillSource::BuiltIn => "built-in",
                crate::skills::SkillSource::FileSystem => "local",
            };
            out.push_str(&format!(
                "- {} [{}]: {}\n",
                skill.name, source, skill.description
            ));
        }
        return Ok(out);
    }
    let skill = crate::skills::resolve_skill(&ctx.cwd, &ctx.home, rest)?;
    Ok(format!(
        "# skill://{}\n\nDescription: {}\nBase directory: {}\n\n{}",
        skill.info.name, skill.info.description, skill.info.base_dir, skill.content
    ))
}

fn read_session_resource(url: &str, ctx: &crate::context::ToolContext) -> Result<String> {
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

fn session_dir(ctx: &crate::context::ToolContext) -> Result<PathBuf> {
    ctx.store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("Error: session path has no parent"))
}

fn format_session_current(ctx: &crate::context::ToolContext) -> Result<String> {
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

fn format_session_stats(ctx: &crate::context::ToolContext) -> Result<String> {
    let dir = session_dir(ctx)?;
    let raw = read_optional_file(&dir.join("stats.json"))?;
    if raw.trim().is_empty() {
        return Ok("{}".to_string());
    }
    let value: Value = serde_json::from_str(&raw)?;
    Ok(serde_json::to_string_pretty(&value)?)
}

fn format_session_messages(ctx: &crate::context::ToolContext, keep_last: usize) -> Result<String> {
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

fn format_session_artifacts(ctx: &crate::context::ToolContext) -> Result<String> {
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

impl super::runner::ToolExec for WriteTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Write",
            "Create or overwrite a local file.",
            super::metadata::ApprovalTier::Write,
            super::metadata::ToolResultKind::FileWrite,
        )
        .mutating()
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            content: String,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let path = resolve_tool_path(&ctx.cwd, &args.path)?;
        ensure_workspace_write(&ctx.cwd, &path)?;
        write(
            &path.display().to_string(),
            &args.content,
            ctx.tool_config.file_write_max_bytes,
        )
        .map(super::runner::ToolOutcome::text)
    }
}

impl super::runner::ToolExec for EditTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Edit",
            "Edit a local file.",
            super::metadata::ApprovalTier::Write,
            super::metadata::ToolResultKind::Edit,
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
            path: String,
            #[serde(default)]
            patch: Option<String>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let path = resolve_tool_path(&ctx.cwd, &args.path)?;
        ensure_workspace_write(&ctx.cwd, &path)?;
        let Some(patch) = args.patch else {
            bail!(
                "Error: Edit requires patch. Re-read the target range, then retry with the @PATH#TAG anchored patch header."
            );
        };
        let result = apply_anchored_patch(
            &path,
            &args.path,
            &patch,
            ctx.tool_config.file_write_max_bytes,
            &ctx.snapshots,
        )?;
        Ok(result).map(|s| super::runner::ToolOutcome {
            conversation_content: s.clone(),
            content: s,
            is_bash: false,
            exit_code: None,
            success: true,
            diagnostics: Vec::new(),
        })
    }
}

fn format_read_snapshot(display_path: &str, tag: &str, start_line: usize, content: &str) -> String {
    let mut out = format!("@{display_path}#{tag}");
    for (idx, line) in crate::tools::snapshot::split_content_lines(content)
        .iter()
        .enumerate()
    {
        out.push('\n');
        out.push_str(&format!("{}:{line}", start_line + idx));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchHunk {
    Replace {
        start: usize,
        end: usize,
        body: Vec<String>,
    },
    Delete {
        start: usize,
        end: usize,
    },
    InsertBefore {
        line: usize,
        body: Vec<String>,
    },
    InsertAfter {
        line: usize,
        body: Vec<String>,
    },
    InsertHead {
        body: Vec<String>,
    },
    InsertTail {
        body: Vec<String>,
    },
}

fn apply_anchored_patch(
    path: &Path,
    display_path: &str,
    patch: &str,
    max_bytes: usize,
    snapshots: &std::sync::Arc<std::sync::Mutex<crate::tools::snapshot::FileSnapshotStore>>,
) -> Result<String> {
    let (parsed, _warnings) = crate::tools::hashline::parse_patch(patch)?;
    if parsed.path != display_path {
        bail!(
            "Error: patch header path '{}' does not match Edit path '{}'",
            parsed.path,
            display_path
        );
    }

    let mut snapshot_guard = snapshots.lock().unwrap_or_else(|e| e.into_inner());
    let content = std::fs::read_to_string(path)
        .map_err(|_| anyhow!("Error: file not found: {}", path.display()))?;
    if content.len() > max_bytes {
        bail!(
            "Error: file too large for edit_file ({} bytes > {} bytes)",
            content.len(),
            max_bytes
        );
    }
    let mut lines = crate::tools::snapshot::split_content_lines(&content);
    let snapshot = snapshot_guard
        .get(path, &parsed.tag)
        .cloned()
        .ok_or_else(|| {
            let target = suggested_read_target(display_path, &parsed.hunks, lines.len());
            anyhow!(
                "Error: snapshot tag {} for {} is unknown. Re-read {}, then retry Edit with the new header.",
                parsed.tag,
                display_path,
                target
            )
        })?;

    validate_patch_hunks(&parsed.hunks, &snapshot, &lines, display_path)?;
    apply_hunks(&mut lines, &parsed.hunks)?;

    let mut updated = lines.join("\n");
    if content.ends_with('\n') {
        updated.push('\n');
    }
    if updated == content {
        let target = suggested_read_target(display_path, &parsed.hunks, lines.len());
        bail!(
            "Error: patch parsed cleanly but produced no changes. Re-read {target} before retrying."
        );
    }

    let (diff, added, removed) = inline_diff(&path.display().to_string(), &content, &updated)?;
    std::fs::write(path, &updated)?;
    snapshot_guard.record(path, &updated, 1);
    Ok(format!(
        "Edit({}) [+{} -{} lines]\n{}\n",
        display_path, added, removed, diff
    ))
}

#[derive(Debug)]
pub(crate) struct ParsedPatch {
    pub(crate) path: String,
    pub(crate) tag: String,
    pub(crate) hunks: Vec<PatchHunk>,
}

fn parse_anchored_patch(input: &str) -> Result<ParsedPatch> {
    let mut lines = input.lines().enumerate().peekable();
    let Some((_, header)) = lines.find(|(_, line)| !line.trim().is_empty()) else {
        bail!("Error: patch is empty");
    };
    let header = header.trim();
    let Some(rest) = header.strip_prefix('@') else {
        bail!("Error: patch must begin with @PATH#TAG");
    };
    let Some((path, tag)) = rest.rsplit_once('#') else {
        bail!("Error: patch header must be @PATH#TAG");
    };
    if path.is_empty() || tag.is_empty() {
        bail!("Error: patch header must be @PATH#TAG");
    }

    let mut hunks = Vec::new();
    while let Some((line_no, line)) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(range) = trimmed.strip_prefix("replace ") {
            let range = range.strip_suffix(':').ok_or_else(|| {
                anyhow!(
                    "Error: line {}: replace hunk must end with ':'",
                    line_no + 1
                )
            })?;
            let (start, end) = parse_patch_range(range)?;
            let body = collect_patch_body(&mut lines, line_no + 1, "replace")?;
            hunks.push(PatchHunk::Replace { start, end, body });
        } else if let Some(range) = trimmed.strip_prefix("delete ") {
            let (start, end) = parse_patch_range(range.trim_end_matches(':'))?;
            if matches!(lines.peek(), Some((_, next)) if next.starts_with('+')) {
                bail!(
                    "Error: line {}: delete does not take body rows",
                    line_no + 1
                );
            }
            hunks.push(PatchHunk::Delete { start, end });
        } else if let Some(target) = trimmed.strip_prefix("insert ") {
            let target = target.strip_suffix(':').ok_or_else(|| {
                anyhow!("Error: line {}: insert hunk must end with ':'", line_no + 1)
            })?;
            let body = collect_patch_body(&mut lines, line_no + 1, "insert")?;
            match target {
                "head" => hunks.push(PatchHunk::InsertHead { body }),
                "tail" => hunks.push(PatchHunk::InsertTail { body }),
                _ if target.starts_with("before ") => {
                    let line = parse_positive_usize(target.trim_start_matches("before "), target)?;
                    hunks.push(PatchHunk::InsertBefore { line, body });
                }
                _ if target.starts_with("after ") => {
                    let line = parse_positive_usize(target.trim_start_matches("after "), target)?;
                    hunks.push(PatchHunk::InsertAfter { line, body });
                }
                _ => bail!(
                    "Error: line {}: invalid insert target '{target}'",
                    line_no + 1
                ),
            }
        } else if line.starts_with('+') {
            bail!(
                "Error: line {}: payload line has no preceding hunk header",
                line_no + 1
            );
        } else {
            bail!(
                "Error: line {}: invalid patch hunk '{}'",
                line_no + 1,
                trimmed
            );
        }
    }

    if hunks.is_empty() {
        bail!("Error: patch contains no edit hunks");
    }

    Ok(ParsedPatch {
        path: path.to_string(),
        tag: tag.to_string(),
        hunks,
    })
}

fn collect_patch_body<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    header_line: usize,
    kind: &str,
) -> Result<Vec<String>>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let mut body = Vec::new();
    while let Some((_, next)) = lines.peek() {
        if !next.starts_with('+') {
            break;
        }
        let (_, row) = lines.next().unwrap();
        body.push(row.strip_prefix('+').unwrap_or(row).to_string());
    }
    if body.is_empty() {
        if let Some((_, next)) = lines.peek() {
            let trimmed = next.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('+') {
                let preview = &trimmed[..trimmed.len().min(60)];
                bail!("Error: line {header_line}: body starts with non-`+` line '{preview}...'. Body rows are ONLY new lines prefixed with `+`. Do NOT include original lines (the N..M range already identifies them).");
            }
        }
        bail!("Error: line {header_line}: {kind} hunk requires at least one +TEXT body row");
    }
    Ok(body)
}

fn parse_patch_range(raw: &str) -> Result<(usize, usize)> {
    let raw = raw.trim();
    if let Some((start, end)) = raw.split_once("..") {
        let start = parse_positive_usize(start, raw)?;
        let end = parse_positive_usize(end, raw)?;
        if end < start {
            bail!("Error: range {raw} ends before it starts");
        }
        Ok((start, end))
    } else {
        let line = parse_positive_usize(raw, raw)?;
        Ok((line, line))
    }
}

fn parse_positive_usize(raw: &str, context: &str) -> Result<usize> {
    let value = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow!("Error: invalid line number in '{context}'"))?;
    if value == 0 {
        bail!("Error: line numbers are 1-indexed; got 0");
    }
    Ok(value)
}

fn validate_patch_hunks(
    hunks: &[PatchHunk],
    snapshot: &crate::tools::snapshot::FileSnapshot,
    current_lines: &[String],
    display_path: &str,
) -> Result<()> {
    let mut targeted = BTreeSet::new();
    for hunk in hunks {
        match hunk {
            PatchHunk::Replace { start, end, .. } | PatchHunk::Delete { start, end } => {
                for line in *start..=*end {
                    validate_snapshot_line(snapshot, current_lines, line, display_path)?;
                    if !targeted.insert(line) {
                        bail!(
                            "Error: overlapping edit hunks target line {line}. Use one hunk per range."
                        );
                    }
                }
            }
            PatchHunk::InsertBefore { line, .. } | PatchHunk::InsertAfter { line, .. } => {
                validate_snapshot_line(snapshot, current_lines, *line, display_path)?;
            }
            PatchHunk::InsertHead { .. } | PatchHunk::InsertTail { .. } => {
                let current_hash = crate::tools::snapshot::hash_text(&current_lines.join("\n"));
                if current_hash != snapshot.file_hash {
                    let target = suggested_read_target(
                        display_path,
                        std::slice::from_ref(hunk),
                        current_lines.len(),
                    );
                    bail!(
                        "Error: snapshot mismatch in {display_path}. The file changed since Read. Re-read {target} before editing."
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_snapshot_line(
    snapshot: &crate::tools::snapshot::FileSnapshot,
    current_lines: &[String],
    line: usize,
    display_path: &str,
) -> Result<()> {
    let expected = snapshot.expected_hash(line).ok_or_else(|| {
        let target = suggested_read_target_for_line(display_path, line, current_lines.len());
        anyhow!(
            "Error: line {line} in {display_path} was not covered by snapshot {}. Re-read {target} before editing.",
            snapshot.tag
        )
    })?;
    let Some(current) = current_lines.get(line - 1) else {
        bail!("Error: line {line} does not exist in {display_path}");
    };
    if crate::tools::snapshot::hash_text(current) != expected {
        let target = suggested_read_target_for_line(display_path, line, current_lines.len());
        bail!(
            "Error: snapshot mismatch in {display_path} at line {line}. The file changed since Read. Re-read {target} before editing."
        );
    }
    Ok(())
}

fn suggested_read_target(display_path: &str, hunks: &[PatchHunk], line_count: usize) -> String {
    if line_count == 0 {
        return display_path.to_string();
    }
    let mut min_line = usize::MAX;
    let mut max_line = 1usize;
    for hunk in hunks {
        let (start, end) = hunk_read_span(hunk, line_count);
        min_line = min_line.min(start);
        max_line = max_line.max(end);
    }
    if min_line == usize::MAX {
        return display_path.to_string();
    }
    format_read_target(display_path, min_line, max_line, line_count)
}

fn suggested_read_target_for_line(display_path: &str, line: usize, line_count: usize) -> String {
    if line_count == 0 {
        return display_path.to_string();
    }
    let line = line.clamp(1, line_count);
    format_read_target(display_path, line, line, line_count)
}

fn format_read_target(
    display_path: &str,
    start_line: usize,
    end_line: usize,
    line_count: usize,
) -> String {
    let start = start_line.saturating_sub(EDIT_REREAD_CONTEXT_LINES).max(1);
    let end = (end_line + EDIT_REREAD_CONTEXT_LINES).min(line_count);
    format!("{display_path}:{start}-{end}")
}

fn hunk_read_span(hunk: &PatchHunk, line_count: usize) -> (usize, usize) {
    match hunk {
        PatchHunk::Replace { start, end, .. } | PatchHunk::Delete { start, end } => {
            ((*start).min(line_count), (*end).min(line_count))
        }
        PatchHunk::InsertBefore { line, .. } | PatchHunk::InsertAfter { line, .. } => {
            let line = (*line).min(line_count);
            (line, line)
        }
        PatchHunk::InsertHead { .. } => (1, 1),
        PatchHunk::InsertTail { .. } => (line_count, line_count),
    }
}

fn apply_hunks(lines: &mut Vec<String>, hunks: &[PatchHunk]) -> Result<()> {
    let mut ordered = hunks.to_vec();
    ordered.sort_by_key(|hunk| std::cmp::Reverse(hunk_apply_index(hunk, lines.len())));
    for hunk in ordered {
        match hunk {
            PatchHunk::Replace { start, end, body } => {
                lines.splice(start - 1..end, body);
            }
            PatchHunk::Delete { start, end } => {
                lines.drain(start - 1..end);
            }
            PatchHunk::InsertBefore { line, body } => {
                lines.splice(line - 1..line - 1, body);
            }
            PatchHunk::InsertAfter { line, body } => {
                lines.splice(line..line, body);
            }
            PatchHunk::InsertHead { body } => {
                lines.splice(0..0, body);
            }
            PatchHunk::InsertTail { body } => {
                lines.extend(body);
            }
        }
    }
    Ok(())
}

fn hunk_apply_index(hunk: &PatchHunk, line_count: usize) -> usize {
    match hunk {
        PatchHunk::Replace { start, .. }
        | PatchHunk::Delete { start, .. }
        | PatchHunk::InsertBefore { line: start, .. } => *start,
        PatchHunk::InsertAfter { line, .. } => line + 1,
        PatchHunk::InsertHead { .. } => 0,
        PatchHunk::InsertTail { .. } => line_count + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ToolConfig, ToolContext};
    use crate::session::artifacts::ArtifactManager;
    use crate::session::store::ConversationStore;
    use crate::tools::runner::ToolExec;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    fn temp_file(name: &str, content: &str) -> String {
        let path = format!("/tmp/mink-test-{}-{}", name, std::process::id());
        fs::write(&path, content).unwrap();
        path
    }

    fn temp_tool_context(name: &str) -> ToolContext {
        let root =
            std::env::temp_dir().join(format!("mink-tool-context-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let cwd = root.join("workspace");
        let session = home.join(".mink/projects/-workspace/session-1");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(session.join("artifacts")).unwrap();
        fs::write(session.join("conversation.jsonl"), "").unwrap();
        fs::write(session.join("stats.json"), "{}\n").unwrap();
        let artifacts = Arc::new(ArtifactManager::new(session.join("artifacts")));
        artifacts.ensure().unwrap();
        ToolContext {
            cwd,
            home,
            store: Arc::new(ConversationStore::new(session.join("conversation.jsonl"))),
            artifacts,
            snapshots: Arc::new(Mutex::new(
                crate::tools::snapshot::FileSnapshotStore::default(),
            )),
            tool_config: ToolConfig::from_config(&crate::config::Config::default()),
            interrupt: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn read_whole_file() {
        let p = temp_file("read-whole", "line1\nline2\nline3\n");
        let result = read(&p, None, None).unwrap();
        assert_eq!(result, "line1\nline2\nline3\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn read_with_offset_limit() {
        let p = temp_file("read-ol", "line1\nline2\nline3\nline4\nline5\n");
        let result = read(&p, Some(2), Some(2)).unwrap();
        assert_eq!(result, "line2\nline3");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn read_offset_exceeds_lines_error() {
        let p = temp_file("read-err", "only\n");
        let result = read(&p, Some(5), None);
        assert!(result.is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn read_large_file_offset_exceeds_lines_error() {
        let p = temp_file(
            "read-large-err",
            &format!(
                "first\n{}\nlast\n",
                "x".repeat(STREAM_READ_THRESHOLD as usize)
            ),
        );
        let result = read(&p, Some(4), None);
        assert!(result.is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn read_large_file_range_matches_line_semantics() {
        let p = temp_file(
            "read-large-range",
            &format!(
                "first\n{}\nthird\nfourth\n",
                "x".repeat(STREAM_READ_THRESHOLD as usize)
            ),
        );
        let result = read(&p, Some(3), Some(1)).unwrap();
        assert_eq!(result, "third");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn read_empty_path_error() {
        assert!(read("", None, None).is_err());
    }

    #[test]
    fn split_read_path_selection_plain_has_no_range() {
        let selection = split_read_path_selection("src/main.rs").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, None);
        assert_eq!(selection.limit, None);
        assert!(!selection.raw);
    }

    #[test]
    fn split_read_path_selection_raw_suffix() {
        let selection = split_read_path_selection("src/main.rs:raw").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, None);
        assert_eq!(selection.limit, None);
        assert!(selection.raw);
    }

    #[test]
    fn split_read_path_selection_range() {
        let selection = split_read_path_selection("src/main.rs:10-14").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, Some(10));
        assert_eq!(selection.limit, Some(5));
    }

    #[test]
    fn split_read_path_selection_plus_count() {
        let selection = split_read_path_selection("src/main.rs:10+4").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, Some(10));
        assert_eq!(selection.limit, Some(4));
    }

    #[test]
    fn split_read_path_selection_range_raw() {
        let selection = split_read_path_selection("src/main.rs:10-14:raw").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, Some(10));
        assert_eq!(selection.limit, Some(5));
        assert!(selection.raw);
    }

    #[test]
    fn split_read_path_selection_raw_range() {
        let selection = split_read_path_selection("src/main.rs:raw:10-14").unwrap();
        assert_eq!(selection.path, "src/main.rs");
        assert_eq!(selection.offset, Some(10));
        assert_eq!(selection.limit, Some(5));
        assert!(selection.raw);
    }

    #[test]
    fn split_read_path_selection_unknown_colon_suffix_left_in_path() {
        let selection = split_read_path_selection("src/main.rs:notes").unwrap();
        assert_eq!(selection.path, "src/main.rs:notes");
        assert_eq!(selection.offset, None);
        assert_eq!(selection.limit, None);
    }

    #[test]
    fn split_read_path_selection_keeps_url_host_port() {
        let selection = split_read_path_selection("https://example.com:8443").unwrap();
        assert_eq!(selection.path, "https://example.com:8443");
        assert_eq!(selection.offset, None);
        assert_eq!(selection.limit, None);
    }

    #[test]
    fn split_read_path_selection_url_range_after_path() {
        let selection = split_read_path_selection("https://example.com/docs:10-14").unwrap();
        assert_eq!(selection.path, "https://example.com/docs");
        assert_eq!(selection.offset, Some(10));
        assert_eq!(selection.limit, Some(5));
    }

    #[test]
    fn split_read_path_selection_rejects_zero_line() {
        assert!(split_read_path_selection("src/main.rs:0").is_err());
    }

    #[test]
    fn split_read_path_selection_rejects_backwards_range() {
        assert!(split_read_path_selection("src/main.rs:10-4").is_err());
    }

    #[test]
    fn read_tool_accepts_offset_limit_args() {
        let ctx = temp_tool_context("read-offset-limit-args");
        // Create a file with known content
        let dir = std::env::temp_dir().join("mink-test-read-offset-limit-args");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("test.txt");
        std::fs::write(&p, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let abspath = std::fs::canonicalize(&p).unwrap();
        let result = ReadTool.execute(
            &json!({"path": abspath.to_string_lossy(), "offset": 2, "limit": 2}),
            &ctx,
        );
        assert!(
            result.is_ok(),
            "Read with offset/limit should succeed: {:?}",
            result.err()
        );
        let outcome = result.unwrap();
        assert!(outcome.content.contains("line2"), "should contain line2");
        assert!(outcome.content.contains("line3"), "should contain line3");
        assert!(
            !outcome.content.contains("line1"),
            "should not contain line1"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_tool_rejects_large_full_read_before_loading_content() {
        let mut ctx = temp_tool_context("read-large-full-limit");
        ctx.tool_config.tool_result_max_bytes = 8;
        let p = ctx.cwd.join("large.txt");
        std::fs::write(&p, "0123456789abcdef\nsecond\n").unwrap();

        let err = match ReadTool.execute(&json!({"path": "large.txt"}), &ctx) {
            Ok(_) => panic!("large full Read should be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("file too large for full Read"), "{err}");

        let ranged = ReadTool
            .execute(&json!({"path": "large.txt", "offset": 2, "limit": 1}), &ctx)
            .unwrap();
        assert!(ranged.content.contains("second"), "{}", ranged.content);

        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn edit_tool_rejects_legacy_string_replace_args() {
        let ctx = temp_tool_context("edit-legacy-args");
        let err = match EditTool.execute(
            &json!({"path":"src/main.rs","old_string":"old","new_string":"new"}),
            &ctx,
        ) {
            Ok(_) => panic!("legacy Edit old_string/new_string args should be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn select_text_lines_applies_range() {
        let result = select_text_lines("a\nb\nc\nd\n", Some(2), Some(2));
        assert_eq!(result, "b\nc");
    }

    #[test]
    fn read_url_resource_uses_cached_artifact_with_selector() {
        let ctx = temp_tool_context("read-url-cache");
        ctx.artifacts
            .write_text(
                "ReadUrl",
                "cached URL read",
                Some("https://example.com/a"),
                "Source: https://example.com/a\n\na\nb\nc\nd",
            )
            .unwrap();

        let result = ReadTool
            .execute(&json!({"path":"https://example.com/a:4-5"}), &ctx)
            .unwrap()
            .content;

        assert_eq!(result, "b\nc");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_lists_skills() {
        let ctx = temp_tool_context("skill-list");
        let result = read_skill_resource("skill://list", &ctx).unwrap();
        assert!(result.contains("# Skills"));
        assert!(result.contains("debugging"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_returns_skill_content() {
        let ctx = temp_tool_context("skill-content");
        let result = read_skill_resource("skill://debugging", &ctx).unwrap();
        assert!(result.contains("# skill://debugging"));
        assert!(result.contains("Base directory: <built-in>"));
        assert!(result.contains("Phase 1"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_prefers_filesystem_skill() {
        let ctx = temp_tool_context("skill-local");
        let skill_dir = ctx.cwd.join(".claude/skills/debugging");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local debugging\"\n---\n\nUse local steps.",
        )
        .unwrap();

        let result = read_skill_resource("skill://debugging", &ctx).unwrap();

        assert!(result.contains("Description: Local debugging"));
        assert!(result.contains("Use local steps."));
        assert!(result.contains(&format!("Base directory: {}", skill_dir.display())));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_rejects_nested_path() {
        let ctx = temp_tool_context("skill-invalid");
        assert!(read_skill_resource("skill://debugging/extra", &ctx).is_err());
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_session_current_summarizes_paths_and_resources() {
        let ctx = temp_tool_context("session-current");
        let session = ctx.store.path().parent().unwrap().to_path_buf();
        fs::write(
            session.join("stats.json"),
            r#"{"current_turn_count":2,"agent_request_count":3,"total_input_tokens":10,"total_output_tokens":4}"#,
        )
        .unwrap();
        fs::write(
            session.join("conversation.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":[{"type":"text","text":"hi"}]})
            ),
        )
        .unwrap();

        let result = read_session_resource("session://current", &ctx).unwrap();

        assert!(result.contains("session_id: session-1"));
        assert!(result.contains("conversation_messages: 2"));
        assert!(result.contains("turns: 2"));
        assert!(result.contains("session://current/messages"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_session_messages_summarizes_conversation_jsonl() {
        let ctx = temp_tool_context("session-messages");
        let session = ctx.store.path().parent().unwrap().to_path_buf();
        fs::write(
            session.join("conversation.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({"role":"user","content":"please read file"}),
                json!({"role":"assistant","content":[{"type":"thinking","thinking":"abc"},{"type":"tool_use","name":"Read","id":"u1","input":{"path":"Cargo.toml"}}]}),
                json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"u1","content":"done"}]})
            ),
        )
        .unwrap();

        let result = read_session_resource("session://current/messages", &ctx).unwrap();

        assert!(result.contains("1 user: please read file"));
        assert!(result.contains("tool_use Read"));
        assert!(result.contains("tool_result u1 4 bytes"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn write_creates_file() {
        let p = format!("/tmp/mink-test-write-{}", std::process::id());
        let result = write(&p, "hello", 1000).unwrap();
        assert!(result.contains("OK"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn write_exceeds_max_bytes_error() {
        let p = format!("/tmp/mink-test-write-big-{}", std::process::id());
        assert!(write(&p, "toolarge", 5).is_err());
    }

    #[test]
    fn format_read_snapshot_includes_header_and_line_numbers() {
        let rendered = format_read_snapshot("src/a.rs", "0A3B", 41, "alpha\nbeta\n");
        assert_eq!(rendered, "@src/a.rs#0A3B\n41:alpha\n42:beta");
    }

    #[test]
    fn anchored_patch_replace_success() {
        let p = temp_file("anchored-replace", "one\ntwo\nthree\n");
        let path = PathBuf::from(&p);
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        ));
        let tag = snapshots
            .lock()
            .unwrap()
            .record(&path, "one\ntwo\nthree\n", 1)
            .tag;
        let patch = format!("@{p}#{tag}\nreplace 2:\n+TWO");

        let result = apply_anchored_patch(&path, &p, &patch, 1000, &snapshots).unwrap();

        assert!(result.contains("Edit("));
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\nTWO\nthree\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_rejects_stale_line() {
        let p = temp_file("anchored-stale", "one\ntwo\nthree\n");
        let path = PathBuf::from(&p);
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        ));
        let tag = snapshots
            .lock()
            .unwrap()
            .record(&path, "one\ntwo\nthree\n", 1)
            .tag;
        fs::write(&p, "one\nchanged\nthree\n").unwrap();
        let patch = format!("@{p}#{tag}\nreplace 2:\n+TWO");

        let err = apply_anchored_patch(&path, &p, &patch, 1000, &snapshots)
            .unwrap_err()
            .to_string();

        assert!(err.contains("snapshot mismatch"), "{err}");
        assert!(err.contains(&format!("Re-read {p}:1-3")), "{err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\nchanged\nthree\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_rejects_unknown_tag() {
        let p = temp_file("anchored-unknown", "one\ntwo\n");
        let path = PathBuf::from(&p);
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        ));
        let patch = format!("@{p}#FFFF\nreplace 1:\n+ONE");

        let err = apply_anchored_patch(&path, &p, &patch, 1000, &snapshots)
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown"), "{err}");
        assert!(err.contains(&format!("Re-read {p}:1-2")), "{err}");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_rejects_delete_body() {
        let err = parse_anchored_patch("@a#0001\ndelete 1\n+bad")
            .unwrap_err()
            .to_string();
        assert!(err.contains("delete does not take body"), "{err}");
    }

    #[test]
    fn workspace_write_allows_inside_cwd() {
        let cwd = std::env::temp_dir().join(format!("mink-workspace-write-{}", std::process::id()));
        fs::create_dir_all(&cwd).unwrap();
        let path = resolve_tool_path(&cwd, "src/file.txt").unwrap();
        assert!(ensure_workspace_write(&cwd, &path).is_ok());
        fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn workspace_write_blocks_parent_escape() {
        let cwd = PathBuf::from("/tmp/workspace");
        let path = resolve_tool_path(&cwd, "../outside.txt").unwrap();
        assert!(ensure_workspace_write(&cwd, &path).is_err());
    }

    #[test]
    fn workspace_write_blocks_absolute_outside_cwd() {
        let cwd = PathBuf::from("/tmp/workspace");
        let path = resolve_tool_path(&cwd, "/tmp/outside.txt").unwrap();
        assert!(ensure_workspace_write(&cwd, &path).is_err());
    }
}
