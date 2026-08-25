use anyhow::{Result, anyhow, bail};
use grep_regex::RegexMatcher;

const MAX_SEARCH_OUTPUT_BYTES: usize = 100_000;

/// Per-agent scope supplied to every virtual filesystem operation.
///
/// Child agents keep the parent's `resource_session_id` so they see the same
/// knowledge base, while `agent_session_id` identifies the concrete caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsScope {
    pub resource_session_id: String,
    pub agent_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsReadRequest {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub max_full_read_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsReadResult {
    pub content: String,
    pub total_lines: usize,
    pub total_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsGlobRequest {
    pub pattern: String,
    pub path: String,
    pub max_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VfsGlobResult {
    pub paths: Vec<String>,
    pub scanned_files: usize,
    pub truncated: bool,
    pub skipped_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsGrepRequest {
    pub pattern: String,
    pub path: String,
    pub file_glob: String,
    pub context: Option<usize>,
    pub max_files: usize,
    pub max_results: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VfsGrepEntry {
    Separator,
    Line {
        path: String,
        line_number: usize,
        content: String,
        matched: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VfsGrepResult {
    pub entries: Vec<VfsGrepEntry>,
    pub match_count: usize,
    pub scanned_files: usize,
    pub truncated_results: bool,
    pub truncated_files: bool,
    pub skipped_files: usize,
}

/// Synchronous, read-only filesystem hook used by Read, Glob and Grep.
///
/// Implementations must apply `scope.resource_session_id` to every lookup.
/// Local filesystem access remains an internal fallback so virtual backends
/// cannot manufacture editable snapshots.
pub trait ReadOnlyFileSystem: Send + Sync {
    fn read(&self, scope: &VfsScope, request: &VfsReadRequest) -> Result<VfsReadResult>;

    fn glob(&self, scope: &VfsScope, request: &VfsGlobRequest) -> Result<VfsGlobResult>;

    fn grep(&self, scope: &VfsScope, request: &VfsGrepRequest) -> Result<VfsGrepResult>;

    /// Optional image read. The default returns `Ok(None)`: existing
    /// implementations need no changes (v7 §12). Returned bytes must be
    /// self-consistent and must not exceed `max_bytes` (enforced by the
    /// caller).
    fn read_image(
        &self,
        scope: &VfsScope,
        path: &str,
        max_bytes: u64,
    ) -> Result<Option<VfsImage>> {
        let _ = (scope, path, max_bytes);
        Ok(None)
    }
}

/// Image bytes exposed by a virtual filesystem backend.
pub struct VfsImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}

pub fn validate_virtual_glob_request(request: &VfsGlobRequest) -> Result<()> {
    if request.pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    globset::GlobBuilder::new(&request.pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| anyhow!("Error: invalid glob pattern '{}': {e}", request.pattern))?;
    normalize_virtual_root(&request.path)?;
    Ok(())
}

pub fn validate_virtual_grep_request(request: &VfsGrepRequest) -> Result<()> {
    if request.pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    if request.max_results == 0 {
        bail!("Error: max search results must be greater than zero");
    }
    RegexMatcher::new_line_matcher(&request.pattern)
        .map_err(|e| anyhow!("Error: invalid regex pattern '{}': {e}", request.pattern))?;
    if !request.file_glob.is_empty() {
        globset::GlobBuilder::new(&request.file_glob)
            .literal_separator(true)
            .build()
            .map_err(|e| anyhow!("Error: invalid glob '{}': {e}", request.file_glob))?;
    }
    normalize_virtual_root(&request.path)?;
    Ok(())
}

/// Normalize a virtual file path using POSIX separators and lexical `.`/`..`.
///
/// Absolute and relative virtual paths share the same root. Escaping that root
/// is rejected.
pub fn normalize_virtual_file_path(path: &str) -> Result<String> {
    let normalized = normalize_virtual_path(path)?;
    if normalized.is_empty() {
        bail!("virtual file path is empty");
    }
    Ok(normalized)
}

/// Normalize a virtual search root. `""`, `"."`, and `"/"` mean the VFS root.
pub fn normalize_virtual_root(path: &str) -> Result<String> {
    normalize_virtual_path(path)
}

fn normalize_virtual_path(path: &str) -> Result<String> {
    if path.contains('\0') {
        bail!("virtual path contains a NUL byte");
    }
    let replaced = path.trim().replace('\\', "/");
    let mut parts = Vec::new();
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("virtual path may not escape its root: {path}");
                }
            }
            value => parts.push(value),
        }
    }
    Ok(parts.join("/"))
}

pub fn format_virtual_glob(result: &VfsGlobResult, request: &VfsGlobRequest) -> String {
    let mut lines = Vec::new();
    let mut total_bytes = 0usize;
    for path in &result.paths {
        total_bytes += path.len() + 1;
        if total_bytes > MAX_SEARCH_OUTPUT_BYTES {
            lines.push(format!(
                "... truncated: {} files shown, output > {MAX_SEARCH_OUTPUT_BYTES} bytes",
                lines.len()
            ));
            break;
        }
        lines.push(path.clone());
    }
    if lines.is_empty() {
        lines.push(format!(
            "No files matched pattern '{}' under {}",
            request.pattern, request.path
        ));
    }
    if result.truncated {
        lines.push(format!(
            "... search truncated: scanned first {} files; results may be incomplete",
            request.max_files
        ));
    }
    if result.skipped_files > 0 {
        lines.push(format!(
            "... skipped {} paths due to traversal errors",
            result.skipped_files
        ));
    }
    lines.join("\n")
}

pub fn format_virtual_grep(result: &VfsGrepResult, request: &VfsGrepRequest) -> String {
    let context = request.context.unwrap_or(0);
    let mut lines = Vec::new();
    let mut total_bytes = 0usize;

    for entry in &result.entries {
        let rendered = match entry {
            VfsGrepEntry::Separator => "--".to_string(),
            VfsGrepEntry::Line {
                path,
                line_number,
                content,
                matched,
            } if context > 0 => {
                let marker = if *matched { '>' } else { ' ' };
                format!("{path}:{line_number}:{marker} {content}")
            }
            VfsGrepEntry::Line {
                path,
                line_number,
                content,
                ..
            } => format!("{path}:{line_number}:{content}"),
        };
        total_bytes += rendered.len() + 1;
        if total_bytes > MAX_SEARCH_OUTPUT_BYTES {
            lines.push(format!(
                "... truncated: output > {MAX_SEARCH_OUTPUT_BYTES} bytes"
            ));
            break;
        }
        lines.push(rendered);
    }

    if lines.is_empty() && !result.truncated_results {
        lines.push(format!(
            "No content matched pattern '{}' under {}",
            request.pattern, request.path
        ));
    }
    if result.truncated_results {
        lines.push(format!("... truncated at {} results", request.max_results));
    }
    if result.truncated_files {
        lines.push(format!(
            "... search truncated: scanned first {} files; results may be incomplete",
            request.max_files
        ));
    }
    if result.skipped_files > 0 {
        lines.push(format!(
            "... skipped {} paths due to traversal errors",
            result.skipped_files
        ));
    }
    lines.join("\n")
}

pub fn select_virtual_lines(
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    path: &str,
) -> Result<String> {
    if offset.is_none() && limit.is_none() {
        return Ok(text.to_string());
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let total = lines.len();
    let start_line = offset.unwrap_or(1).max(1);
    if start_line > total {
        if total == 0 && start_line == 1 {
            return Ok(String::new());
        }
        bail!(
            "Error: offset {} exceeds total lines {} in {}",
            start_line,
            total,
            path
        );
    }
    let start = start_line - 1;
    let end = limit.map_or(total, |count| {
        if count == 0 {
            total
        } else {
            start.saturating_add(count).min(total)
        }
    });
    Ok(lines[start..end].join("\n"))
}

pub fn tool_line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1
    }
}

#[cfg(test)]
#[path = "vfs_tests.rs"]
mod tests;
