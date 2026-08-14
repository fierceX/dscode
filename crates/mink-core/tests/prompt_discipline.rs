//! A4：prompt 资产写作纪律的机械执行（AGENTS.md 不变式的测试形态）。
//!
//! 对 src/assets/prompts/**/*.md 逐文件执行：
//! 1. 每个 <critical> 块必须有 3-6 条 "- " bullet；
//! 2. 每条 bullet <= 12 英文词（空白切分计数）；
//! 3. bullet 禁词：token/budget/engine/internal（整词、大小写不敏感）；
//! 4. 占位符白名单（与 prompt/workflows.rs 的渲染调用同步）；
//! 5. 示例置尾：含代码围栏的资产，最后一个围栏必须位于最后一个
//!    </critical>/</anti-patterns> 之后（示例之后不得再有指令段）。

use std::path::{Path, PathBuf};

/// 占位符白名单——与 crates/mink-core/src/prompt/workflows.rs 的
/// 渲染调用保持同步；新增占位符必须同时加入此白名单。
const PLACEHOLDER_WHITELIST: &[&str] = &[
    "CLEAR_PROVIDER",
    "CONFIRM_PROVIDER",
    "DRAFT_PROVIDER",
    "EDIT_PROVIDER",
    "FUZZY_MODE",
    "FUZZY_THRESHOLD",
    "LARGE_FILE_GUIDANCE",
    "READ_PROVIDER",
    "SEARCH_PROVIDER",
    "SEEN_LINE_MODE",
    "TODO_ADVANCE_PROVIDER",
    "TODO_READ_PROVIDER",
    "TODO_WRITE_PROVIDER",
];

fn prompt_files() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/prompts");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("prompt asset dir readable") {
            let path = entry.expect("prompt asset entry readable").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no prompt assets found");
    files
}

fn check_critical_blocks(path: &Path, text: &str) {
    let mut block_count = 0usize;
    let mut parts = text.split("<critical>");
    parts.next(); // 首个 <critical> 之前的序言
    for part in parts {
        let body = part
            .split("</critical>")
            .next()
            .expect("critical block must close");
        block_count += 1;
        let bullets: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("- "))
            .collect();
        assert!(
            (3..=6).contains(&bullets.len()),
            "{} critical block #{block_count} has {} bullets (want 3-6)",
            path.display(),
            bullets.len()
        );
        for bullet in bullets {
            let words = bullet.split_whitespace().count();
            assert!(
                words <= 12,
                "{} bullet has {words} words (max 12): {bullet}",
                path.display()
            );
            for word in bullet.split_whitespace() {
                let trimmed: String = word.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                let lower = trimmed.to_ascii_lowercase();
                assert!(
                    !matches!(lower.as_str(), "token" | "budget" | "engine" | "internal"),
                    "{} bullet uses banned word {lower:?}: {bullet}",
                    path.display()
                );
            }
        }
    }
    assert!(
        block_count > 0,
        "{} has no <critical> block",
        path.display()
    );
}

fn check_placeholders(path: &Path, text: &str) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}").expect("placeholder must close");
        let name = &after[..end];
        assert!(
            PLACEHOLDER_WHITELIST.contains(&name),
            "{} uses unknown placeholder {{{{name}}}} (update PLACEHOLDER_WHITELIST and prompt/workflows.rs)",
            path.display()
        );
        rest = &after[end + 2..];
    }
}

fn check_examples_at_tail(path: &Path, text: &str) {
    let Some(fence) = text.rfind("```") else {
        return; // 无示例资产无置尾约束
    };
    let tag = text
        .rfind("</critical>")
        .into_iter()
        .chain(text.rfind("</anti-patterns>"))
        .max();
    if let Some(tag) = tag {
        assert!(
            fence > tag,
            "{} places example code before its final instruction block; examples must sit at the tail",
            path.display()
        );
    }
}

#[test]
fn prompt_assets_obey_writing_discipline() {
    for path in prompt_files() {
        let text = std::fs::read_to_string(&path).expect("prompt asset readable");
        check_critical_blocks(&path, &text);
        check_placeholders(&path, &text);
        check_examples_at_tail(&path, &text);
    }
}
