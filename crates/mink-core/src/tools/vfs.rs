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
mod tests {
    use super::*;

    #[test]
    fn normalizes_virtual_paths() {
        assert_eq!(normalize_virtual_root("./docs//api").unwrap(), "docs/api");
        assert_eq!(normalize_virtual_root("docs/../src").unwrap(), "src");
        assert_eq!(normalize_virtual_root("/").unwrap(), "");
        assert!(normalize_virtual_root("../private").is_err());
    }

    #[test]
    fn virtual_glob_formatter_preserves_protocol_output() {
        let request = VfsGlobRequest {
            pattern: "**/*.md".into(),
            path: ".".into(),
            max_files: 100,
        };
        let result = VfsGlobResult {
            paths: vec!["docs/guide.md".into()],
            scanned_files: 1,
            truncated: false,
            skipped_files: 0,
        };
        assert_eq!(format_virtual_glob(&result, &request), "docs/guide.md");
    }

    #[test]
    fn virtual_grep_formatter_renders_context_and_paths() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: "./docs".into(),
            file_glob: "*.md".into(),
            context: Some(1),
            max_files: 100,
            max_results: 100,
        };
        let result = VfsGrepResult {
            entries: vec![
                VfsGrepEntry::Line {
                    path: "docs/guide.md".into(),
                    line_number: 1,
                    content: "intro".into(),
                    matched: false,
                },
                VfsGrepEntry::Line {
                    path: "docs/guide.md".into(),
                    line_number: 2,
                    content: "needle".into(),
                    matched: true,
                },
            ],
            match_count: 1,
            scanned_files: 1,
            truncated_results: false,
            truncated_files: false,
            skipped_files: 0,
        };
        let output = format_virtual_grep(&result, &request);
        assert!(output.contains("docs/guide.md:2:> needle"), "{output}");
        assert!(output.contains("docs/guide.md:1:  intro"), "{output}");
        assert!(!output.contains("src/lib.rs"), "{output}");
    }

    #[test]
    fn virtual_line_selection_matches_read_ranges() {
        assert_eq!(
            select_virtual_lines("one\ntwo\nthree\n", Some(2), Some(2), "a.txt").unwrap(),
            "two\nthree"
        );
        assert!(select_virtual_lines("one\n", Some(2), None, "a.txt").is_err());
    }

    #[test]
    fn virtual_grep_formatter_handles_zero_result_limit() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: ".".into(),
            file_glob: String::new(),
            context: None,
            max_files: 10,
            max_results: 0,
        };
        let result = VfsGrepResult {
            truncated_results: true,
            ..VfsGrepResult::default()
        };
        let output = format_virtual_grep(&result, &request);
        assert_eq!(output, "... truncated at 0 results");
    }
}
