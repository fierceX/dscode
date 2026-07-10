use anyhow::{Result, anyhow, bail};
use grep_printer::StandardBuilder;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::overrides::OverrideBuilder;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_OUTPUT_BYTES: usize = 100_000;

pub fn glob(pattern: &str, path: &str, max_files: usize) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let base = Path::new(path);
    let overrides = build_rg_overrides(base, pattern)
        .map_err(|e| anyhow!("Error: invalid glob pattern '{pattern}': {e}"))?
        .build()
        .map_err(|e| anyhow!("Error: invalid glob pattern '{pattern}': {e}"))?;

    let mut walk_builder = ignore::WalkBuilder::new(base);
    configure_search_walker(&mut walk_builder);
    walk_builder.overrides(overrides);
    let walker = walk_builder.build();

    let mut results = Vec::new();
    let mut total_bytes = 0usize;
    let mut files_seen = 0usize;
    let mut walk_errors = 0usize;
    let mut truncated_walk = false;

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                walk_errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        files_seen += 1;
        if files_seen > max_files {
            truncated_walk = true;
            break;
        }
        let line = display_search_path(entry.path());
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

    if truncated_walk {
        results.push(format!(
            "... search truncated: scanned first {max_files} files; results may be incomplete"
        ));
    }
    if walk_errors > 0 {
        results.push(format!(
            "... skipped {walk_errors} paths due to traversal errors"
        ));
    }

    if results.is_empty() {
        results.extend(format_empty_glob_fallback(pattern, base));
    }

    Ok(results.join("\n"))
}

fn configure_search_walker(builder: &mut ignore::WalkBuilder) {
    builder.standard_filters(true).parents(false);
}

fn build_rg_overrides(
    base: &Path,
    pattern: &str,
) -> std::result::Result<OverrideBuilder, ignore::Error> {
    let mut builder = OverrideBuilder::new(base);
    builder.add(pattern)?;
    Ok(builder)
}

fn display_search_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn format_empty_glob_fallback(pattern: &str, base: &Path) -> Vec<String> {
    const MAX_ROOT_ENTRIES: usize = 50;

    let mut lines = vec![format!("... no files matched pattern '{pattern}'")];
    let mut entries = match fs::read_dir(base) {
        Ok(entries) => entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let file_type = entry.file_type().ok()?;
                let mut name = entry.file_name().to_string_lossy().to_string();
                if file_type.is_dir() {
                    name.push('/');
                }
                Some((!file_type.is_dir(), name))
            })
            .collect::<Vec<_>>(),
        Err(err) => {
            lines.push(format!(
                "... unable to list search root '{}': {err}",
                display_search_path(base)
            ));
            return lines;
        }
    };

    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    lines.push(format!("... search root '{}':", display_search_path(base)));
    for (_, name) in entries.iter().take(MAX_ROOT_ENTRIES) {
        lines.push(format!("...   {name}"));
    }
    if entries.len() > MAX_ROOT_ENTRIES {
        lines.push(format!(
            "...   ... {} more entries",
            entries.len() - MAX_ROOT_ENTRIES
        ));
    }
    lines
}

