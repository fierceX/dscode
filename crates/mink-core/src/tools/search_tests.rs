use super::*;
use crate::tools::runner::ToolExec;
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

#[tokio::test]
async fn grep_searches_registered_resource_content() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("grep-session-history").await?;
    ctx.store
        .add_user("remember the cobalt migration decision")
        .await?;
    let tool_ctx = crate::context::ToolContext::from(ctx.as_ref());

    let result = GrepTool.execute(
        &serde_json::json!({
            "pattern": "cobalt migration",
            "path": "session://current/history",
            "context": 1
        }),
        &tool_ctx,
    )?;

    assert!(result.content.contains("session://current/history"));
    assert!(
        result
            .content
            .contains("remember the cobalt migration decision")
    );
    Ok(())
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
