use anyhow::{Result, anyhow, bail};
use std::path::Path;

pub fn read(path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    if path.is_empty() { bail!("Error: no path provided"); }
    let data = std::fs::read_to_string(path)
        .map_err(|_| anyhow!("Error: file not found or unreadable: {path}"))?;
    if offset.is_none() && limit.is_none() { return Ok(data); }

    let mut lines: Vec<&str> = data.split('\n').collect();
    if !lines.is_empty() && lines.last().map(|l| l.is_empty()).unwrap_or(false) { lines.pop(); }
    let total_lines = lines.len();

    let start = match offset {
        Some(o) if o > 1 => {
            if o > total_lines { bail!("Error: offset {} exceeds total lines {} in {}", o, total_lines, path); }
            o - 1
        }
        _ => 0,
    };
    let end = match limit {
        Some(l) if l > 0 => (start + l).min(total_lines),
        _ => total_lines,
    };
    Ok(lines[start..end].join("\n"))
}

pub fn write(path: &str, content: &str, max_bytes: usize) -> Result<String> {
    if path.is_empty() { bail!("Error: no path provided"); }
    if content.len() > max_bytes {
        bail!("Error: content too large for write_file ({} bytes > {} bytes)", content.len(), max_bytes);
    }
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    let sz = std::fs::metadata(path)?.len();
    Ok(format!("OK: wrote {sz} bytes to {path}"))
}

pub fn edit(path: &str, old_s: &str, new_s: &str, max_bytes: usize) -> Result<String> {
    if path.is_empty() { bail!("Error: no path provided"); }
    if old_s.is_empty() { bail!("Error: empty old_string"); }
    let content = std::fs::read_to_string(path)
        .map_err(|_| anyhow!("Error: file not found: {path}"))?;
    if content.len() > max_bytes {
        bail!("Error: file too large for edit_file ({} bytes > {} bytes)", content.len(), max_bytes);
    }
    if !content.contains(old_s) {
        bail!("Error: old_string not found in {path}. Hint: use Grep to locate the target lines, then Read the relevant portion (with offset/limit) to copy the exact text before retrying Edit.");
    }
    let updated = content.replacen(old_s, new_s, 1);
    if updated.is_empty() { bail!("Error: edit produced empty result, reverted"); }
    let diff = unified_diff_color(path, &content, &updated)?;
    std::fs::write(path, updated)?;
    if diff.is_empty() {
        Ok(format!("Edit({path}) [no changes]"))
    } else {
        let (added, removed) = count_diff_lines(&diff);
        let summary = format!("Edit({path}) [+{added} -{removed} lines]");
        Ok(format!("{summary}\n{diff}\n"))
    }
}

fn unified_diff_color(path: &str, old_content: &str, new_content: &str) -> Result<String> {
    let old_path = std::env::temp_dir().join(format!("edit-old-{}", std::process::id()));
    let new_path = std::env::temp_dir().join(format!("edit-new-{}", std::process::id()));
    std::fs::write(&old_path, old_content)?;
    std::fs::write(&new_path, new_content)?;
    let label = path.trim_start_matches('/');

    let diff = std::process::Command::new("diff")
        .args(["-u", "--color=always", "--label", &format!("a/{label}"), "--label", &format!("b/{label}"),
               old_path.to_str().unwrap_or(""), new_path.to_str().unwrap_or("")])
        .output();
    let _ = std::fs::remove_file(&old_path);
    let _ = std::fs::remove_file(&new_path);

    match diff {
        Ok(output) => {
            if output.status.success() || output.status.code() == Some(1) {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                if stdout.contains("unsupported --color") || stdout.contains("unrecognized option '--color'") {
                    let old2 = std::env::temp_dir().join(format!("edit-old2-{}", std::process::id()));
                    let new2 = std::env::temp_dir().join(format!("edit-new2-{}", std::process::id()));
                    std::fs::write(&old2, old_content)?;
                    std::fs::write(&new2, new_content)?;
                    let diff2 = std::process::Command::new("diff")
                        .args(["-u", "--label", &format!("a/{label}"), "--label", &format!("b/{label}"),
                               old2.to_str().unwrap_or(""), new2.to_str().unwrap_or("")])
                        .output();
                    let _ = std::fs::remove_file(&old2);
                    let _ = std::fs::remove_file(&new2);
                    match diff2 {
                        Ok(o) if o.status.success() || o.status.code() == Some(1) =>
                            Ok(String::from_utf8_lossy(&o.stdout).to_string()),
                        _ => bail!("Error: diff failed"),
                    }
                } else {
                    Ok(stdout)
                }
            } else {
                bail!("Error: diff failed")
            }
        }
        Err(_) => bail!("Error: diff failed"),
    }
}

fn count_diff_lines(diff: &str) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.lines() {
        let stripped = strip_ansi(line);
        if stripped.starts_with('+') && !stripped.starts_with("+++") { added += 1; }
        if stripped.starts_with('-') && !stripped.starts_with("---") { removed += 1; }
    }
    (added, removed)
}

fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && ((bytes[j] >= 0x30 && bytes[j] <= 0x3f) || (bytes[j] >= 0x20 && bytes[j] <= 0x2f)) { j += 1; }
            if j < bytes.len() && bytes[j] >= 0x40 && bytes[j] <= 0x7e { j += 1; }
            i = j;
        } else { out.push(bytes[i] as char); i += 1; }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_file(name: &str, content: &str) -> String {
        let path = format!("/tmp/dscode-test-{}-{}", name, std::process::id());
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
        let p = format!("/tmp/dscode-test-write-{}", std::process::id());
        let result = write(&p, "hello", 1000).unwrap();
        assert!(result.contains("OK"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn write_exceeds_max_bytes_error() {
        let p = format!("/tmp/dscode-test-write-big-{}", std::process::id());
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
}

pub struct ReadTool;
pub struct WriteTool;
pub struct EditTool;

impl super::runner::ToolExec for ReadTool {
    fn name(&self) -> &'static str { "Read" }
    fn execute(&self, input: &serde_json::Value, _ctx: &crate::context::ToolContext) -> anyhow::Result<(String, bool, String, Option<i32>)> {
        #[derive(serde::Deserialize)]
        struct Args { path: String, #[serde(default)] offset: Option<usize>, #[serde(default)] limit: Option<usize> }
        let args: Args = serde_json::from_value(input.clone())?;
        read(&args.path, args.offset, args.limit).map(|s| (s, false, String::new(), None))
    }
}

impl super::runner::ToolExec for WriteTool {
    fn name(&self) -> &'static str { "Write" }
    fn execute(&self, input: &serde_json::Value, ctx: &crate::context::ToolContext) -> anyhow::Result<(String, bool, String, Option<i32>)> {
        #[derive(serde::Deserialize)]
        struct Args { path: String, content: String }
        let args: Args = serde_json::from_value(input.clone())?;
        write(&args.path, &args.content, ctx.file_write_max_bytes).map(|s| (s, false, String::new(), None))
    }
}

impl super::runner::ToolExec for EditTool {
    fn name(&self) -> &'static str { "Edit" }
    fn execute(&self, input: &serde_json::Value, ctx: &crate::context::ToolContext) -> anyhow::Result<(String, bool, String, Option<i32>)> {
        #[derive(serde::Deserialize)]
        struct Args { path: String, old_string: String, new_string: String }
        let args: Args = serde_json::from_value(input.clone())?;
        edit(&args.path, &args.old_string, &args.new_string, ctx.file_write_max_bytes)
            .map(|s| { let c = s.clone(); (s, false, c, None) })
    }
}
