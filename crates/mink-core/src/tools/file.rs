use crate::resources::selector::{select_text_lines, split_read_path_selection};
use crate::tools::surface::FilesystemBackend;
use anyhow::{Result, anyhow, bail, ensure};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// Threshold for switching to streaming read (bytes).
const STREAM_READ_THRESHOLD: u64 = 1_048_576; // 1MB

const FULL_READ_PREVIEW_LINES: usize = 200;

fn file_not_found_suggestion(path: &Path) -> String {
    let pattern = path
        .parent()
        .map(|parent| format!("{}/*", parent.display()))
        .unwrap_or_else(|| "*".to_string());
    format!(
        "Error: file not found or unreadable: {}. Use Glob(pattern='{pattern}') to discover existing files.",
        path.display()
    )
}

/// Read up to `max_lines` lines from the file head. Returns the lines and,
/// when the whole file was read, the exact line count (None otherwise).
fn read_head_lines(path: &Path, max_lines: usize) -> Result<(Vec<String>, Option<usize>)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|error| anyhow!("Error: cannot read {}: {error}", path.display()))?;
    let mut lines = Vec::with_capacity(max_lines + 1);
    for line in std::io::BufReader::new(file).lines() {
        let line =
            line.map_err(|error| anyhow!("Error: cannot read {}: {error}", path.display()))?;
        lines.push(line);
        if lines.len() > max_lines {
            lines.pop();
            return Ok((lines, None));
        }
    }
    let total = lines.len();
    Ok((lines, Some(total)))
}

/// Read the last `max_lines` lines of a file (for the tail portion of an
/// oversized-read preview).
fn read_tail_lines(path: &Path, max_lines: usize) -> Result<Vec<String>> {
    use std::io::{Seek as _, SeekFrom};
    let meta = std::fs::metadata(path)?;
    let window = 8192usize.min(meta.len() as usize);
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::End(-(window as i64)))?;
    let mut bytes = Vec::new();
    std::io::BufReader::new(file).read_to_end(&mut bytes)?;
    // 窗口起点可能落在多字节 UTF-8 字符中间：丢弃残缺的首字符再解码，
    // 否则含 CJK 的超限文件整读失败。
    if window < meta.len() as usize {
        let mut start = 0;
        while start < bytes.len() && (bytes[start] >> 6) == 0b10 {
            start += 1;
        }
        bytes.drain(..start);
    }
    let text = String::from_utf8(bytes)?;
    let mut lines = text.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.into_iter().map(str::to_string).collect())
}

/// Format the tail block of a preview: absolute line numbers when the exact
/// total is known, `…` prefixes otherwise.
fn format_preview_tail(tail: &[String], total: Option<usize>) -> String {
    if tail.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n...\n");
    let base = total.map(|total| total.saturating_sub(tail.len()));
    for (index, line) in tail.iter().enumerate() {
        match base {
            Some(base) => out.push_str(&format!("{}:{line}\n", base + index + 1)),
            None => out.push_str(&format!("…:{line}\n")),
        }
    }
    out
}
/// When a full read would exceed the byte limit, return a preview response
/// (size, line count, head lines, tail lines, selector example); Ok(None)
/// means the file fits within the limit and should be read normally.
fn full_read_preview(path: &Path, max_bytes: usize) -> Result<Option<String>> {
    let meta = std::fs::metadata(path).map_err(|_| anyhow!(file_not_found_suggestion(path)))?;
    if meta.len() as u128 <= max_bytes as u128 {
        return Ok(None);
    }
    let (head, exact_total) = read_head_lines(path, FULL_READ_PREVIEW_LINES)?;
    let total_desc = match exact_total {
        Some(total) => format!("{total} lines"),
        None => format!("more than {FULL_READ_PREVIEW_LINES} lines"),
    };
    let mut out = format!(
        "(file too large: {} bytes / {total_desc}; showing first {FULL_READ_PREVIEW_LINES} lines)\n",
        meta.len()
    );
    out.push_str(&format_numbered_read(1, &head.join("\n")));
    let tail = read_tail_lines(path, 5)?;
    out.push_str(&format_preview_tail(&tail, exact_total));
    out.push_str(&format!(
        "[Full file exceeds the read limit ({max_bytes} bytes). Use 'path:start-end' to read a line range, e.g. '{}:1-{FULL_READ_PREVIEW_LINES}'.]",
        path.display()
    ));
    Ok(Some(out))
}