pub fn grep(
    pattern: &str,
    path: &str,
    file_glob: &str,
    context: Option<usize>,
    max_files: usize,
    max_results: usize,
) -> Result<String> {
    if pattern.is_empty() {
        bail!("Error: no pattern provided");
    }
    let matcher = RegexMatcher::new_line_matcher(pattern)
        .map_err(|e| anyhow!("Error: invalid regex pattern '{pattern}': {e}"))?;
    let ctx = context.unwrap_or(0);
    let base = Path::new(path);

    let mut walk_builder = ignore::WalkBuilder::new(base);
    configure_search_walker(&mut walk_builder);
    if !file_glob.is_empty() {
        let overrides = build_rg_overrides(base, file_glob)
            .map_err(|e| anyhow!("Error: invalid glob '{file_glob}': {e}"))?
            .build()
            .map_err(|e| anyhow!("Error: invalid glob '{file_glob}': {e}"))?;
        walk_builder.overrides(overrides);
    }
    let walker = walk_builder.build();

    let mut output: Vec<u8> = Vec::new();
    let mut total_results = 0usize;
    let mut files_seen = 0usize;
    let mut walk_errors = 0usize;
    let mut truncated_walk = false;
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                walk_errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        files_seen += 1;
        if files_seen > max_files {
            truncated_walk = true;
            break;
        }
        let file_path = entry.path();
        let remaining = max_results.saturating_sub(total_results);
        if remaining == 0 {
            break;
        }

        let mut file_output = Vec::new();
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .before_context(ctx)
            .after_context(ctx)
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .max_matches(Some(remaining as u64))
            .build();

        let mut printer_builder = StandardBuilder::new();
        printer_builder.heading(false).path(true);
        let match_count = {
            let mut printer = printer_builder.build_no_color(&mut file_output);
            let display_path = PathBuf::from(display_search_path(file_path));
            let mut sink = printer.sink_with_path(&matcher, &display_path);
            if searcher
                .search_path(&matcher, file_path, &mut sink)
                .is_err()
            {
                continue;
            }
            sink.match_count() as usize
        };
        if match_count == 0 {
            continue;
        }
        append_rg_output(&mut output, &file_output, ctx > 0);
        total_results = total_results.saturating_add(match_count.min(remaining));
        if output.len() > MAX_OUTPUT_BYTES {
            output.truncate(MAX_OUTPUT_BYTES);
            output.extend_from_slice(
                format!("\n... truncated: output > {MAX_OUTPUT_BYTES} bytes").as_bytes(),
            );
            break;
        }
        if total_results >= max_results {
            append_tool_note(
                &mut output,
                &format!("... truncated at {max_results} results"),
            );
            break;
        }
    }

    if truncated_walk {
        append_tool_note(
            &mut output,
            &format!(
                "... search truncated: scanned first {max_files} files; results may be incomplete"
            ),
        );
    }
    if walk_errors > 0 {
        append_tool_note(
            &mut output,
            &format!("... skipped {walk_errors} paths due to traversal errors"),
        );
    }

    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn append_rg_output(output: &mut Vec<u8>, file_output: &[u8], with_context: bool) {
    if file_output.is_empty() {
        return;
    }
    if with_context && !output.is_empty() {
        output.extend_from_slice(b"--\n");
    }
    output.extend_from_slice(file_output);
}

fn append_tool_note(output: &mut Vec<u8>, note: &str) {
    if !output.is_empty() && !output.ends_with(b"\n") {
        output.push(b'\n');
    }
    output.extend_from_slice(note.as_bytes());
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
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            #[serde(default)]
            path: Option<String>,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        if let Some(vfs) = &ctx.read_only_fs {
            let request = crate::tools::vfs::VfsGlobRequest {
                pattern: args.pattern,
                path: args.path.unwrap_or_else(|| ".".to_string()),
                max_files: ctx.tool_config.max_search_files,
            };
            crate::tools::vfs::validate_virtual_glob_request(&request)?;
            let result = vfs.glob(&ctx.vfs_scope, &request)?;
            return Ok(super::runner::ToolOutcome::text(
                crate::tools::vfs::format_virtual_glob(&result, &request),
            ));
        }
        let root = resolve_search_root(&ctx.cwd, args.path.as_deref().unwrap_or("."));
        glob(
            &args.pattern,
            &root.display().to_string(),
            ctx.tool_config.max_search_files,
        )
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
        ctx: &crate::context::ToolContext,
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
        if let Some(vfs) = &ctx.read_only_fs {
            let request = crate::tools::vfs::VfsGrepRequest {
                pattern: args.pattern,
                path: args.path.unwrap_or_else(|| ".".to_string()),
                file_glob: args.glob.unwrap_or_default(),
                context: args.context,
                max_files: ctx.tool_config.max_search_files,
                max_results: ctx.tool_config.max_search_results,
            };
            crate::tools::vfs::validate_virtual_grep_request(&request)?;
            let result = vfs.grep(&ctx.vfs_scope, &request)?;
            return Ok(super::runner::ToolOutcome::text(
                crate::tools::vfs::format_virtual_grep(&result, &request),
            ));
        }
        let root = resolve_search_root(&ctx.cwd, args.path.as_deref().unwrap_or("."));
        grep(
            &args.pattern,
            &root.display().to_string(),
            args.glob.as_deref().unwrap_or(""),
            args.context,
            ctx.tool_config.max_search_files,
            ctx.tool_config.max_search_results,
        )
        .map(super::runner::ToolOutcome::text)
    }
}

