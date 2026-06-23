use anyhow::{Result, anyhow, bail};

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
    regex::Regex::new(&request.pattern)
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

/// A logical UTF-8 file used by database backend helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualFile {
    pub path: String,
    pub content: String,
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

pub fn collect_virtual_glob(
    files: impl IntoIterator<Item = VirtualFile>,
    request: &VfsGlobRequest,
) -> Result<VfsGlobResult> {
    try_collect_virtual_glob(files.into_iter().map(Ok), request)
}

pub fn try_collect_virtual_glob(
    files: impl IntoIterator<Item = Result<VirtualFile>>,
    request: &VfsGlobRequest,
) -> Result<VfsGlobResult> {
    validate_virtual_glob_request(request)?;
    let matcher = globset::GlobBuilder::new(&request.pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| anyhow!("Error: invalid glob pattern '{}': {e}", request.pattern))?
        .compile_matcher();
    let root = normalize_virtual_root(&request.path)?;
    let mut result = VfsGlobResult::default();

    for file in files {
        let file = file?;
        let path = normalize_virtual_file_path(&file.path)?;
        let Some(relative) = relative_virtual_path(&path, &root) else {
            continue;
        };
        result.scanned_files += 1;
        if result.scanned_files > request.max_files {
            result.scanned_files = request.max_files;
            result.truncated = true;
            break;
        }
        if matcher.is_match(relative) {
            result.paths.push(relative.to_string());
        }
    }
    Ok(result)
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

pub fn collect_virtual_grep(
    files: impl IntoIterator<Item = VirtualFile>,
    request: &VfsGrepRequest,
) -> Result<VfsGrepResult> {
    try_collect_virtual_grep(files.into_iter().map(Ok), request)
}

pub fn try_collect_virtual_grep(
    files: impl IntoIterator<Item = Result<VirtualFile>>,
    request: &VfsGrepRequest,
) -> Result<VfsGrepResult> {
    validate_virtual_grep_request(request)?;
    let regex = regex::Regex::new(&request.pattern)
        .map_err(|e| anyhow!("Error: invalid regex pattern '{}': {e}", request.pattern))?;
    let file_matcher = if request.file_glob.is_empty() {
        None
    } else {
        Some(
            globset::GlobBuilder::new(&request.file_glob)
                .literal_separator(true)
                .build()
                .map_err(|e| anyhow!("Error: invalid glob '{}': {e}", request.file_glob))?
                .compile_matcher(),
        )
    };
    let root = normalize_virtual_root(&request.path)?;
    let context = request.context.unwrap_or(0);
    let mut result = VfsGrepResult::default();

    for file in files {
        let file = file?;
        let path = normalize_virtual_file_path(&file.path)?;
        let Some(relative) = relative_virtual_path(&path, &root) else {
            continue;
        };
        result.scanned_files += 1;
        if result.scanned_files > request.max_files {
            result.scanned_files = request.max_files;
            result.truncated_files = true;
            break;
        }
        if file_matcher
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(relative))
        {
            continue;
        }

        let lines: Vec<&str> = file.content.lines().collect();
        let mut in_hunk = false;
        for (index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if result.match_count >= request.max_results {
                result.truncated_results = true;
                break;
            }
            if context > 0 {
                let start = index.saturating_sub(context);
                let end = (index + 1 + context).min(lines.len());
                if !in_hunk && index > start {
                    result.entries.push(VfsGrepEntry::Separator);
                }
                in_hunk = true;
                for (line_index, context_line) in lines.iter().enumerate().take(end).skip(start) {
                    result.entries.push(VfsGrepEntry::Line {
                        path: path.clone(),
                        line_number: line_index + 1,
                        content: (*context_line).to_string(),
                        matched: line_index == index,
                    });
                }
            } else {
                result.entries.push(VfsGrepEntry::Line {
                    path: path.clone(),
                    line_number: index + 1,
                    content: (*line).to_string(),
                    matched: true,
                });
            }
            result.match_count += 1;
            if result.match_count >= request.max_results {
                result.truncated_results = true;
                break;
            }
        }
        if result.truncated_results {
            break;
        }
    }
    Ok(result)
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

fn relative_virtual_path<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root.is_empty() {
        Some(path)
    } else if path == root {
        Some("")
    } else {
        path.strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<VirtualFile> {
        vec![
            VirtualFile {
                path: "docs/guide.md".into(),
                content: "intro\nneedle\nend\n".into(),
            },
            VirtualFile {
                path: "src/lib.rs".into(),
                content: "pub fn value() {}\n".into(),
            },
        ]
    }

    #[test]
    fn normalizes_virtual_paths() {
        assert_eq!(normalize_virtual_root("./docs//api").unwrap(), "docs/api");
        assert_eq!(normalize_virtual_root("docs/../src").unwrap(), "src");
        assert_eq!(normalize_virtual_root("/").unwrap(), "");
        assert!(normalize_virtual_root("../private").is_err());
    }

    #[test]
    fn virtual_glob_preserves_globset_semantics() {
        let request = VfsGlobRequest {
            pattern: "**/*.md".into(),
            path: ".".into(),
            max_files: 100,
        };
        let result = collect_virtual_glob(files(), &request).unwrap();
        assert_eq!(format_virtual_glob(&result, &request), "docs/guide.md");
    }

    #[test]
    fn virtual_grep_renders_context_and_paths() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: "./docs".into(),
            file_glob: "*.md".into(),
            context: Some(1),
            max_files: 100,
            max_results: 100,
        };
        let result = collect_virtual_grep(files(), &request).unwrap();
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
    fn virtual_grep_accepts_exact_file_root() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: "docs/guide.md".into(),
            file_glob: String::new(),
            context: None,
            max_files: 10,
            max_results: 10,
        };
        let result = collect_virtual_grep(files(), &request).unwrap();
        assert_eq!(result.match_count, 1);
    }

    #[test]
    fn zero_result_limit_reports_truncation_without_false_no_match() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: ".".into(),
            file_glob: String::new(),
            context: None,
            max_files: 10,
            max_results: 0,
        };
        let result = collect_virtual_grep(files(), &request).unwrap();
        let output = format_virtual_grep(&result, &request);
        assert_eq!(output, "... truncated at 0 results");
    }

    #[test]
    fn fallible_grep_iterator_stops_after_result_limit() {
        let request = VfsGrepRequest {
            pattern: "needle".into(),
            path: ".".into(),
            file_glob: String::new(),
            context: None,
            max_files: 10,
            max_results: 1,
        };
        let files = vec![
            Ok(VirtualFile {
                path: "first.md".into(),
                content: "needle\n".into(),
            }),
            Err(anyhow!("second row should not be consumed")),
        ];
        let result = try_collect_virtual_grep(files, &request).unwrap();
        assert_eq!(result.match_count, 1);
        assert!(result.truncated_results);
    }
}