pub fn read(path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    if path.is_empty() {
        bail!("Error: no path provided");
    }

    // Fast path: small file or full read — use read_to_string directly.
    let meta =
        std::fs::metadata(path).map_err(|_| anyhow!(file_not_found_suggestion(Path::new(path))))?;

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
pub(crate) fn inline_diff(path: &str, old: &str, new: &str) -> Result<(String, usize, usize)> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadTargetClass {
    Filesystem {
        backend: FilesystemBackend,
        raw: bool,
    },
    RegisteredResource,
}

pub(crate) fn classify_read_selection(
    selection: &crate::resources::selector::ReadPathSelection,
    router: &crate::resources::ResourceRouter,
    filesystem_backend: FilesystemBackend,
) -> anyhow::Result<ReadTargetClass> {
    if router.can_handle(&selection.path) {
        return Ok(ReadTargetClass::RegisteredResource);
    }
    if router.is_url_like(&selection.path) {
        let scheme = selection
            .path
            .split_once("://")
            .map_or("", |(scheme, _)| scheme);
        anyhow::bail!("unknown resource scheme: {scheme}");
    }
    Ok(ReadTargetClass::Filesystem {
        backend: filesystem_backend,
        raw: selection.raw,
    })
}

pub(crate) fn classify_read_target(
    input: &serde_json::Value,
    router: &crate::resources::ResourceRouter,
    filesystem_backend: FilesystemBackend,
) -> anyhow::Result<ReadTargetClass> {
    let path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("tool input is missing a string path"))?;
    let selection = split_read_path_selection(path)?;
    classify_read_selection(&selection, router, filesystem_backend)
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
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let selection = split_read_path_selection(&args.path)?;
        let filesystem_backend = if ctx.read_only_fs.is_some() {
            FilesystemBackend::ReadOnlyVfs
        } else {
            FilesystemBackend::Local
        };
        let target_class =
            classify_read_selection(&selection, &ctx.resource_router, filesystem_backend)?;
        if target_class == ReadTargetClass::RegisteredResource {
            let resource = ctx.resource_router.resolve(&selection, ctx)?;
            let text = select_text_lines(
                &resource.content,
                selection.offset,
                selection.limit,
                &selection.path,
            )?;
            return Ok(super::runner::ToolOutcome::text(text));
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
                let shown = result.content.lines().take(FULL_READ_PREVIEW_LINES).count();
                let total_desc = if result.total_lines > 0 {
                    format!("{} lines", result.total_lines)
                } else {
                    format!("more than {} lines", FULL_READ_PREVIEW_LINES)
                };
                let mut preview = format!(
                    "(file too large: {} bytes / {total_desc}; showing first {shown} lines)\n",
                    result.total_bytes
                );
                for (idx, line) in result
                    .content
                    .lines()
                    .take(FULL_READ_PREVIEW_LINES)
                    .enumerate()
                {
                    preview.push_str(&format!("{}:{}\n", idx + 1, line));
                }
                // Tail portion from the backend content (only when the content
                // actually exceeds the head window, so small results do not
                // duplicate their own head).
                let all_lines = result.content.lines().collect::<Vec<_>>();
                if all_lines.len() > FULL_READ_PREVIEW_LINES {
                    let tail = all_lines
                        .into_iter()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>();
                    let tail_owned = tail.into_iter().map(str::to_string).collect::<Vec<_>>();
                    preview.push_str(&format_preview_tail(&tail_owned, None));
                }
                preview.push_str(&format!(
                    "[Full file exceeds the read limit ({} bytes). Use 'path:start-end' to read a line range, e.g. '{}:1-{}'.]",
                    ctx.tool_config.tool_result_max_bytes,
                    selection.path,
                    FULL_READ_PREVIEW_LINES
                ));
                return Ok(super::runner::ToolOutcome::text(preview));
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
        if selection.offset.is_none()
            && selection.limit.is_none()
            && let Some(preview) = full_read_preview(&path, ctx.tool_config.tool_result_max_bytes)?
        {
            return Ok(super::runner::ToolOutcome::text(preview));
        }
        if let Some(cached) = ctx.memo_hit(
            &path,
            selection.raw,
            selection.offset,
            selection
                .offset
                .zip(selection.limit)
                .map(|(start, count)| start.saturating_add(count.saturating_sub(1))),
        ) {
            return Ok(super::runner::ToolOutcome::text(cached));
        }
        let start_line = selection.offset.unwrap_or(1);
        let editable_limit = 4 * 1024 * 1024usize;
        let full_text = if ctx.tool_config.edit_mode == crate::config::EditMode::Hashline
            && std::fs::metadata(&path)?.len() as u128
                <= editable_limit.min(ctx.tool_config.file_write_max_bytes) as u128
        {
            Some(
                std::fs::read_to_string(&path)
                    .map_err(|error| anyhow!("Error: cannot read {}: {error}", path.display()))?,
            )
        } else {
            None
        };
        let content = full_text.as_ref().map_or_else(
            || {
                read(
                    &path.display().to_string(),
                    selection.offset,
                    selection.limit,
                )
            },
            |text| select_text_lines(text, selection.offset, selection.limit, &selection.path),
        )?;
        let visible_count = crate::tools::snapshot::split_content_lines(&content).len();
        if selection.raw {
            // Size-guard raw output in every mode (Replace or beyond the
            // editable limit has no full_text snapshot path): a raw read whose
            // output would be truncated must not seed the memo.
            ensure!(
                content.len() <= ctx.tool_config.tool_result_max_bytes,
                "Error: selected Read output is too large ({} bytes > {} bytes); request a narrower line range",
                content.len(),
                ctx.tool_config.tool_result_max_bytes
            );
            // raw 读取不记录可编辑 snapshot（文档协议：raw 不生成锚点），
            // 也不满足 Hashline 的 seen-lines 守卫。
            // Offer the memo candidate; the runner records it only when the
            // final composed output (raw content plus its summary line) fits
            // the budget, so a later hit can never ask the model to "reuse"
            // truncated content. Raw reads stay under the raw flag and never
            // satisfy non-raw requests.
            let mut outcome = super::runner::ToolOutcome::text(content);
            outcome.memo_candidate = Some(super::runner::MemoCandidate {
                path: path.clone(),
                raw: true,
                start_line: selection.offset,
                end_line: memo_end_line(selection.offset, selection.limit, visible_count),
            });
            return Ok(outcome);
        }
        let rendered = match full_text {
            Some(full_text) => {
                let tag = crate::tools::snapshot::compute_file_tag(&full_text);
                let rendered = format_hashline_read(&selection.path, &tag, start_line, &content);
                ensure!(
                    rendered.len() <= ctx.tool_config.tool_result_max_bytes,
                    "Error: selected Hashline Read output is too large ({} bytes > {} bytes); request a narrower line range",
                    rendered.len(),
                    ctx.tool_config.tool_result_max_bytes
                );
                let snapshot = ctx
                    .snapshots
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .record(&path, &full_text, start_line..start_line + visible_count);
                debug_assert_eq!(snapshot.tag, tag);
                rendered
            }
            None => {
                // Replace 模式（或超限文件）无 hashline 快照语义，但仍记录回滚基线。
                if let Ok(meta) = std::fs::metadata(&path)
                    && meta.len() as u128
                        <= editable_limit.min(ctx.tool_config.file_write_max_bytes) as u128
                    && let Ok(full) = std::fs::read_to_string(&path)
                {
                    ctx.snapshots
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .record(&path, &full, start_line..start_line + visible_count);
                }
                format_numbered_read(start_line, &content)
            }
        };
        // Offer the memo candidate; the runner records it only when the final
        // composed output (rendered content plus its summary line) fits the
        // budget, so a later hit can never ask the model to "reuse" content
        // that was truncated or spilled at the runner layer.
        let mut outcome = super::runner::ToolOutcome::text(rendered);
        outcome.memo_candidate = Some(super::runner::MemoCandidate {
            path: path.clone(),
            raw: false,
            start_line: selection.offset,
            end_line: memo_end_line(selection.offset, selection.limit, visible_count),
        });
        Ok(outcome)
    }
}

