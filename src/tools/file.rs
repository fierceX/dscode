use anyhow::{Result, anyhow, bail};
use similar::{ChangeTag, TextDiff};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// Threshold for switching to streaming read (bytes).
const STREAM_READ_THRESHOLD: u64 = 1_048_576; // 1MB

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
        let total_lines = lines.len();
        let start = match offset {
            Some(o) if o > 1 => {
                if o > total_lines {
                    bail!(
                        "Error: offset {} exceeds total lines {} in {}",
                        o,
                        total_lines,
                        path
                    );
                }
                o - 1
            }
            _ => 0,
        };
        let end = match limit {
            Some(l) if l > 0 => (start + l).min(total_lines),
            _ => total_lines,
        };
        return Ok(lines[start..end].join("\n"));
    }

    // Large file + range: stream — scan line boundaries, then read exact byte range.
    let start_line = offset.unwrap_or(1);
    let count = limit.unwrap_or(usize::MAX);

    let mut reader = BufReader::new(std::fs::File::open(path)?);
    // line_offsets[i] = byte offset where line (i+1) starts in the file.
    let mut line_offsets: Vec<u64> = Vec::with_capacity(4096);

    // Scan line boundaries up to what we need.
    // target_line is the last line index we need (0-based, exclusive).
    let target_idx = (start_line.saturating_sub(1) + count) as u64;
    let mut line_idx = 0u64;
    let mut buf = Vec::new();

    loop {
        let pos_before = reader.stream_position()?;
        buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut buf)?;
        if bytes_read == 0 {
            break; // EOF
        }
        line_offsets.push(pos_before);
        line_idx += 1;
        if line_idx == target_idx.saturating_add(1) {
            break; // we have all the offsets we need
        }
    }

    // If file is smaller than requested offset, it's an error.
    if start_line > 1 && line_offsets.len() < start_line - 1 {
        bail!(
            "Error: offset {} exceeds total lines {} in {}",
            start_line,
            line_offsets.len(),
            path
        );
    }

    let idx = (start_line.saturating_sub(1)) as usize;
    let start_byte = *line_offsets.get(idx).unwrap_or(&0);

    // End byte: either the start of the next line after the range, or EOF.
    let end_idx = idx + count.min(line_offsets.len().saturating_sub(idx));
    let end_byte = if end_idx < line_offsets.len() {
        line_offsets[end_idx]
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

    Ok(String::from_utf8(content)?)
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

pub fn edit(path: &str, old_s: &str, new_s: &str, max_bytes: usize) -> Result<String> {
    if path.is_empty() {
        bail!("Error: no path provided");
    }
    if old_s.is_empty() {
        bail!("Error: empty old_string");
    }
    let content =
        std::fs::read_to_string(path).map_err(|_| anyhow!("Error: file not found: {path}"))?;
    if content.len() > max_bytes {
        bail!(
            "Error: file too large for edit_file ({} bytes > {} bytes)",
            content.len(),
            max_bytes
        );
    }
    if !content.contains(old_s) {
        bail!(
            "Error: old_string not found in {path}. Hint: use Grep to locate the target lines, then Read the relevant portion (with offset/limit) to copy the exact text before retrying Edit."
        );
    }
    let updated = content.replacen(old_s, new_s, 1);
    if updated.is_empty() {
        bail!("Error: edit produced empty result, reverted");
    }
    let (diff, added, removed) = inline_diff(path, &content, &updated)?;
    std::fs::write(path, updated)?;
    let summary = format!("Edit({path}) [+{added} -{removed} lines]");
    Ok(format!("{summary}\n{diff}\n"))
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
                if val.ends_with('\n') {
                    let trimmed = &val[..val.len() - 1];
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

fn ensure_workspace_write(cwd: &Path, path: &Path) -> Result<()> {
    let root = cwd
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(cwd));
    let target = if path.exists() {
        path.canonicalize()
            .unwrap_or_else(|_| normalize_lexically(path))
    } else {
        let parent = path.parent().unwrap_or(path);
        let parent_real = parent
            .canonicalize()
            .unwrap_or_else(|_| normalize_lexically(parent));
        let name = path.file_name().map(PathBuf::from).unwrap_or_default();
        parent_real.join(name)
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
    fn name(&self) -> &'static str {
        "Read"
    }
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            #[serde(default)]
            offset: Option<usize>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let path = resolve_tool_path(&ctx.cwd, &args.path)?;
        read(&path.display().to_string(), args.offset, args.limit)
            .map(super::runner::ToolOutcome::text)
    }
}

impl super::runner::ToolExec for WriteTool {
    fn name(&self) -> &'static str {
        "Write"
    }
    fn mutating(&self) -> bool {
        true
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
    fn name(&self) -> &'static str {
        "Edit"
    }
    fn mutating(&self) -> bool {
        true
    }
    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            old_string: String,
            new_string: String,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let path = resolve_tool_path(&ctx.cwd, &args.path)?;
        ensure_workspace_write(&ctx.cwd, &path)?;
        edit(
            &path.display().to_string(),
            &args.old_string,
            &args.new_string,
            ctx.tool_config.file_write_max_bytes,
        )
        .map(|s| super::runner::ToolOutcome {
            conversation_content: s.clone(),
            content: s,
            is_bash: false,
            exit_code: None,
            success: true,
            diagnostics: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_file(name: &str, content: &str) -> String {
        let path = format!("/tmp/mink-test-{}-{}", name, std::process::id());
        fs::write(&path, content).unwrap();
        path
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
    fn read_empty_path_error() {
        assert!(read("", None, None).is_err());
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
    fn edit_replaces_and_returns_diff() {
        let p = temp_file("edit-basic", "prefix old-value suffix\n");
        let result = edit(&p, "old-value", "new-value", 1000).unwrap();
        assert!(result.contains("new-value") || result.contains("+new-value"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "prefix new-value suffix\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn edit_not_found_error() {
        let p = temp_file("edit-nf", "prefix old suffix\n");
        assert!(edit(&p, "missing", "replacement", 1000).is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn edit_empty_old_string_error() {
        let p = temp_file("edit-empty", "text\n");
        assert!(edit(&p, "", "new", 1000).is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn workspace_write_allows_inside_cwd() {
        let cwd = PathBuf::from("/tmp/workspace");
        let path = resolve_tool_path(&cwd, "src/file.txt").unwrap();
        assert!(ensure_workspace_write(&cwd, &path).is_ok());
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