fn resolve_search_root(cwd: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
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
        assert!(glob("", ".", 5000).is_err());
    }

    #[test]
    fn glob_basic_matches() {
        // Should find Cargo.toml in the project root
        let result = glob("Cargo.toml", ".", 5000).unwrap();
        assert!(result.contains("Cargo.toml"));
    }

    #[test]
    fn glob_recursive_pattern() {
        let result = glob("**/*.rs", "src/tools", 5000).unwrap();
        assert!(result.contains("search.rs") || result.contains("bash.rs"));
        assert!(result.contains("runner.rs"));
    }

    #[test]
    fn glob_matches_rooted_pattern_from_cwd() {
        let result = glob("src/**/*.rs", ".", 5000).unwrap();

        assert!(result.contains("src/tools/search.rs"), "{result}");
        assert!(result.contains("src/lib.rs"), "{result}");
    }

    #[test]
    fn glob_matches_rooted_pattern_from_absolute_root() {
        let dir = std::env::temp_dir().join(format!("glob-root-{}", std::process::id()));
        fs::create_dir_all(dir.join("src/tools")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/tools/search.rs"), "pub fn search() {}\n").unwrap();

        let result = glob("src/**/*.rs", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("src/main.rs"));
        assert!(result.contains("src/tools/search.rs"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_matches_directory_prefixed_single_level_pattern() {
        let dir = temp_dir("glob-dir-prefix");
        fs::create_dir_all(dir.join("data/nested")).unwrap();
        fs::create_dir_all(dir.join("other")).unwrap();
        fs::write(dir.join("data/a.md"), "a\n").unwrap();
        fs::write(dir.join("data/nested/b.md"), "b\n").unwrap();
        fs::write(dir.join("other/a.md"), "other\n").unwrap();

        let result = glob("data/*.md", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("data/a.md"), "{result}");
        assert!(!result.contains("data/nested/b.md"), "{result}");
        assert!(!result.contains("other/a.md"), "{result}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_pattern_is_relative_to_search_path() {
        let dir = temp_dir("glob-path-relative");
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join("data/a.md"), "a\n").unwrap();

        let data_root = dir.join("data");

        let result = glob("*.md", &data_root.display().to_string(), 5000).unwrap();
        assert!(result.contains("data/a.md"), "{result}");

        let repeated_prefix = glob("data/*.md", &data_root.display().to_string(), 5000).unwrap();
        assert!(
            repeated_prefix.contains("no files matched pattern 'data/*.md'"),
            "pattern is relative to path and should not match repeated prefix: {repeated_prefix}"
        );
        assert!(repeated_prefix.contains("a.md"), "{repeated_prefix}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_ignores_parent_gitignore_rules() {
        let parent = temp_dir("glob-parent-ignore");
        let workspace = parent.join("workspace");
        fs::create_dir_all(workspace.join("data")).unwrap();
        fs::write(parent.join(".gitignore"), "data/\n").unwrap();
        fs::write(workspace.join("data/a.md"), "a\n").unwrap();

        let result = glob("data/*.md", &workspace.display().to_string(), 5000).unwrap();

        assert!(result.contains("data/a.md"), "{result}");

        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn glob_directory_prefixed_pattern_overrides_local_gitignore_like_rg() {
        let dir = temp_dir("glob-dir-prefix-ignore");
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join(".gitignore"), "data/\n").unwrap();
        fs::write(dir.join("data/a.md"), "a\n").unwrap();

        let result = glob("data/*.md", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("data/a.md"), "{result}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_plain_pattern_matches_file_names_recursively_like_rg() {
        let dir = temp_dir("glob-plain");
        fs::create_dir_all(dir.join("docs/specs")).unwrap();
        fs::write(dir.join("root.docx"), "doc").unwrap();
        fs::write(dir.join("docs/specs/api.docx"), "doc").unwrap();
        fs::write(dir.join("docs/specs/notes.txt"), "note").unwrap();

        let result = glob("*.docx", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("root.docx"));
        assert!(result.contains("docs/specs/api.docx"));
        assert!(!result.contains("docs/specs/notes.txt"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_recursive_pattern_matches_nested_file_names() {
        let dir = temp_dir("glob-recursive");
        fs::create_dir_all(dir.join("src/bin")).unwrap();
        fs::write(dir.join("src/bin/main.rs"), "fn main() {}\n").unwrap();

        let result = glob("**/*.*", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("src/bin/main.rs"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_empty_result_includes_root_fallback() {
        let dir = temp_dir("glob-empty");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join("type_result.json"), "{}\n").unwrap();

        let result = glob("*.docx", &dir.display().to_string(), 5000).unwrap();

        assert!(
            result.contains("no files matched pattern '*.docx'"),
            "{result}"
        );
        assert!(result.contains("data/"), "{result}");
        assert!(result.contains("type_result.json"), "{result}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_empty_pattern_errors() {
        assert!(grep("", ".", "", None, 5000, 1000).is_err());
    }

    #[test]
    fn grep_basic_search() {
        let dir = std::env::temp_dir().join(format!("grep-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.txt"), "hello world\nfoo bar\n").unwrap();

        let result = grep("hello", &dir.display().to_string(), "", None, 5000, 1000).unwrap();
        assert!(result.contains("test.txt:1:hello world"), "{result}");
        assert!(!result.contains("foo"), "{result}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_with_context() {
        let dir = std::env::temp_dir().join(format!("grep-ctx-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.txt"), "line1\nline2\nmatch\nline4\nline5\n").unwrap();

        let result = grep("match", &dir.display().to_string(), "", Some(1), 5000, 1000).unwrap();
        assert!(result.contains("test.txt-2-line2"), "{result}");
        assert!(result.contains("test.txt:3:match"), "{result}");
        assert!(result.contains("test.txt-4-line4"), "{result}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_empty_result_matches_rg_empty_stdout() {
        let dir = temp_dir("grep-empty");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.txt"), "hello world\n").unwrap();

        let result = grep("missing", &dir.display().to_string(), "", None, 5000, 1000).unwrap();
        assert!(result.is_empty(), "{result}");

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

        let result = grep("match", &dir.display().to_string(), "", None, 5000, 1000).unwrap();
        assert!(result.contains("truncated"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_with_file_glob() {
        let dir = std::env::temp_dir().join(format!("grep-glob-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data.txt"), "secret\n").unwrap();
        fs::write(dir.join("data.md"), "secret\n").unwrap();

        let result = grep(
            "secret",
            &dir.display().to_string(),
            "*.txt",
            None,
            5000,
            1000,
        )
        .unwrap();
        assert!(result.contains("data.txt"));
        assert!(!result.contains("data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_file_glob_uses_rg_override_semantics() {
        let dir = temp_dir("grep-glob-path");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("root.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.md"), "secret\n").unwrap();

        let result = grep(
            "secret",
            &dir.display().to_string(),
            "*.txt",
            None,
            5000,
            1000,
        )
        .unwrap();

        assert!(result.contains("root.txt"));
        assert!(result.contains("nested/data.txt"));
        assert!(!result.contains("nested/data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_file_glob_recursive_pattern_matches_nested_files() {
        let dir = temp_dir("grep-glob-recursive");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/data.txt"), "secret\n").unwrap();
        fs::write(dir.join("nested/data.md"), "secret\n").unwrap();

        let result = grep(
            "secret",
            &dir.display().to_string(),
            "**/*.txt",
            None,
            5000,
            1000,
        )
        .unwrap();

        assert!(result.contains("nested/data.txt"));
        assert!(!result.contains("nested/data.md"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_ignores_parent_gitignore_rules() {
        let parent = temp_dir("grep-parent-ignore");
        let workspace = parent.join("workspace");
        fs::create_dir_all(workspace.join("data")).unwrap();
        fs::write(parent.join(".gitignore"), "data/\n").unwrap();
        fs::write(workspace.join("data/a.md"), "secret\n").unwrap();

        let result = grep(
            "secret",
            &workspace.display().to_string(),
            "data/*.md",
            None,
            5000,
            1000,
        )
        .unwrap();

        assert!(result.contains("data/a.md:1:secret"), "{result}");

        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn glob_explicit_pattern_can_match_hidden_files_like_rg() {
        let dir = temp_dir("glob-ignore");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".hidden.txt"), "hidden\n").unwrap();
        fs::write(dir.join("kept.txt"), "kept\n").unwrap();

        let result = glob("*.txt", &dir.display().to_string(), 5000).unwrap();

        assert!(result.contains("kept.txt"));
        assert!(result.contains(".hidden.txt"));

        fs::remove_dir_all(&dir).ok();
    }
}