/// memo 端行语义：开放式选择器（读到 EOF）记 `None`，与 `covers()` 的
/// `(Some(N), None)` 查询对齐，使重复的 `path:N` 开放式读取可以命中；
/// 有界选择器记实际末行。
fn memo_end_line(
    offset: Option<usize>,
    limit: Option<usize>,
    visible_count: usize,
) -> Option<usize> {
    limit?;
    offset.map(|start| start + visible_count.saturating_sub(1))
}

impl super::runner::ToolExec for WriteTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Write",
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
        #[serde(deny_unknown_fields)]
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
        if ctx.tool_config.edit_mode == crate::config::EditMode::Hashline
            && args.content.len()
                <= (4 * 1024 * 1024usize).min(ctx.tool_config.file_write_max_bytes)
        {
            let line_count = crate::tools::snapshot::split_content_lines(&args.content).len();
            let snapshot = ctx
                .snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(&path, &args.content, 1..=line_count);
            return Ok(super::runner::ToolOutcome::text(format!(
                "{result}\n[{}#{}]",
                args.path, snapshot.tag
            )));
        }
        Ok(super::runner::ToolOutcome::text(result))
    }
}

impl super::runner::ToolExec for EditTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Edit",
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
        match ctx.tool_config.edit_mode {
            crate::config::EditMode::Hashline => execute_hashline_edit(input, ctx),
            crate::config::EditMode::Replace => execute_replace_edit(input, ctx),
        }
    }
}

fn format_hashline_read(display_path: &str, tag: &str, start_line: usize, content: &str) -> String {
    let mut out = format!("[{display_path}#{tag}]");
    for (idx, line) in crate::tools::snapshot::split_content_lines(content)
        .iter()
        .enumerate()
    {
        out.push('\n');
        out.push_str(&format!("{}:{line}", start_line + idx));
    }
    out
}

