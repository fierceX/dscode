use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 5000;
const MAX_RESULTS: usize = 1000;
const MAX_OUTPUT_BYTES: usize = 100_000;

pub fn glob(pattern: &str, path: &str) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let glob = build_glob_matcher(pattern)
        .map_err(|e| anyhow!("Error: invalid glob pattern '{pattern}': {e}"))?
        .compile_matcher();
    let base = Path::new(path);
    let walk_root = glob_walk_root(base, pattern);

    let walker = ignore::WalkBuilder::new(walk_root)
        .standard_filters(true)
        .max_depth(Some(50))
        .build();

    let mut results = Vec::new();
    let mut total_bytes = 0usize;

    for entry in walker.flatten().take(MAX_FILES) {
        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }
        let p = entry.path();
        let match_path = relative_match_path(p, base);
        if glob.is_match(&match_path) {
            let line = match_path;
            total_bytes += line.len() + 1;
            if total_bytes > MAX_OUTPUT_BYTES {
                results.push(format!(
                    "... truncated: {} files shown, output > {MAX_OUTPUT_BYTES} bytes",
                    results.len()
                ));
                break;
            }
            results.push(line);
        }
    }

    if results.is_empty() {
        let scope = base.display();
        results.push(format!(
            "No files matched pattern '{pattern}' under {scope}"
        ));
    }

    Ok(results.join("\n"))
}

fn build_glob_matcher(pattern: &str) -> std::result::Result<globset::Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
}

fn glob_walk_root(base: &Path, pattern: &str) -> PathBuf {
    let Some(prefix) = static_glob_dir_prefix(pattern) else {
        return base.to_path_buf();
    };
    let candidate = base.join(prefix);
    if candidate.is_dir() {
        candidate
    } else {
        base.to_path_buf()
    }
}

fn static_glob_dir_prefix(pattern: &str) -> Option<&str> {
    let meta_idx = pattern
        .char_indices()
        .find_map(|(idx, ch)| matches!(ch, '*' | '?' | '[' | '{').then_some(idx))?;
    let raw_prefix = &pattern[..meta_idx];
    let trimmed = raw_prefix.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    if raw_prefix.ends_with('/') || raw_prefix.ends_with('\\') {
        return Some(trimmed);
    }
    trimmed
        .rfind(|ch| ch == '/' || ch == '\\')
        .and_then(|idx| (idx > 0).then_some(&trimmed[..idx]))
}

fn relative_match_path(path: &Path, base: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let normalized = rel
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

pub fn grep(pattern: &str, path: &str, file_glob: &str, context: Option<usize>) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let re = regex::Regex::new(pattern)
        .map_err(|e| anyhow!("Error: invalid regex pattern '{pattern}': {e}"))?;
    let ctx = context.unwrap_or(0);

    let file_glob_matcher = if !file_glob.is_empty() {
        Some(
            build_glob_matcher(file_glob)
                .map_err(|e| anyhow!("Error: invalid glob '{file_glob}': {e}"))?
                .compile_matcher(),
        )
    } else {
        None
    };

    let walker = ignore::WalkBuilder::new(path)
        .standard_filters(true)
        .max_depth(Some(50))
        .build();

    let mut results: Vec<String> = Vec::new();
    let mut total_results = 0usize;
    let mut total_bytes = 0usize;
    let mut truncated = false;

    for entry in walker.flatten().take(MAX_FILES) {
        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path();

        if let Some(ref matcher) = file_glob_matcher {
            let relative_path = relative_match_path(file_path, Path::new(path));
            if !matcher.is_match(&relative_path) {
                continue;
            }
        }

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let lines: Vec<&str> = content.lines().collect();
        let mut in_hunk = false;

        for (i, line) in lines.iter().enumerate() {
            if !re.is_match(line) {
                continue;
            }

            if total_results >= MAX_RESULTS {
                results.push(format!("... truncated at {MAX_RESULTS} results"));
                truncated = true;
                break;
            }

            if ctx > 0 {
                let start = i.saturating_sub(ctx);
                let end = (i + 1 + ctx).min(lines.len());

                if !in_hunk && i > start {
                    results.push("--".to_string());
                    total_bytes += 3;
                }
                in_hunk = true;

                for ctx_i in start..end {
                    let marker = if ctx_i == i { '>' } else { ' ' };
                    let line_str = format!(
                        "{}:{}:{} {}",
                        file_path.display(),
                        ctx_i + 1,
                        marker,
                        lines[ctx_i]
                    );
                    total_bytes += line_str.len() + 1;
                    if total_bytes > MAX_OUTPUT_BYTES {
                        results.push(format!("... truncated: output > {MAX_OUTPUT_BYTES} bytes"));
                        truncated = true;
                        break;
                    }
                    results.push(line_str);
                }
            } else {
                let line_str = format!("{}:{}:{}", file_path.display(), i + 1, line);
                total_bytes += line_str.len() + 1;
                if total_bytes > MAX_OUTPUT_BYTES {
                    results.push(format!("... truncated: output > {MAX_OUTPUT_BYTES} bytes"));
                    truncated = true;
                    break;
                }
                results.push(line_str);
            }

            total_results += 1;
        }
        if truncated {
            break;
        }
    }

    Ok(results.join("\n"))
}

// ---- Tool implementations ----

pub struct GlobTool;
pub struct GrepTool;

impl super::runner::ToolExec for GlobTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Glob",
            "Find files matching a glob pattern.",
            super::metadata::ApprovalTier::Read,
            super::metadata::ToolResultKind::Search,
        )
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            #[serde(default)]
            path: Option<String>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        glob(&args.pattern, args.path.as_deref().unwrap_or("."))
            .map(super::runner::ToolOutcome::text)
    }
}

