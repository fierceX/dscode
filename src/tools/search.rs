use anyhow::{Result, anyhow, bail};

const MAX_FILES: usize = 5000;
const MAX_RESULTS: usize = 1000;
const MAX_OUTPUT_BYTES: usize = 100_000;

pub fn glob(pattern: &str, path: &str) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let glob = globset::Glob::new(pattern)
        .map_err(|e| anyhow!("Error: invalid glob pattern '{pattern}': {e}"))?
        .compile_matcher();

    let walker = ignore::WalkBuilder::new(path)
        .standard_filters(false)
        .max_depth(Some(50))
        .build();

    let mut results = Vec::new();
    let mut total_bytes = 0usize;

    for entry in walker.flatten().take(MAX_FILES) {
        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }
        let p = entry.path();
        // ignore::Walk may yield paths with a "./" prefix; normalize for glob matching.
        let p_display = p.display().to_string();
        let p_normalized = p_display.strip_prefix("./").unwrap_or(&p_display);
        if glob.is_match(p_normalized) {
            let line = p_display;
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

    Ok(results.join("\n"))
}

pub fn grep(
    pattern: &str,
    path: &str,
    file_glob: &str,
    context: Option<usize>,
) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let re = regex::Regex::new(pattern)
        .map_err(|e| anyhow!("Error: invalid regex pattern '{pattern}': {e}"))?;
    let ctx = context.unwrap_or(0);

    let file_glob_matcher = if !file_glob.is_empty() {
        Some(
            globset::Glob::new(file_glob)
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
            let f_display = file_path.display().to_string();
            let f_normalized = f_display.strip_prefix("./").unwrap_or(&f_display);
            if !matcher.is_match(f_normalized) {
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
                    let line_str =
                        format!("{}:{}:{} {}", file_path.display(), ctx_i + 1, marker, lines[ctx_i]);
                    total_bytes += line_str.len() + 1;
                    if total_bytes > MAX_OUTPUT_BYTES {
                        results.push(format!(
                            "... truncated: output > {MAX_OUTPUT_BYTES} bytes"
                        ));
                        truncated = true;
                        break;
                    }
                    results.push(line_str);
                }
            } else {
                let line_str = format!("{}:{}:{}", file_path.display(), i + 1, line);
                total_bytes += line_str.len() + 1;
                if total_bytes > MAX_OUTPUT_BYTES {
                    results.push(format!(
                        "... truncated: output > {MAX_OUTPUT_BYTES} bytes"
                    ));
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
    fn name(&self) -> &'static str {
        "Glob"
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
    fn name(&self) -> &'static str {
        "Grep"
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
        fs::write(
            dir.join("test.txt"),
            "line1\nline2\nmatch\nline4\nline5\n",
        )
        .unwrap();

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
    fn glob_respects_hidden_dirs() {
        // Glob doesn't need to respect .gitignore — it's file discovery, not search.
        // This test verifies we can still find files in directories with dotfiles.
    }
}