fn format_numbered_read(start_line: usize, content: &str) -> String {
    let mut out = String::new();
    for (idx, line) in crate::tools::snapshot::split_content_lines(content)
        .iter()
        .enumerate()
    {
        if !out.is_empty() {
            out.push('\n');
        }
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

#[derive(Debug, Clone)]
struct TextShape {
    bom: bool,
    crlf: bool,
}

fn decode_text_shape(raw: &str) -> (TextShape, String) {
    let bom = raw.starts_with('\u{feff}');
    let without_bom = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    // 仅当全部换行都是 CRLF 才按 CRLF 恢复：此前只看第一个换行，
    // 混合行尾文件（如 "a\r\nb\nc"）会被整体改写成 CRLF。混合文件
    // 统一按 LF 处理（少数 CRLF 行归一为 LF，不做全文件 EOL 改写）。
    let bytes = without_bom.as_bytes();
    let crlf = bytes
        .iter()
        .enumerate()
        .all(|(i, b)| *b != b'\n' || (i > 0 && bytes[i - 1] == b'\r'));
    (
        TextShape { bom, crlf },
        crate::tools::snapshot::normalize_snapshot_text(without_bom),
    )
}

fn restore_text_shape(shape: &TextShape, normalized: &str) -> String {
    let text = if shape.crlf {
        normalized.replace('\n', "\r\n")
    } else {
        normalized.to_string()
    };
    if shape.bom {
        format!("\u{feff}{text}")
    } else {
        text
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temporary = parent.join(format!(".{name}.mink-edit-{}-{stamp}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Ok(metadata) = std::fs::metadata(path)
        && let Err(error) = file.set_permissions(metadata.permissions())
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn move_file_noclobber(source: &Path, destination: &Path, replacement: Option<&str>) -> Result<()> {
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    ensure!(
        !destination.exists(),
        "MV destination already exists: {}",
        destination.display()
    );

    if let Some(content) = replacement {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let temporary = parent.join(format!(".{name}.mink-move-{}-{stamp}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let operation = (|| -> Result<()> {
            if let Ok(metadata) = std::fs::metadata(source) {
                file.set_permissions(metadata.permissions())?;
            }
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            // hard_link is the portable no-clobber publication primitive used
            // here: it fails if destination appeared after preflight.
            std::fs::hard_link(&temporary, destination)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        operation?;
    } else {
        std::fs::hard_link(source, destination)?;
    }

    if let Err(error) = std::fs::remove_file(source) {
        let _ = std::fs::remove_file(destination);
        return Err(error.into());
    }
    Ok(())
}

#[derive(Debug)]
enum HashlineAction {
    Write {
        updated: String,
        shape: TextShape,
    },
    Remove,
    Move {
        destination: PathBuf,
        updated: String,
        shape: TextShape,
    },
}

#[derive(Debug)]
struct PreparedHashline {
    path: PathBuf,
    display_path: String,
    normalized_original: String,
    action: HashlineAction,
    warnings: Vec<String>,
    clipboard_after: crate::tools::hashline::Clipboard,
}

fn execute_hashline_edit(
    input: &serde_json::Value,
    ctx: &crate::context::ToolContext,
) -> Result<super::runner::ToolOutcome> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        input: String,
    }
    let args: Args = serde_json::from_value(input.clone()).map_err(|error| {
        anyhow!(
            "Error: Hashline Edit accepts only {{\"input\": \"[PATH#TAG]...\"}}; legacy path/patch and old_string/new_string inputs are unsupported: {error}"
        )
    })?;
    let patch = crate::tools::hashline::parse(&args.input)?;
    let mut store = ctx
        .snapshots
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut clipboard =
        crate::tools::hashline::Clipboard::with_named(store.named_clipboard().clone());
    let mut prepared = Vec::new();
    let mut canonical_targets = BTreeSet::new();
    let mut move_destinations = BTreeSet::new();

    for authored in &patch.sections {
        let authored_path = resolve_tool_path(&ctx.cwd, &authored.path)?;
        let (path, recovered_path) = if authored_path.exists() {
            (authored_path, None)
        } else {
            let file_name = authored_path
                .file_name()
                .ok_or_else(|| anyhow!("Error: {} has no recoverable filename", authored.path))?;
            let recovered = store
                .unique_path_for_tag_and_name(&authored.tag, file_name, &authored_path)
                .map_err(|message| {
                    anyhow!("Error: {} does not exist and {message}", authored.path)
                })?;
            (recovered, Some(authored.path.clone()))
        };
        let canonical = crate::tools::snapshot::canonical_snapshot_path(&path);
        ensure!(
            canonical_targets.insert(canonical.clone()),
            "Error: multiple hashline sections resolve to the same canonical path {}",
            canonical.display()
        );
        store.begin_noop_attempt(&canonical, &args.input);
        let original = std::fs::read_to_string(&canonical)
            .map_err(|error| anyhow!("Error: cannot read {}: {error}", canonical.display()))?;
        ensure!(
            original.len() <= ctx.tool_config.file_write_max_bytes,
            "Error: file too large for Edit ({} bytes > {} bytes): {}",
            original.len(),
            ctx.tool_config.file_write_max_bytes,
            canonical.display()
        );
        let (shape, normalized) = decode_text_shape(&original);
        let current_tag = crate::tools::snapshot::compute_file_tag(&normalized);
        let authored_anchors = crate::tools::hashline::anchor_lines(authored);
        let versions = store.versions(&canonical, &authored.tag);
        if versions.is_empty() {
            let context = format_mismatch_anchor_context(&normalized, &authored_anchors);
            bail!(
                "Error: snapshot tag #{} for {} is unknown and does not belong to this session. Do not invent tags or reuse a tag from another session.\nCurrent content hash (diagnostic only): #{}. This hash is not an authorized snapshot tag and must not be used to retry.\nUse Read or Grep to obtain a verifiable new [PATH#TAG] header before editing again.{}",
                authored.tag,
                authored.path,
                current_tag,
                context
            );
        }

        let mut warnings = Vec::new();
        if let Some(authored_path) = recovered_path {
            warnings.push(format!(
                "path {authored_path:?} does not exist; matched its filename and snapshot tag #{} to {}",
                authored.tag,
                display_relative_path(&ctx.cwd, &canonical)
            ));
        }
        let (section, snapshot) = if current_tag.eq_ignore_ascii_case(&authored.tag) {
            let snapshot = versions
                .iter()
                .find(|snapshot| {
                    snapshot.text == normalized
                        || hash_equivalent_snapshot_text(&snapshot.text, &normalized)
                })
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "Error: tag #{} collides with unobserved content for {}; re-read the file",
                        authored.tag,
                        authored.path
                    )
                })?;
            (authored.clone(), snapshot)
        } else if crate::tools::hashline::is_head_tail_only(authored) {
            warnings.push(format!(
                "stale snapshot #{}: HEAD/TAIL-only operations were applied to the current file",
                authored.tag
            ));
            (authored.clone(), versions[0].clone())
        } else {
            let mut recovered = Vec::new();
            for snapshot in versions {
                if let Ok(offset) =
                    recover_uniform_offset(&snapshot.text, &normalized, &authored_anchors)
                    && let Ok(section) = crate::tools::hashline::remap_anchors(authored, offset)
                {
                    recovered.push((section, snapshot, offset));
                }
            }
            if recovered.len() != 1 {
                let context = format_mismatch_anchor_context(&normalized, &authored_anchors);
                let guidance = if store.is_edit_result_tag(&canonical, &current_tag) {
                    format!(
                        "The current content matches [{}#{}], a header returned by an earlier successful Edit in this session; that exact response header may be reused directly.",
                        authored.path, current_tag
                    )
                } else {
                    let current_hint = store
                        .latest_tag(&canonical)
                        .filter(|tag| !tag.eq_ignore_ascii_case(&current_tag))
                        .map(|tag| {
                            format!(
                                "\nThe last known snapshot [{}#{tag}] predates the current content (the file drifted after that snapshot) and cannot be reused; editing against it would fail again. Re-Read the file and use the newly returned header.",
                                authored.path
                            )
                        })
                        .unwrap_or_default();
                    format!(
                        "The file has drifted outside a successful Edit response. Re-run Read or Grep and use the newly returned header; do not retry with the diagnostic hash below.{current_hint}"
                    )
                };
                bail!(
                    "Error: stale snapshot #{} for {} could not be recovered unambiguously. An anchor changed, was deleted/split, repeated, or mapped with an inconsistent offset.\nCurrent content hash (diagnostic): #{}. {}{}",
                    authored.tag,
                    authored.path,
                    current_tag,
                    guidance,
                    context
                );
            }
            let (section, snapshot, offset) = recovered.remove(0);
            warnings.push(format!(
                "recovered stale snapshot #{} with a uniform {offset:+} line offset",
                authored.tag
            ));
            (section, snapshot)
        };

        if ctx.tool_config.edit_enforce_seen_lines {
            let missing = authored_anchors
                .difference(&snapshot.seen_lines)
                .copied()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                let lines = crate::tools::snapshot::split_content_lines(&snapshot.text);
                let preview = missing
                    .iter()
                    .take(200)
                    .filter_map(|line| lines.get(line - 1).map(|text| format!("{line}:{text}")))
                    .collect::<Vec<_>>()
                    .join("\n");
                let message = format!(
                    "Error: hashline anchors were not shown by Read/Grep for {}: {}{}{}",
                    authored.path,
                    missing
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    if preview.is_empty() { "" } else { "\n" },
                    preview
                );
                let fully_echoed = missing.len() <= 200
                    && missing.iter().all(|line| {
                        lines
                            .get(line.saturating_sub(1))
                            .is_some_and(|text| text.len() <= 500)
                    })
                    && message.len() <= ctx.tool_config.tool_result_max_bytes;
                if fully_echoed {
                    store.add_seen_lines(
                        &canonical,
                        &snapshot.tag,
                        &snapshot.text,
                        missing.iter().copied(),
                    );
                }
                bail!(message);
            }
        }

        let applied = crate::tools::hashline::apply(&normalized, &section, &mut clipboard)?;
        warnings.extend(applied.warnings);
        let updated = applied.text;
        let file_operation = section
            .operations
            .last()
            .and_then(|operation| match operation {
                crate::tools::hashline::Operation::Remove => Some((None, true)),
                crate::tools::hashline::Operation::Move { destination } => {
                    Some((Some(destination.as_str()), false))
                }
                _ => None,
            });
        let action = match file_operation {
            Some((None, true)) => {
                ensure!(
                    current_tag.eq_ignore_ascii_case(&authored.tag),
                    "Error: REM refuses a stale file tag; re-read {}",
                    authored.path
                );
                HashlineAction::Remove
            }
            Some((Some(destination), false)) => {
                let destination = resolve_tool_path(&ctx.cwd, destination)?;
                let destination = crate::tools::snapshot::canonical_snapshot_path(&destination);
                ensure!(
                    !destination.exists(),
                    "Error: MV destination already exists: {}",
                    destination.display()
                );
                ensure!(
                    move_destinations.insert(destination.clone()),
                    "Error: multiple Hashline sections move to the same destination {}",
                    destination.display()
                );
                ensure!(
                    !canonical_targets.contains(&destination),
                    "Error: MV destination is also a source path in this Hashline batch: {}",
                    destination.display()
                );
                let final_text = restore_text_shape(&shape, &updated);
                ensure!(
                    final_text.len() <= ctx.tool_config.file_write_max_bytes,
                    "Error: moved file would exceed file_write_max_bytes"
                );
                HashlineAction::Move {
                    destination,
                    updated,
                    shape,
                }
            }
            _ => {
                if updated == normalized {
                    if patch.sections.len() != 1 {
                        bail!(
                            "Error: Hashline batch preflight found a no-op section for {}; no files were committed. Remove or change the no-op section before retrying.",
                            authored.path
                        );
                    }
                    // The requested final content is already present at the exact target positions.
                    if crate::tools::hashline::already_applied(
                        &normalized,
                        &patch.sections[0].operations,
                    ) {
                        // Idempotent success writes nothing: keep the
                        // mutation epoch untouched so read memos stay valid
                        // (mirrors the Replace-mode idempotent path).
                        let mut outcome = tool_outcome(format!(
                            "Edit({}): already applied (idempotent)\nThe requested final content is already present; no change was made.",
                            authored.path
                        ));
                        outcome.no_mutation = true;
                        return Ok(outcome);
                    }
                    let count = store.note_noop(&canonical, &args.input);
                    if count >= 3 {
                        bail!(
                            "Error: identical Hashline payload produced no changes three consecutive times for {}. The same payload will continue to fail; change the operation or re-check the file and its anchors before retrying.",
                            authored.path
                        );
                    }
                    // Soft no-op writes nothing either: only the no-op
                    // counter changes, never the mutation epoch.
                    let mut outcome = tool_outcome(format!(
                        "Edit({}): no changes (soft no-op {count}/2)\nNo file or clipboard state was changed. Do not expand the edit range. Confirm whether the intended change is already present and re-check the target anchors before retrying.",
                        authored.path
                    ));
                    outcome.no_mutation = true;
                    return Ok(outcome);
                }
                let final_text = restore_text_shape(&shape, &updated);
                ensure!(
                    final_text.len() <= ctx.tool_config.file_write_max_bytes,
                    "Error: edited file would exceed file_write_max_bytes"
                );
                HashlineAction::Write { updated, shape }
            }
        };
        prepared.push(PreparedHashline {
            path: canonical,
            display_path: authored.path.clone(),
            normalized_original: normalized,
            action,
            warnings,
            clipboard_after: clipboard.clone(),
        });
    }

    let section_names = prepared
        .iter()
        .map(|item| item.display_path.clone())
        .collect::<Vec<_>>();
    let mut rendered = Vec::new();
    let mut committed = Vec::new();
    for (index, item) in prepared.into_iter().enumerate() {
        let result: Result<String> = (|| match item.action {
            HashlineAction::Write { updated, shape } => {
                let final_text = restore_text_shape(&shape, &updated);
                atomic_write(&item.path, &final_text)?;
                let (diff, added, removed) =
                    inline_diff(&item.display_path, &item.normalized_original, &updated)?;
                let (first_changed, last_changed) =
                    changed_line_window(&item.normalized_original, &updated);
                let lines = crate::tools::snapshot::split_content_lines(&updated);
                let start = first_changed.saturating_sub(12).max(1);
                let end = (last_changed + 12)
                    .min(lines.len())
                    .max(start.min(lines.len()));
                let visible = if lines.is_empty() {
                    1..1
                } else {
                    start..end.saturating_add(1)
                };
                let snapshot = store.record_edit(&item.path, &updated, visible.clone());
                store.reset_noop(&item.path);
                let body = if lines.is_empty() || start > end {
                    format!("[{}#{}]", item.display_path, snapshot.tag)
                } else {
                    format_hashline_read(
                        &item.display_path,
                        &snapshot.tag,
                        start,
                        &lines[start - 1..end].join("\n"),
                    )
                };
                Ok(format!(
                    "Edit({}): updated\n{}\nfirstChangedLine: {}\nlinesAdded: {}\nlinesRemoved: {}{}\nDiff:\n{}",
                    item.display_path,
                    body,
                    first_changed,
                    added,
                    removed,
                    format_warnings(&item.warnings),
                    diff
                ))
            }
            HashlineAction::Remove => {
                let (diff, added, removed) =
                    inline_diff(&item.display_path, &item.normalized_original, "")?;
                std::fs::remove_file(&item.path)?;
                store.reset_noop(&item.path);
                Ok(format!(
                    "Edit({}): removed\nfirstChangedLine: 1\nlinesAdded: {}\nlinesRemoved: {}{}\nDiff:\n{}",
                    item.display_path,
                    added,
                    removed,
                    format_warnings(&item.warnings),
                    diff
                ))
            }
            HashlineAction::Move {
                destination,
                updated,
                shape,
            } => {
                let final_text = restore_text_shape(&shape, &updated);
                let replacement =
                    (updated != item.normalized_original).then_some(final_text.as_str());
                move_file_noclobber(&item.path, &destination, replacement)?;
                store.relocate(&item.path, &destination);
                let display = display_relative_path(&ctx.cwd, &destination);
                let (diff, added, removed) =
                    inline_diff(&item.display_path, &item.normalized_original, &updated)?;
                let changed = (updated != item.normalized_original)
                    .then(|| changed_line_window(&item.normalized_original, &updated).0);
                let lines = crate::tools::snapshot::split_content_lines(&updated);
                let (start, end) = changed.map_or_else(
                    || (1, lines.len().min(25)),
                    |line| (line.saturating_sub(12).max(1), (line + 12).min(lines.len())),
                );
                let visible = if lines.is_empty() || start > end {
                    1..1
                } else {
                    start..end.saturating_add(1)
                };
                let snapshot = store.record_edit(&destination, &updated, visible);
                store.reset_noop(&destination);
                let body = if lines.is_empty() || start > end {
                    format!("[{display}#{}]", snapshot.tag)
                } else {
                    format_hashline_read(
                        &display,
                        &snapshot.tag,
                        start,
                        &lines[start - 1..end].join("\n"),
                    )
                };
                Ok(format!(
                    "Edit({}): moved -> {}\n{}\nfirstChangedLine: {}\nlinesAdded: {}\nlinesRemoved: {}{}\nDiff:\n{}",
                    item.display_path,
                    display,
                    body,
                    changed.map_or_else(|| "none".to_string(), |line| line.to_string()),
                    added,
                    removed,
                    format_warnings(&item.warnings),
                    if diff.is_empty() {
                        "(no content changes; path moved)"
                    } else {
                        &diff
                    }
                ))
            }
        })();
        match result {
            Ok(text) => {
                committed.push(item.display_path.clone());
                store.set_named_clipboard(item.clipboard_after.named().clone());
                rendered.push(text);
            }
            Err(error) => {
                let uncommitted = section_names[index + 1..].join(", ");
                bail!(
                    "Error: multi-file Hashline commit stopped; committed [{}]; failed {}: {error}; not committed [{}]",
                    committed.join(", "),
                    item.display_path,
                    uncommitted
                );
            }
        }
    }
    let content = rendered.join("\n\n");
    Ok(tool_outcome(content))
}

fn hash_equivalent_snapshot_text(left: &str, right: &str) -> bool {
    left.split('\n')
        .map(|line| line.trim_end_matches([' ', '\t', '\r']))
        .eq(right
            .split('\n')
            .map(|line| line.trim_end_matches([' ', '\t', '\r'])))
}

fn recover_uniform_offset(old: &str, current: &str, anchors: &BTreeSet<usize>) -> Result<isize> {
    ensure!(
        !anchors.is_empty(),
        "no content anchors are available for stale recovery"
    );
    let diff = TextDiff::from_lines(old, current);
    let old_lines = old.split('\n').collect::<Vec<_>>();
    let current_lines = current.split('\n').collect::<Vec<_>>();
    let mut line_map = vec![None; old_lines.len()];
    for operation in diff
        .ops()
        .iter()
        .filter(|operation| operation.tag() == similar::DiffTag::Equal)
    {
        for offset in 0..operation.old_range().len() {
            line_map[operation.old_range().start + offset] =
                Some(operation.new_range().start + offset);
        }
    }

    let duplicated_old = duplicated_line_values(&old_lines);
    let duplicated_current = duplicated_line_values(&current_lines);
    let sorted = anchors.iter().copied().collect::<Vec<_>>();
    let mut offsets = BTreeSet::new();
    let mut run_start = 0usize;
    while run_start < sorted.len() {
        let mut run_end = run_start;
        while run_end + 1 < sorted.len() && sorted[run_end + 1] == sorted[run_end] + 1 {
            run_end += 1;
        }
        let first = sorted[run_start];
        let last = sorted[run_end];
        let before = first.checked_sub(1).filter(|line| *line >= 1);
        let after = (last < old_lines.len()).then_some(last + 1);
        for anchor in &sorted[run_start..=run_end] {
            let old_index = anchor - 1;
            let mapped = line_map
                .get(old_index)
                .and_then(|mapped| *mapped)
                .ok_or_else(|| anyhow!("anchor line {anchor} changed or was deleted"))?;
            let offset = mapped as isize - old_index as isize;
            offsets.insert(offset);

            let context_matches = |line: usize| {
                let index = line - 1;
                line_map.get(index).and_then(|mapped| *mapped) == index.checked_add_signed(offset)
            };
            let duplicate = duplicated_old.contains(old_lines[old_index])
                || duplicated_current.contains(current_lines[mapped]);
            let context_valid = if duplicate {
                let mut checked = false;
                let mut valid = true;
                if let Some(line) = before {
                    checked = true;
                    valid &= context_matches(line);
                }
                if let Some(line) = after {
                    checked = true;
                    valid &= context_matches(line);
                }
                checked && valid
            } else {
                after.is_some_and(context_matches) || before.is_some_and(context_matches)
            };
            ensure!(
                context_valid,
                "anchor context at line {anchor} is changed or ambiguous"
            );
        }
        run_start = run_end + 1;
    }
    ensure!(offsets.len() == 1, "anchors do not share one line offset");
    Ok(*offsets.first().expect("one offset"))
}

fn duplicated_line_values<'a>(lines: &'a [&'a str]) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::new();
    let mut duplicated = BTreeSet::new();
    for line in lines {
        if !seen.insert(*line) {
            duplicated.insert(*line);
        }
    }
    duplicated
}

fn format_warnings(warnings: &[String]) -> String {
    if warnings.is_empty() {
        String::new()
    } else {
        format!("\nWarnings:\n{}", warnings.join("\n"))
    }
}

fn changed_line_window(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut start = usize::MAX;
    let mut end = 1;
    for operation in diff
        .ops()
        .iter()
        .filter(|operation| operation.tag() != similar::DiffTag::Equal)
    {
        start = start.min(operation.new_range().start + 1);
        end = end.max(
            operation
                .new_range()
                .end
                .max(operation.new_range().start + 1),
        );
    }
    if start == usize::MAX {
        (1, 1)
    } else {
        (start, end)
    }
}

const MISMATCH_CONTEXT_RADIUS: usize = 2;
const MISMATCH_CONTEXT_MAX_LINES: usize = 40;
const MISMATCH_CONTEXT_MAX_LINE_BYTES: usize = 500;

fn format_mismatch_anchor_context(content: &str, anchors: &BTreeSet<usize>) -> String {
    if anchors.is_empty() {
        return "\nAnchor context: unavailable (the section has no numbered anchors).".to_string();
    }

    let lines = crate::tools::snapshot::split_content_lines(content);
    if lines.is_empty() {
        return "\nAnchor context from current content: (file is empty).".to_string();
    }

    let mut requested = BTreeSet::new();
    let mut out_of_range = false;
    for anchor in anchors {
        if *anchor == 0 || *anchor > lines.len() {
            out_of_range = true;
            continue;
        }
        let start = anchor.saturating_sub(MISMATCH_CONTEXT_RADIUS).max(1);
        let end = anchor
            .saturating_add(MISMATCH_CONTEXT_RADIUS)
            .min(lines.len());
        requested.extend(start..=end);
    }

    if requested.is_empty() {
        return format!(
            "\nAnchor context from current content: unavailable (requested anchors are outside 1..={}).",
            lines.len()
        );
    }

    let total_requested = requested.len();
    let selected = requested
        .into_iter()
        .take(MISMATCH_CONTEXT_MAX_LINES)
        .collect::<Vec<_>>();
    let mut truncated = total_requested > selected.len() || out_of_range;
    let mut rendered = String::from(
        "\nAnchor context from current content (diagnostic only; this does not mark lines as seen or authorize a tag):",
    );
    let mut previous = None;
    for line_number in selected {
        if previous.is_some_and(|line| line + 1 != line_number) {
            rendered.push_str("\n  …");
        }
        let source = &lines[line_number - 1];
        let (text, line_truncated) = utf8_bounded_line(source, MISMATCH_CONTEXT_MAX_LINE_BYTES);
        truncated |= line_truncated;
        rendered.push('\n');
        rendered.push_str(if anchors.contains(&line_number) {
            "* "
        } else {
            "  "
        });
        rendered.push_str(&line_number.to_string());
        rendered.push(':');
        rendered.push_str(text);
        if line_truncated {
            rendered.push_str(" … [line truncated]");
        }
        previous = Some(line_number);
    }
    if truncated {
        rendered.push_str(
            "\n[Anchor context truncated to safety limits; use Read/Grep for complete current context.]",
        );
    }
    rendered
}

fn utf8_bounded_line(line: &str, max_bytes: usize) -> (&str, bool) {
    if line.len() <= max_bytes {
        return (line, false);
    }
    let mut end = max_bytes;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    (&line[..end], true)
}

fn display_relative_path(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn execute_replace_edit(
    input: &serde_json::Value,
    ctx: &crate::context::ToolContext,
) -> Result<super::runner::ToolOutcome> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        path: String,
        edits: Vec<crate::tools::replace::ReplaceEntry>,
    }
    let args: Args = serde_json::from_value(input.clone()).map_err(|error| {
        anyhow!("Error: Replace Edit requires path and edits with old_text/new_text/all; legacy patch inputs are unsupported: {error}")
    })?;
    ensure!(
        !args.edits.is_empty(),
        "Error: edits must contain at least one entry"
    );
    let path = resolve_replace_target(&ctx.cwd, &args.path)?;
    let display_path = display_relative_path(&ctx.cwd, &path);
    let mut output = Vec::new();
    let mut wrote = false;
    for (index, edit) in args.edits.iter().enumerate() {
        let original = std::fs::read_to_string(&path)
            .map_err(|error| anyhow!("Error: cannot read {}: {error}", path.display()))?;
        ensure!(
            original.len() <= ctx.tool_config.file_write_max_bytes,
            "Error: file too large for Edit"
        );
        let (shape, normalized) = decode_text_shape(&original);
        let old_text = crate::tools::snapshot::normalize_snapshot_text(&edit.old_text);
        let new_text = crate::tools::snapshot::normalize_snapshot_text(&edit.new_text);
        let result = crate::tools::replace::replace_text(
            &normalized,
            &old_text,
            &new_text,
            edit.all,
            ctx.tool_config.edit_fuzzy_match,
            ctx.tool_config.edit_fuzzy_threshold,
            &display_path,
        )
        .map_err(|error| {
            anyhow!(
                "{}{}",
                error,
                if index > 0 {
                    format!("\n{index} earlier edit(s) in this call were already committed")
                } else {
                    String::new()
                }
            )
        })?;
        // Idempotent edits change nothing on disk: skip the write, the diff
        // and the mutation-epoch bump so mtime/inode stay stable and read
        // memos remain valid.
        if result.strategy == "idempotent" {
            output.push(format!(
                "Edit({}).{}: already applied (idempotent)\nThe requested final content is already present; no change was made.",
                display_path,
                index + 1
            ));
            continue;
        }
        let final_text = restore_text_shape(&shape, &result.content);
        ensure!(
            final_text.len() <= ctx.tool_config.file_write_max_bytes,
            "Error: edited file would exceed file_write_max_bytes"
        );
        atomic_write(&path, &final_text)?;
        wrote = true;
        let (diff, added, removed) = inline_diff(&display_path, &normalized, &result.content)?;
        let first_changed = changed_line_window(&normalized, &result.content).0;
        output.push(format!(
            "Edit({}).{}: updated\nfirstChangedLine: {}\nmatchStrategy: {}\nmatchCount: {}\nlinesAdded: {}\nlinesRemoved: {}\nDiff:\n{}",
            display_path,
            index + 1,
            first_changed,
            result.strategy,
            result.count,
            added,
            removed,
            diff
        ));
    }
    let mut outcome = tool_outcome(output.join("\n\n"));
    outcome.no_mutation = !wrote;
    Ok(outcome)
}

fn resolve_replace_target(cwd: &Path, authored: &str) -> Result<PathBuf> {
    let direct = resolve_tool_path(cwd, authored)?;
    if direct.is_file() {
        return Ok(crate::tools::snapshot::canonical_snapshot_path(&direct));
    }
    let suffix = Path::new(authored)
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_owned()),
            _ => None,
        })
        .collect::<PathBuf>();
    ensure!(
        !suffix.as_os_str().is_empty(),
        "Error: file not found: {authored}"
    );
    let mut candidates = ignore::WalkBuilder::new(cwd)
        .standard_filters(true)
        .parents(false)
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| path.ends_with(&suffix))
        .take(6)
        .collect::<Vec<_>>();
    candidates.sort();
    match candidates.as_slice() {
        [path] => Ok(crate::tools::snapshot::canonical_snapshot_path(path)),
        [] => bail!("Error: file not found: {authored}"),
        _ => bail!(
            "Error: path suffix {authored:?} is ambiguous: {}",
            candidates
                .iter()
                .map(|path| display_relative_path(cwd, path))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn tool_outcome(content: String) -> super::runner::ToolOutcome {
    super::runner::ToolOutcome {
        conversation_content: String::new(),
        content,
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

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
