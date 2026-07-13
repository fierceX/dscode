use crate::resources::selector::{select_text_lines, split_read_path_selection};
use anyhow::{Result, anyhow, bail};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// Threshold for switching to streaming read (bytes).
const STREAM_READ_THRESHOLD: u64 = 1_048_576; // 1MB
const EDIT_REREAD_CONTEXT_LINES: usize = 12;
const EDIT_ERROR_CONTEXT_LINES: usize = 2;

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
        if ctx.resource_router.can_handle(&selection.path) {
            let resource = ctx.resource_router.resolve(&selection, ctx)?;
            let text = select_text_lines(&resource.content, selection.offset, selection.limit);
            return Ok(super::runner::ToolOutcome::text(text));
        }
        if ctx.resource_router.is_url_like(&selection.path) && !is_web_url(&selection.path) {
            let scheme = selection
                .path
                .split_once("://")
                .map_or("", |(scheme, _)| scheme);
            bail!("Error: unknown resource scheme: {scheme}");
        }
        if is_web_url(&selection.path) {
            return read_url_resource(&selection.path, ctx)
                .map(|text| select_text_lines(&text, selection.offset, selection.limit))
                .map(super::runner::ToolOutcome::text);
        }
        if let Some(vfs) = &ctx.read_only_fs {
            let result = vfs.read(
                &ctx.vfs_scope,
                &crate::tools::vfs::VfsReadRequest {
                    path: selection.path.clone(),
                    offset: selection.offset,
                    limit: selection.limit,
                    max_full_read_bytes: ctx.tool_config.tool_result_max_bytes,
                },
            )?;
            if selection.offset.is_none()
                && selection.limit.is_none()
                && result.total_bytes > ctx.tool_config.tool_result_max_bytes
            {
                bail!(
                    "Error: file too large for full Read ({} bytes > {} bytes): {}. Use a line selector such as '{}:1-200' or pass offset/limit.",
                    result.total_bytes,
                    ctx.tool_config.tool_result_max_bytes,
                    selection.path,
                    selection.path
                );
            }
            if selection.raw {
                return Ok(super::runner::ToolOutcome::text(result.content));
            }
            return Ok(super::runner::ToolOutcome::text(format_read_only_virtual(
                &selection.path,
                selection.offset.unwrap_or(1),
                &result.content,
            )));
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
        let (snapshot_content, snapshot_start_line) = snapshot_source_for_read(
            &path,
            &content,
            start_line,
            ctx.tool_config.file_write_max_bytes,
        );
        let snapshot = ctx
            .snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&path, &snapshot_content, snapshot_start_line);
        Ok(super::runner::ToolOutcome::text(format_read_snapshot(
            &selection.path,
            &snapshot.tag,
            start_line,
            &content,
        )))
    }
}