impl super::runner::ToolExec for GrepTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "Grep",
            "Search file contents with a regex pattern.",
            super::metadata::ApprovalTier::Read,
            super::metadata::ToolResultKind::Search,
        )
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            glob: Option<String>,
            #[serde(default)]
            context: Option<usize>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        grep(
            &args.pattern,
            args.path.as_deref().unwrap_or("."),
            args.glob.as_deref().unwrap_or(""),
            args.context,
        )
        .map(super::runner::ToolOutcome::text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn glob_empty_pattern_errors() {
        assert!(glob("", ".").is_err());
    }

    #[test]
    fn glob_basic_matches() {
        // Should find Cargo.toml in the project root
        let result = glob("Cargo.toml", ".").unwrap();
        assert!(result.contains("Cargo.toml"));
    }

    #[test]
    fn glob_recursive_pattern() {
        let result = glob("**/*.rs", "src/tools").unwrap();
        assert!(result.contains("search.rs") || result.contains("bash.rs"));
        assert!(result.contains("runner.rs"));
    }

    #[test]
    fn glob_matches_rooted_pattern_from_cwd() {
        let result = glob("src/**/*.rs", ".").unwrap();

        assert!(result.contains("src/tools/search.rs"));
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn glob_matches_rooted_pattern_from_absolute_root() {
        let dir = std::env::temp_dir().join(format!("glob-root-{}", std::process::id()));
        fs::create_dir_all(dir.join("src/tools")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/tools/search.rs"), "pub fn search() {}\n").unwrap();

        let result = glob("src/**/*.rs", &dir.display().to_string()).unwrap();

        assert!(result.contains("src/main.rs"));
        assert!(result.contains("src/tools/search.rs"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_plain_pattern_matches_current_level_only() {
        let dir = temp_dir("glob-plain");
        fs::create_dir_all(dir.join("docs/specs")).unwrap();
        fs::write(dir.join("root.docx"), "doc").unwrap();
        fs::write(dir.join("docs/specs/api.docx"), "doc").unwrap();
        fs::write(dir.join("docs/specs/notes.txt"), "note").unwrap();

        let result = glob("*.docx", &dir.display().to_string()).unwrap();

        assert!(result.contains("root.docx"));
        assert!(!result.contains("docs/specs/api.docx"));
        assert!(!result.contains("docs/specs/notes.txt"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_recursive_pattern_matches_nested_file_names() {
        let dir = temp_dir("glob-recursive");
        fs::create_dir_all(dir.join("src/bin")).unwrap();
        fs::write(dir.join("src/bin/main.rs"), "fn main() {}\n").unwrap();

        let result = glob("**/*.*", &dir.display().to_string()).unwrap();

        assert!(result.contains("src/bin/main.rs"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_empty_result_reports_no_match() {
        let dir = temp_dir("glob-empty");
        fs::create_dir_all(&dir).unwrap();

        let result = glob("*.docx", &dir.display().to_string()).unwrap();

        assert!(result.contains("No files matched pattern '*.docx'"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_empty_pattern_errors() {
        assert!(grep("", ".", "", None).is_err());
    }

    #[test]
    fn grep_basic_search() {
        let dir = std::env::temp_dir().join(format!("grep-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.txt"), "hello world\nfoo bar\n").unwrap();

        let result = grep("hello", &dir.display().to_string(), "", None).unwrap();
        assert!(result.contains("hello"));
        assert!(!result.contains("foo"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_with_context() {
        let dir = std::env::temp_dir().join(format!("grep-ctx-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.txt"), "line1\nline2\nmatch\nline4\nline5\n").unwrap();

        let result = grep("match", &dir.display().to_string(), "", Some(1)).unwrap();
        assert!(result.contains("match"));
        assert!(result.contains("line2")); // context before
        assert!(result.contains("line4")); // context after

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_truncates_large_output() {
        // Create a file with many matching lines to trigger truncation
        let dir = std::env::temp_dir().join(format!("grep-trunc-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let content = (0..2000)
            .map(|i| format!("match line {}\n", i))
            .collect::<String>();
        fs::write(dir.join("large.txt"), content).unwrap();

        let result = grep("match", &dir.display().to_string(), "", None).unwrap();
        assert!(result.contains("truncated"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_with_file_glob() {
        let dir = std::env::temp_dir().join(format!("grep-glob-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data.txt"), "secret\n").unwrap();
        fs::write(dir.join("data.md"), "secret\n").unwrap();

        let result = grep("secret", &dir.display().to_string(), "*.txt", None).unwrap();
        assert!(result.contains("data.txt"));
        assert!(!result.contains("data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_file_glob_uses_globset_path_semantics() {
        let dir = temp_dir("grep-glob-path");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("root.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.md"), "secret\n").unwrap();

        let result = grep("secret", &dir.display().to_string(), "*.txt", None).unwrap();

        assert!(result.contains("root.txt"));
        assert!(!result.contains("nested/data.txt"));
        assert!(!result.contains("nested/data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_file_glob_recursive_pattern_matches_nested_files() {
        let dir = temp_dir("grep-glob-recursive");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/data.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.md"), "secret\n").unwrap();

        let result = grep("secret", &dir.display().to_string(), "**/*.txt", None).unwrap();

        assert!(result.contains("nested/data.txt"));
        assert!(!result.contains("nested/data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_uses_ignore_standard_filters() {
        let dir = temp_dir("glob-ignore");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".hidden.txt"), "hidden\n").unwrap();
        fs::write(dir.join("kept.txt"), "kept\n").unwrap();

        let result = glob("*.txt", &dir.display().to_string()).unwrap();

        assert!(result.contains("kept.txt"));
        assert!(!result.contains(".hidden.txt"));

        fs::remove_dir_all(&dir).ok();
    }
}