fn snapshot_source_for_read(
    path: &Path,
    displayed_content: &str,
    displayed_start_line: usize,
    max_edit_bytes: usize,
) -> (String, usize) {
    let full_text = std::fs::metadata(path)
        .ok()
        .filter(|meta| meta.len() as u128 <= max_edit_bytes as u128)
        .and_then(|_| std::fs::read_to_string(path).ok());
    match full_text {
        Some(text) => (text, 1),
        None => (displayed_content.to_string(), displayed_start_line),
    }
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
        let result = write(
            &path.display().to_string(),
            &args.content,
            ctx.tool_config.file_write_max_bytes,
        )?;
        ctx.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .invalidate_path(&path);
        Ok(super::runner::ToolOutcome::text(result))
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
        if input.get("old_string").is_some() || input.get("new_string").is_some() {
            bail!(
                "Error: Edit old_string/new_string is not supported. Use Read on the target range to get @PATH#TAG, then call Edit with the patch parameter."
            );
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            path: String,
            #[serde(default)]
            patch: Option<String>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let path = resolve_tool_path(&ctx.cwd, &args.path)?;
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

fn format_read_only_virtual(display_path: &str, start_line: usize, content: &str) -> String {
    let mut out = format!("[read-only virtual file: {display_path}]");
    for (idx, line) in crate::tools::snapshot::split_content_lines(content)
        .iter()
        .enumerate()
    {
        out.push('\n');
        out.push_str(&format!("{}:{line}", start_line + idx));
    }
    out
}

fn format_post_edit_snapshot(
    display_path: &str,
    tag: &str,
    content: &str,
    hunks: &[PatchHunk],
) -> String {
    let lines = crate::tools::snapshot::split_content_lines(content);
    if lines.is_empty() {
        return format!("@{display_path}#{tag}");
    }

    let mut min_line = usize::MAX;
    let mut max_line = 1usize;
    for hunk in hunks {
        let (start, end) = hunk_post_edit_span(hunk, lines.len());
        min_line = min_line.min(start);
        max_line = max_line.max(end);
    }
    if min_line == usize::MAX {
        min_line = 1;
        max_line = lines.len().min(1);
    }

    let start = min_line.saturating_sub(EDIT_REREAD_CONTEXT_LINES).max(1);
    let end = (max_line + EDIT_REREAD_CONTEXT_LINES).min(lines.len());
    format_read_snapshot(display_path, tag, start, &lines[start - 1..end].join("\n"))
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
    let (parsed, warnings) = crate::tools::hashline::parse_patch(patch)?;
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
            let context = format_current_context(
                &lines,
                &collect_hunk_anchor_lines(&parsed.hunks, lines.len()),
            );
            anyhow!(
                "Error: snapshot tag {} for {} is unknown. Re-read {}, then retry Edit with the new header.{}",
                parsed.tag,
                display_path,
                target,
                context
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
        let context = format_current_context(
            &lines,
            &collect_hunk_anchor_lines(&parsed.hunks, lines.len()),
        );
        bail!(
            "Error: patch parsed cleanly but produced no changes. Re-read {target} before retrying.{context}"
        );
    }

    let (diff, added, removed) = inline_diff(&path.display().to_string(), &content, &updated)?;
    std::fs::write(path, &updated)?;
    snapshot_guard.invalidate_path(path);
    let snapshot = snapshot_guard.record(path, &updated, 1);
    let followup = format_post_edit_snapshot(display_path, &snapshot.tag, &updated, &parsed.hunks);
    let warning_block = if warnings.is_empty() {
        String::new()
    } else {
        format!("\nWarnings:\n{}\n", warnings.join("\n"))
    };
    Ok(format!(
        "Edit({}) [+{} -{} lines]\n{}\n{}{}\n",
        display_path, added, removed, followup, warning_block, diff
    ))
}

#[derive(Debug)]
pub(crate) struct ParsedPatch {
    pub(crate) path: String,
    pub(crate) tag: String,
    pub(crate) hunks: Vec<PatchHunk>,
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
                    let context = format_current_context(
                        current_lines,
                        &hunk_anchor_lines(hunk, current_lines.len()),
                    );
                    bail!(
                        "Error: snapshot mismatch in {display_path}. The file changed since Read. Re-read {target} before editing.{context}"
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
        let context = format_current_context(current_lines, &[line]);
        anyhow!(
            "Error: line {line} in {display_path} was not covered by snapshot {}. Re-read {target} before editing.{context}",
            snapshot.tag
        )
    })?;
    let Some(current) = current_lines.get(line - 1) else {
        bail!("Error: line {line} does not exist in {display_path}");
    };
    if crate::tools::snapshot::hash_text(current) != expected {
        let target = suggested_read_target_for_line(display_path, line, current_lines.len());
        let context = format_current_context(current_lines, &[line]);
        bail!(
            "Error: snapshot mismatch in {display_path} at line {line}. The file changed since Read. Re-read {target} before editing.{context}"
        );
    }
    Ok(())
}

fn collect_hunk_anchor_lines(hunks: &[PatchHunk], line_count: usize) -> Vec<usize> {
    let mut lines = Vec::new();
    for hunk in hunks {
        lines.extend(hunk_anchor_lines(hunk, line_count));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn hunk_anchor_lines(hunk: &PatchHunk, line_count: usize) -> Vec<usize> {
    let clamp = |line: usize| {
        if line_count == 0 {
            1
        } else {
            line.clamp(1, line_count)
        }
    };
    match hunk {
        PatchHunk::Replace { start, end, .. } | PatchHunk::Delete { start, end } => {
            let start = clamp(*start);
            let end = clamp(*end);
            if start == end {
                vec![start]
            } else {
                vec![start, end]
            }
        }
        PatchHunk::InsertBefore { line, .. } | PatchHunk::InsertAfter { line, .. } => {
            vec![clamp(*line)]
        }
        PatchHunk::InsertHead { .. } => vec![1],
        PatchHunk::InsertTail { .. } => vec![line_count.max(1)],
    }
}

fn format_current_context(current_lines: &[String], anchor_lines: &[usize]) -> String {
    if current_lines.is_empty() || anchor_lines.is_empty() {
        return String::new();
    }
    let anchors: BTreeSet<usize> = anchor_lines
        .iter()
        .copied()
        .filter(|line| *line >= 1 && *line <= current_lines.len())
        .collect();
    if anchors.is_empty() {
        return String::new();
    }

    let mut display_lines = BTreeSet::new();
    for line in &anchors {
        let start = line.saturating_sub(EDIT_ERROR_CONTEXT_LINES).max(1);
        let end = (*line + EDIT_ERROR_CONTEXT_LINES).min(current_lines.len());
        for display_line in start..=end {
            display_lines.insert(display_line);
        }
    }

    let mut out = String::from("\nCurrent context:\n");
    let mut previous = 0usize;
    for line in display_lines {
        if previous != 0 && line > previous + 1 {
            out.push_str("...\n");
        }
        previous = line;
        let marker = if anchors.contains(&line) { '*' } else { ' ' };
        let text = current_lines
            .get(line - 1)
            .map(String::as_str)
            .unwrap_or("");
        out.push_str(&format!("{marker}{line}:{text}\n"));
    }
    out
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

fn hunk_post_edit_span(hunk: &PatchHunk, line_count: usize) -> (usize, usize) {
    if line_count == 0 {
        return (1, 1);
    }
    match hunk {
        PatchHunk::Replace { start, body, .. } => {
            let start = (*start).min(line_count).max(1);
            let end = start
                .saturating_add(body.len().saturating_sub(1))
                .min(line_count);
            (start, end.max(start))
        }
        PatchHunk::Delete { start, .. } => {
            let line = (*start).min(line_count).max(1);
            (line, line)
        }
        PatchHunk::InsertBefore { line, body } => {
            let start = (*line).min(line_count).max(1);
            let end = start
                .saturating_add(body.len().saturating_sub(1))
                .min(line_count);
            (start, end.max(start))
        }
        PatchHunk::InsertAfter { line, body } => {
            let start = line.saturating_add(1).min(line_count).max(1);
            let end = start
                .saturating_add(body.len().saturating_sub(1))
                .min(line_count);
            (start, end.max(start))
        }
        PatchHunk::InsertHead { body } => (1, body.len().max(1).min(line_count)),
        PatchHunk::InsertTail { body } => {
            let count = body.len().max(1).min(line_count);
            (line_count - count + 1, line_count)
        }
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
        let capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &cwd,
                &home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );
        ToolContext {
            vfs_scope: crate::tools::vfs::VfsScope {
                resource_session_id: "session-1".into(),
                agent_session_id: "session-1".into(),
            },
            read_only_fs: None,
            cwd,
            home,
            store: Arc::new(ConversationStore::new(session.join("conversation.jsonl"))),
            artifacts,
            snapshots: Arc::new(Mutex::new(
                crate::tools::snapshot::FileSnapshotStore::default(),
            )),
            tool_config: ToolConfig::from_config(&crate::config::Config::default()),
            interrupt: Arc::new(AtomicBool::new(false)),
            resource_router: Arc::new(crate::resources::ResourceRouter::with_builtin_handlers()),
            capability_snapshot,
        }
    }

    struct VirtualReadOnlyFs;

    impl crate::tools::vfs::ReadOnlyFileSystem for VirtualReadOnlyFs {
        fn read(
            &self,
            scope: &crate::tools::vfs::VfsScope,
            request: &crate::tools::vfs::VfsReadRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsReadResult> {
            assert_eq!(scope.resource_session_id, "session-1");
            assert_eq!(scope.agent_session_id, "session-1");
            assert_eq!(request.path, "knowledge/guide.md");
            Ok(crate::tools::vfs::VfsReadResult {
                content: "alpha\nbeta".into(),
                total_lines: 2,
                total_bytes: 10,
            })
        }

        fn glob(
            &self,
            _scope: &crate::tools::vfs::VfsScope,
            _request: &crate::tools::vfs::VfsGlobRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsGlobResult> {
            unreachable!()
        }

        fn grep(
            &self,
            _scope: &crate::tools::vfs::VfsScope,
            _request: &crate::tools::vfs::VfsGrepRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsGrepResult> {
            unreachable!()
        }
    }

    #[test]
    fn read_tool_routes_virtual_files_without_edit_snapshot() {
        let mut ctx = temp_tool_context("virtual-read");
        ctx.read_only_fs = Some(Arc::new(VirtualReadOnlyFs));
        let outcome = ReadTool
            .execute(&json!({"path": "knowledge/guide.md"}), &ctx)
            .unwrap();

        assert!(
            outcome
                .content
                .starts_with("[read-only virtual file: knowledge/guide.md]")
        );
        assert!(outcome.content.contains("1:alpha"));
        assert!(!outcome.content.starts_with('@'));
    }

    #[test]
    fn read_tool_enforces_full_read_limit_for_virtual_backend() {
        let mut ctx = temp_tool_context("virtual-read-limit");
        ctx.read_only_fs = Some(Arc::new(VirtualReadOnlyFs));
        ctx.tool_config.tool_result_max_bytes = 5;
        let error = match ReadTool.execute(&json!({"path": "knowledge/guide.md"}), &ctx) {
            Ok(_) => panic!("virtual full read should exceed configured limit"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("file too large for full Read"));
    }

    #[test]
    fn unknown_resource_scheme_fails_closed() {
        let ctx = temp_tool_context("unknown-resource");
        let err = match ReadTool.execute(&json!({"path": "kb://policy/rust"}), &ctx) {
            Ok(_) => panic!("unknown resource scheme should fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(err.contains("unknown resource scheme: kb"), "{err}");
    }

    #[test]
    fn unknown_resource_scheme_does_not_enter_vfs() {
        let mut ctx = temp_tool_context("unknown-resource-vfs");
        ctx.read_only_fs = Some(Arc::new(VirtualReadOnlyFs));
        let err = match ReadTool.execute(&json!({"path": "kb://policy/rust"}), &ctx) {
            Ok(_) => panic!("unknown resource scheme should fail before VFS"),
            Err(error) => error.to_string(),
        };
        assert!(err.contains("unknown resource scheme: kb"), "{err}");
    }

    #[test]
    fn resource_router_does_not_change_local_read_snapshot() {
        let ctx = temp_tool_context("router-local-snapshot");
        fs::write(ctx.cwd.join("local.txt"), "alpha\nbeta\n").unwrap();

        let outcome = ReadTool
            .execute(&json!({"path": "local.txt"}), &ctx)
            .unwrap();

        assert!(
            outcome.content.starts_with("@local.txt#"),
            "{}",
            outcome.content
        );
        assert!(outcome.content.contains("1:alpha"), "{}", outcome.content);
        assert!(outcome.content.contains("2:beta"), "{}", outcome.content);
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn resource_router_does_not_consume_selector_twice() {
        let ctx = temp_tool_context("router-selector-once");
        let record = ctx
            .artifacts
            .write_text("Bash", "full output", None, "one\ntwo\nthree\n")
            .unwrap();

        let outcome = ReadTool
            .execute(
                &json!({"path": format!("artifact://{}:2-2", record.id)}),
                &ctx,
            )
            .unwrap();

        assert_eq!(outcome.content, "two");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
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
        assert!(
            err.contains("old_string/new_string is not supported"),
            "{err}"
        );
    }

    #[test]
    fn ranged_read_records_full_file_snapshot_for_edit() {
        let ctx = temp_tool_context("read-range-full-snapshot");
        let p = ctx.cwd.join("range.txt");
        fs::write(&p, "one\ntwo\nthree\n").unwrap();

        let read = ReadTool
            .execute(&json!({"path":"range.txt:2-2"}), &ctx)
            .unwrap()
            .content;
        assert!(read.contains("@range.txt#"), "{read}");
        assert!(read.contains("2:two"), "{read}");
        assert!(!read.contains("1:one"), "{read}");
        let tag = read
            .lines()
            .next()
            .unwrap()
            .strip_prefix("@range.txt#")
            .unwrap();

        let patch = format!("@range.txt#{tag}\nreplace 1:\n+ONE");
        let edited = EditTool
            .execute(&json!({"path":"range.txt","patch":patch}), &ctx)
            .unwrap()
            .content;

        assert!(edited.contains("@range.txt#"), "{edited}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "ONE\ntwo\nthree\n");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
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
        let result = crate::resources::skill::read_skill_resource("skill://list", &ctx).unwrap();
        assert!(result.contains("# Skills"));
        assert!(result.contains("debugging"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_returns_skill_content() {
        let ctx = temp_tool_context("skill-content");
        let result =
            crate::resources::skill::read_skill_resource("skill://debugging", &ctx).unwrap();
        assert!(result.contains("# skill://debugging"));
        assert!(result.contains("Base directory: <built-in>"));
        assert!(result.contains("Phase 1"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_allows_trailing_slash() {
        let ctx = temp_tool_context("skill-trailing-slash");
        let result =
            crate::resources::skill::read_skill_resource("skill://debugging/", &ctx).unwrap();
        assert!(result.contains("# skill://debugging"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_rejects_empty_authority_path() {
        let ctx = temp_tool_context("skill-empty-authority");
        let err = crate::resources::skill::read_skill_resource("skill:///debugging", &ctx)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid skill resource"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_resource_prefers_filesystem_skill() {
        let mut ctx = temp_tool_context("skill-local");
        let skill_dir = ctx.cwd.join(".claude/skills/debugging");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local debugging\"\n---\n\nUse local steps.",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let result =
            crate::resources::skill::read_skill_resource("skill://debugging", &ctx).unwrap();

        assert!(result.contains("Description: Local debugging"));
        assert!(result.contains("Use local steps."));
        assert!(result.contains(&format!("Base directory: {}", skill_dir.display())));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn skill_list_excludes_model_addressable() {
        let mut ctx = temp_tool_context("skill-hidden-list");
        let skill_dir = ctx.cwd.join("skills/hidden-review");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Hidden review\"\nhide: true\n---\n\nUse hidden steps.",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let result = crate::resources::skill::read_skill_resource("skill://list", &ctx).unwrap();

        assert!(!result.contains("hidden-review"), "{result}");
        let hidden =
            crate::resources::skill::read_skill_resource("skill://hidden-review", &ctx).unwrap();
        assert!(hidden.contains("Use hidden steps."));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn skill_list_all_includes_model_addressable_and_selected() {
        let mut ctx = temp_tool_context("skill-list-all");
        let skill_dir = ctx.cwd.join("skills/hidden-review");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Hidden review\"\nhide: true\n---\n\nUse hidden steps.",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &["hidden-review".to_string()],
            )
            .unwrap(),
        );

        let result =
            crate::resources::skill::read_skill_resource("skill://list/all", &ctx).unwrap();

        assert!(result.contains("## Discoverable"));
        assert!(result.contains("## Addressable"));
        assert!(result.contains("## Selected"));
        assert!(result.contains("- hidden-review [skills, model-addressable]: Hidden review"));
        assert!(result.contains("- hidden-review [skills]: Hidden review"));
        let alias = crate::resources::skill::read_skill_resource("skill://all", &ctx).unwrap();
        assert_eq!(result, alias);
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn skill_list_all_does_not_leak_host_only() {
        let mut ctx = temp_tool_context("skill-list-all-host-only");
        let loaded = crate::capabilities::skills::LoadedSkill {
            skill: crate::capabilities::skills::SkillCapability {
                name: "host-secret".to_string(),
                description: "Do not leak this description".to_string(),
                content: "secret body".to_string(),
                base_dir: "/secret/base".to_string(),
                disable_model_invocation: true,
            },
            source: crate::capabilities::SourceMeta {
                provider_id: "test-provider".to_string(),
                provider_name: "test provider".to_string(),
                level: crate::capabilities::SourceLevel::Runtime,
                source_path: None,
                display_label: Some("test".to_string()),
            },
            exposure: crate::capabilities::CapabilityExposure::HostOnly,
            revision: "rev".to_string(),
        };
        let mut by_name = std::collections::BTreeMap::new();
        by_name.insert("host-secret".to_string(), loaded.clone());
        ctx.capability_snapshot = Arc::new(crate::capabilities::CapabilitySnapshot {
            skills: crate::capabilities::SkillSnapshot {
                all: vec![loaded],
                by_name,
                warnings: vec![crate::capabilities::CapabilityWarning {
                    provider_id: "test-provider".to_string(),
                    message: "host-only skill 'host-secret' is hidden".to_string(),
                }],
                dependency_fingerprint: "deps".to_string(),
                ..crate::capabilities::SkillSnapshot::default()
            },
            context_files: crate::capabilities::ContextFileSnapshot::default(),
            rules: crate::capabilities::RuleSnapshot::default(),
            warnings: Vec::new(),
            dependency_fingerprint: "deps".to_string(),
        });

        let result =
            crate::resources::skill::read_skill_resource("skill://list/all", &ctx).unwrap();

        assert!(result.contains("host-only skill 'host-secret' is hidden"));
        assert!(!result.contains("Do not leak this description"), "{result}");
        assert!(!result.contains("secret body"), "{result}");
        assert!(!result.contains("/secret/base"), "{result}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn host_only_skill_cannot_be_read() {
        let mut ctx = temp_tool_context("skill-host-only");
        let loaded = crate::capabilities::skills::LoadedSkill {
            skill: crate::capabilities::skills::SkillCapability {
                name: "host-secret".to_string(),
                description: "Host secret".to_string(),
                content: "secret body".to_string(),
                base_dir: "<runtime>".to_string(),
                disable_model_invocation: true,
            },
            source: crate::capabilities::SourceMeta {
                provider_id: "test".to_string(),
                provider_name: "test".to_string(),
                level: crate::capabilities::SourceLevel::Runtime,
                source_path: None,
                display_label: Some("test".to_string()),
            },
            exposure: crate::capabilities::CapabilityExposure::HostOnly,
            revision: "rev".to_string(),
        };
        let mut by_name = std::collections::BTreeMap::new();
        by_name.insert("host-secret".to_string(), loaded.clone());
        ctx.capability_snapshot = Arc::new(crate::capabilities::CapabilitySnapshot {
            skills: crate::capabilities::SkillSnapshot {
                all: vec![loaded],
                by_name,
                dependency_fingerprint: "deps".to_string(),
                ..crate::capabilities::SkillSnapshot::default()
            },
            context_files: crate::capabilities::ContextFileSnapshot::default(),
            rules: crate::capabilities::RuleSnapshot::default(),
            warnings: Vec::new(),
            dependency_fingerprint: "deps".to_string(),
        });

        let err = crate::resources::skill::read_skill_resource("skill://host-secret", &ctx)
            .unwrap_err()
            .to_string();

        assert!(err.contains("host-only"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_returns_file_content() {
        let mut ctx = temp_tool_context("skill-subresource");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nUse references/details.md.",
        )
        .unwrap();
        fs::write(
            skill_dir.join("references/details.md"),
            "alpha\nbeta\ngamma\n",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let result = crate::resources::skill::read_skill_resource(
            "skill://local-guide/references/details.md",
            &ctx,
        )
        .unwrap();

        assert!(result.contains("# skill://local-guide/references/details.md"));
        assert!(result.contains("Content-Type: text/markdown"));
        assert!(result.contains("alpha\nbeta\ngamma"));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_applies_line_selector() {
        let mut ctx = temp_tool_context("skill-subresource-selector");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nUse references/details.txt.",
        )
        .unwrap();
        fs::write(skill_dir.join("references/details.txt"), "a\nb\nc\nd\n").unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let result = ReadTool
            .execute(
                &json!({"path":"skill://local-guide/references/details.txt:6-7"}),
                &ctx,
            )
            .unwrap()
            .content;

        assert_eq!(result, "a\nb");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_rejects_parent_dir() {
        let mut ctx = temp_tool_context("skill-subresource-parent");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nbody",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let err =
            crate::resources::skill::read_skill_resource("skill://local-guide/../secret", &ctx)
                .unwrap_err()
                .to_string();

        assert!(err.contains("escapes skill directory"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_rejects_encoded_parent_dir() {
        let mut ctx = temp_tool_context("skill-subresource-encoded-parent");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nbody",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let err =
            crate::resources::skill::read_skill_resource("skill://local-guide/%2E%2E/secret", &ctx)
                .unwrap_err()
                .to_string();

        assert!(err.contains("escapes skill directory"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_rejects_absolute_path() {
        let mut ctx = temp_tool_context("skill-subresource-absolute");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nbody",
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let err =
            crate::resources::skill::read_skill_resource("skill://local-guide/%2Ftmp/secret", &ctx)
                .unwrap_err()
                .to_string();

        assert!(err.contains("must be relative"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_rejects_builtin() {
        let ctx = temp_tool_context("skill-subresource-builtin");
        let err = crate::resources::skill::read_skill_resource(
            "skill://debugging/references/foo.md",
            &ctx,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("filesystem-backed skills"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_skill_subresource_rejects_host_only() {
        let mut ctx = temp_tool_context("skill-subresource-host-only");
        let loaded = crate::capabilities::skills::LoadedSkill {
            skill: crate::capabilities::skills::SkillCapability {
                name: "host-secret".to_string(),
                description: "Host secret".to_string(),
                content: "secret body".to_string(),
                base_dir: "<runtime>".to_string(),
                disable_model_invocation: true,
            },
            source: crate::capabilities::SourceMeta {
                provider_id: "test".to_string(),
                provider_name: "test".to_string(),
                level: crate::capabilities::SourceLevel::Runtime,
                source_path: None,
                display_label: Some("test".to_string()),
            },
            exposure: crate::capabilities::CapabilityExposure::HostOnly,
            revision: "rev".to_string(),
        };
        let mut by_name = std::collections::BTreeMap::new();
        by_name.insert("host-secret".to_string(), loaded.clone());
        ctx.capability_snapshot = Arc::new(crate::capabilities::CapabilitySnapshot {
            skills: crate::capabilities::SkillSnapshot {
                all: vec![loaded],
                by_name,
                dependency_fingerprint: "deps".to_string(),
                ..crate::capabilities::SkillSnapshot::default()
            },
            context_files: crate::capabilities::ContextFileSnapshot::default(),
            rules: crate::capabilities::RuleSnapshot::default(),
            warnings: Vec::new(),
            dependency_fingerprint: "deps".to_string(),
        });

        let err =
            crate::resources::skill::read_skill_resource("skill://host-secret/secret.txt", &ctx)
                .unwrap_err()
                .to_string();

        assert!(err.contains("host-only"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn read_skill_subresource_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let mut ctx = temp_tool_context("skill-subresource-symlink");
        let skill_dir = ctx.cwd.join("skills/local-guide");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local guide\"\n---\n\nbody",
        )
        .unwrap();
        fs::write(ctx.cwd.join("outside.txt"), "secret").unwrap();
        symlink(
            ctx.cwd.join("outside.txt"),
            skill_dir.join("references/outside.txt"),
        )
        .unwrap();
        ctx.capability_snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &ctx.cwd,
                &ctx.home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );

        let err = crate::resources::skill::read_skill_resource(
            "skill://local-guide/references/outside.txt",
            &ctx,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("escapes skill directory"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_rule_resource_lists_and_returns_content() {
        let ctx = temp_tool_context("rule-resource");
        let list = crate::resources::rule::read_rule_resource("rule://list", &ctx).unwrap();
        assert!(list.contains("default-agent-rules"));

        let rule =
            crate::resources::rule::read_rule_resource("rule://default-agent-rules", &ctx).unwrap();
        assert!(rule.contains("# rule://default-agent-rules"));
        assert!(rule.contains("Prefer safe, exact edits."));
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
    }

    #[test]
    fn read_tool_routes_rule_resource() {
        let ctx = temp_tool_context("rule-read-tool");
        let result = ReadTool
            .execute(
                &serde_json::json!({"path":"rule://default-agent-rules"}),
                &ctx,
            )
            .unwrap();
        assert!(result.content.contains("Prefer safe, exact edits."));
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

        let result =
            crate::resources::session::read_session_resource("session://current", &ctx).unwrap();

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

        let result =
            crate::resources::session::read_session_resource("session://current/messages", &ctx)
                .unwrap();

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
    fn write_tool_invalidates_existing_snapshots() {
        let ctx = temp_tool_context("write-invalidates-snapshot");
        let p = ctx.cwd.join("write-stale.txt");
        fs::write(&p, "old\n").unwrap();
        let tag = ctx.snapshots.lock().unwrap().record(&p, "old\n", 1).tag;

        WriteTool
            .execute(&json!({"path":"write-stale.txt","content":"new\n"}), &ctx)
            .unwrap();
        let patch = format!("@write-stale.txt#{tag}\nreplace 1:\n+again");
        let err = match EditTool.execute(&json!({"path":"write-stale.txt","patch":patch}), &ctx) {
            Ok(_) => panic!("stale tag after Write should be rejected"),
            Err(err) => err.to_string(),
        };

        assert!(err.contains("unknown"), "{err}");
        let _ = fs::remove_dir_all(ctx.home.parent().unwrap());
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
        assert!(result.contains(&format!("@{p}#")), "{result}");
        assert!(result.contains("2:TWO"), "{result}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\nTWO\nthree\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_result_includes_parser_warnings() {
        let p = temp_file("anchored-warning", "one\ntwo\nthree\n");
        let path = PathBuf::from(&p);
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        ));
        let tag = snapshots
            .lock()
            .unwrap()
            .record(&path, "one\ntwo\nthree\n", 1)
            .tag;
        let patch = format!("@{p}#{tag}\nreplace 2:\nTWO");

        let result = apply_anchored_patch(&path, &p, &patch, 1000, &snapshots).unwrap();

        assert!(result.contains("Warnings:"), "{result}");
        assert!(result.contains("body row missing '+' prefix"), "{result}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\nTWO\nthree\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_invalidates_old_tags_after_success() {
        let p = temp_file("anchored-invalidates-old", "one\ntwo\nthree\n");
        let path = PathBuf::from(&p);
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        ));
        let old_tag = snapshots
            .lock()
            .unwrap()
            .record(&path, "one\ntwo\nthree\n", 1)
            .tag;
        let first_patch = format!("@{p}#{old_tag}\nreplace 1:\n+ONE");
        apply_anchored_patch(&path, &p, &first_patch, 1000, &snapshots).unwrap();

        let stale_patch = format!("@{p}#{old_tag}\nreplace 3:\n+THREE");
        let err = apply_anchored_patch(&path, &p, &stale_patch, 1000, &snapshots)
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown"), "{err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "ONE\ntwo\nthree\n");
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
        assert!(err.contains("Current context:"), "{err}");
        assert!(err.contains("*2:changed"), "{err}");
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
        assert!(err.contains("Current context:"), "{err}");
        assert!(err.contains("*1:one"), "{err}");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn anchored_patch_rejects_delete_body() {
        let err = crate::tools::hashline::parse_patch("@a#0001\ndelete 1\n+bad")
            .unwrap_err()
            .to_string();
        assert!(err.contains("delete does not take body"), "{err}");
    }
}
