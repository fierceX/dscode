//! Hashline patch parser — 手写 Tokenizer + Executor 状态机。
//!
//! 将 LLM 输出的 anchored patch 文本解析为 `ParsedPatch`，
//! 兼容 mink 现有的 `PatchHunk` 类型。
//!
//! 与旧 `parse_anchored_patch` 的关键区别：
//! - 遇到非 `+` 前缀的 body 行（如 markdown 表格 `|`），作为 body 行接受（附带警告）
//! - 空白行在 hunk body 中被跳过而非终止 body 收集
//! - 两阶段设计：Tokenizer 逐行分类 → Executor 状态机

use crate::tools::file::PatchHunk;
use anyhow::{Result, bail};

// ── Token 定义 ────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum BlockTarget {
    Replace { start: usize, end: usize },
    Delete { start: usize, end: usize },
    InsertBefore { line: usize },
    InsertAfter { line: usize },
    InsertHead,
    InsertTail,
}

#[derive(Debug)]
enum Token {
    Header {
        path: String,
        tag: String,
    },
    OpBlock(BlockTarget),
    PayloadLiteral(String),
    Raw(String),
    Blank,
}

// ── Tokenizer ──────────────────────────────────────────────────────────────

struct Tokenizer;

impl Tokenizer {
    fn classify_line(line: &str) -> Token {
        let trimmed = line.trim();

        // 空行
        if trimmed.is_empty() {
            return Token::Blank;
        }

        // @PATH#TAG header
        if let Some(rest) = trimmed.strip_prefix('@') {
            if let Some((path, tag)) = rest.rsplit_once('#') {
                if !path.is_empty() && !tag.is_empty() && tag.len() <= 16 {
                    return Token::Header {
                        path: path.to_string(),
                        tag: tag.to_string(),
                    };
                }
            }
        }

        // + 前缀 body 行
        if line.starts_with('+') {
            return Token::PayloadLiteral(line[1..].to_string());
        }

        // Hunk 头部: replace / delete / insert
        if let Some(target) = try_parse_hunk_header(trimmed) {
            return Token::OpBlock(target);
        }

        // 其他内容行
        Token::Raw(line.to_string())
    }
}

fn try_parse_hunk_header(s: &str) -> Option<BlockTarget> {
    // replace N..M: 或 replace N:
    if let Some(range) = s.strip_prefix("replace ") {
        let range = range.strip_suffix(':')?;
        let (start, end) = parse_line_range(range).or_else(|| parse_single_line(range))?;
        return Some(BlockTarget::Replace { start, end });
    }

    // delete N..M 或 delete N
    if let Some(range) = s.strip_prefix("delete ") {
        let range = range.strip_suffix(':').unwrap_or(range);
        let (start, end) = parse_line_range(range).or_else(|| parse_single_line(range))?;
        return Some(BlockTarget::Delete { start, end });
    }

    // insert ...
    if let Some(target) = s.strip_prefix("insert ") {
        let target = target.strip_suffix(':').unwrap_or(target);
        // insert before N:
        if let Some(n) = target.strip_prefix("before ") {
            let line: usize = n.trim().parse().ok()?;
            return Some(BlockTarget::InsertBefore { line });
        }
        // insert after N:
        if let Some(n) = target.strip_prefix("after ") {
            let line: usize = n.trim().parse().ok()?;
            return Some(BlockTarget::InsertAfter { line });
        }
        // insert head:
        if target.trim() == "head" {
            return Some(BlockTarget::InsertHead);
        }
        // insert tail:
        if target.trim() == "tail" {
            return Some(BlockTarget::InsertTail);
        }
    }

    None
}

/// 解析 "N..M" 或 "N.. M" 形式的行范围
fn parse_line_range(s: &str) -> Option<(usize, usize)> {
    let s = s.trim();
    let dots = s.find("..")?;
    let start: usize = s[..dots].trim().parse().ok()?;
    let end: usize = s[dots + 2..].trim().parse().ok()?;
    if start >= 1 {
        Some((start, end))
    } else {
        None
    }
}

/// 解析单个行号 "N"
fn parse_single_line(s: &str) -> Option<(usize, usize)> {
    let n: usize = s.trim().parse().ok()?;
    if n >= 1 {
        Some((n, n))
    } else {
        None
    }
}

// ── Executor 状态机 ───────────────────────────────────────────────────────

struct Executor {
    path: Option<String>,
    tag: Option<String>,
    hunks: Vec<PatchHunk>,
    pending: Option<PendingHunk>,
    warnings: Vec<String>,
    line_no: usize,
}

struct PendingHunk {
    target: BlockTarget,
    body: Vec<String>,
    header_line: usize,
}

impl Executor {
    fn new() -> Self {
        Executor {
            path: None,
            tag: None,
            hunks: Vec::new(),
            pending: None,
            warnings: Vec::new(),
            line_no: 0,
        }
    }

    fn feed(&mut self, token: Token) -> Result<()> {
        self.line_no += 1;
        match token {
            Token::Header { path, tag } => {
                self.flush_pending()?;
                self.path = Some(path);
                self.tag = Some(tag);
            }
            Token::OpBlock(target) => {
                // 校验范围合法性
                if let BlockTarget::Replace { start, end }
                | BlockTarget::Delete { start, end } = &target
                {
                    if end < start {
                        bail!(
                            "Error: line {}: range {}..{} ends before it starts",
                            self.line_no,
                            start,
                            end
                        );
                    }
                }
                self.flush_pending()?;
                let header_line = self.line_no;
                self.pending = Some(PendingHunk {
                    target,
                    body: Vec::new(),
                    header_line,
                });
            }
            Token::PayloadLiteral(text) => {
                if let Some(ref pending) = self.pending {
                    match pending.target {
                        BlockTarget::Delete { .. } => {
                            bail!(
                                "Error: line {}: delete does not take body rows",
                                self.line_no
                            );
                        }
                        _ => {}
                    }
                }
                if let Some(ref mut pending) = self.pending {
                    pending.body.push(text);
                } else {
                    bail!(
                        "Error: line {}: payload line has no preceding hunk header",
                        self.line_no
                    );
                }
            }
            Token::Raw(text) => {
                let trimmed = text.trim();
                // 完全空行 → 跳过
                if trimmed.is_empty() {
                    return Ok(());
                }
                if let Some(ref mut pending) = self.pending {
                    // 非 + 行作为 body 行处理（带警告）
                    // 跳过 delete 的 body 检查
                    match pending.target {
                        BlockTarget::Delete { .. } => {
                            bail!(
                                "Error: line {}: delete does not take body rows",
                                self.line_no
                            );
                        }
                        _ => {
                            if !self
                                .warnings
                                .contains(&BARE_BODY_AUTO_PIPED_WARNING.to_string())
                            {
                                self.warnings.push(BARE_BODY_AUTO_PIPED_WARNING.to_string());
                            }
                            pending.body.push(trimmed.to_string());
                        }
                    }
                } else {
                    bail!(
                        "Error: line {}: invalid patch hunk '{}'",
                        self.line_no,
                        trimmed
                    );
                }
            }
            Token::Blank => {
                // 空白行：不清除 pending，允许 hunk body 中有空行
            }
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let header_line = pending.header_line;
        match pending.target {
            BlockTarget::Replace { start, end } => {
                if pending.body.is_empty() {
                    bail!("Error: line {header_line}: replace hunk requires at least one body row");
                }
                // 检查是否有 body 行以 `|` 开头（markdown 表格常见错误）
                self.hunks.push(PatchHunk::Replace {
                    start,
                    end,
                    body: pending.body,
                });
            }
            BlockTarget::Delete { start, end } => {
                self.hunks.push(PatchHunk::Delete { start, end });
            }
            BlockTarget::InsertBefore { line } => {
                if pending.body.is_empty() {
                    bail!("Error: line {header_line}: insert hunk requires at least one body row");
                }
                self.hunks.push(PatchHunk::InsertBefore {
                    line,
                    body: pending.body,
                });
            }
            BlockTarget::InsertAfter { line } => {
                if pending.body.is_empty() {
                    bail!("Error: line {header_line}: insert hunk requires at least one body row");
                }
                self.hunks.push(PatchHunk::InsertAfter {
                    line,
                    body: pending.body,
                });
            }
            BlockTarget::InsertHead => {
                if pending.body.is_empty() {
                    bail!("Error: line {header_line}: insert hunk requires at least one body row");
                }
                self.hunks.push(PatchHunk::InsertHead {
                    body: pending.body,
                });
            }
            BlockTarget::InsertTail => {
                if pending.body.is_empty() {
                    bail!("Error: line {header_line}: insert hunk requires at least one body row");
                }
                self.hunks.push(PatchHunk::InsertTail {
                    body: pending.body,
                });
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        self.flush_pending()?;
        if self.hunks.is_empty() {
            bail!("Error: patch contains no edit hunks");
        }
        Ok(())
    }
}

const BARE_BODY_AUTO_PIPED_WARNING: &str = "body row missing '+' prefix, treated as literal";

// ── 公开 API ──────────────────────────────────────────────────────────────

/// 解析 hashline patch 文本，返回 `ParsedPatch`。
///
/// 与旧的 `parse_anchored_patch` 输出类型完全兼容。
pub(crate) fn parse_patch(input: &str) -> Result<(super::file::ParsedPatch, Vec<String>)> {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() || lines.iter().all(|l| l.trim().is_empty()) {
        bail!("Error: patch is empty");
    }

    let mut executor = Executor::new();

    for line in &lines {
        let token = Tokenizer::classify_line(line);
        executor.feed(token)?;
    }

    executor.end()?;

    let path = executor.path.ok_or_else(|| {
        anyhow::anyhow!(
            "Error: patch must begin with @PATH#TAG"
        )
    })?;
    let tag = executor.tag.ok_or_else(|| {
        anyhow::anyhow!(
            "Error: patch header must be @PATH#TAG"
        )
    })?;

    let parsed = super::file::ParsedPatch {
        path,
        tag,
        hunks: executor.hunks,
    };

    Ok((parsed, executor.warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::file::ParsedPatch;

    fn parse_success(input: &str) -> (ParsedPatch, Vec<String>) {
        parse_patch(input).unwrap()
    }

    fn parse_error(input: &str) -> String {
        parse_patch(input).unwrap_err().to_string()
    }

    #[test]
    fn replace_single_line() {
        let (p, w) = parse_success("@a.rs#0A\nreplace 2:\n+two");
        assert_eq!(p.path, "a.rs");
        assert_eq!(p.tag, "0A");
        assert_eq!(p.hunks.len(), 1);
        assert!(matches!(&p.hunks[0], PatchHunk::Replace { start: 2, end: 2, .. }));
        assert!(w.is_empty());
    }

    #[test]
    fn replace_range() {
        let (p, w) = parse_success("@a.rs#0A\nreplace 2..4:\n+new2\n+new3");
        assert_eq!(p.hunks.len(), 1);
        assert!(matches!(&p.hunks[0], PatchHunk::Replace { start: 2, end: 4, .. }));
        assert!(w.is_empty());
    }

    #[test]
    fn delete_single() {
        let (p, w) = parse_success("@f#FF\nreplace 1:\n+hello\n\ndelete 3..5\n\nreplace 7:\n+world");
        assert_eq!(p.hunks.len(), 3);
        assert!(w.is_empty());
    }

    #[test]
    fn insert_before_after_head_tail() {
        let (p, w) = parse_success(
            "@f#00\n\
             insert before 1:\n+top\n\
             insert after 1:\n+after\n\
             insert head:\n+head\n\
             insert tail:\n+tail",
        );
        assert_eq!(p.hunks.len(), 4);
        assert!(w.is_empty());
    }

    #[test]
    fn bare_body_line_accepted_with_warning() {
        let (p, w) = parse_success("@f#00\nreplace 1:\n| table content");
        assert_eq!(p.hunks.len(), 1);
        if let PatchHunk::Replace { body, .. } = &p.hunks[0] {
            assert_eq!(body[0], "| table content");
        } else {
            panic!("expected Replace");
        }
        assert!(w.iter().any(|m| m.contains("body row missing")));
    }

    #[test]
    fn raw_body_lines_accepted() {
        let (p, _w) = parse_success("@f#00\nreplace 1..3:\n+a\n| pipe line\n+c\nplain text");
        assert_eq!(p.hunks.len(), 1);
        if let PatchHunk::Replace { body, .. } = &p.hunks[0] {
            assert_eq!(body, &["a", "| pipe line", "c", "plain text"]);
        } else {
            panic!("expected Replace");
        }
    }

    #[test]
    fn blank_lines_in_body_skipped() {
        // +a, + (empty), + (empty), c (raw), +d
        let (p, w) = parse_success("@f#00\nreplace 1..3:\n+a\n+\n+\nc\n+d");
        assert_eq!(p.hunks.len(), 1);
        if let PatchHunk::Replace { body, .. } = &p.hunks[0] {
            // 两个 `+` 产生两个空字符串，`c` 是 raw body 行
            assert_eq!(body, &["a", "", "", "c", "d"]);
        } else {
            panic!("expected Replace");
        }
        // `c` 是 raw body 行，触发警告
        assert!(w.iter().any(|m| m.contains("body row missing")));
    }

    #[test]
    fn payload_without_header_errors() {
        let err = parse_error("+orphan");
        assert!(err.contains("no preceding hunk header"));
    }

    #[test]
    fn empty_patch_errors() {
        let err = parse_error("");
        assert!(err.contains("patch is empty"));
    }

    #[test]
    fn missing_header_errors() {
        let err = parse_error("replace 1:\n+hello");
        assert!(err.contains("must begin with @PATH#TAG"));
    }

    #[test]
    fn delete_takes_no_body() {
        let err = parse_error("@f#00\ndelete 3\n+garbage");
        assert!(err.contains("delete does not take body rows"));
    }

    #[test]
    fn replace_empty_body_errors() {
        let err = parse_error("@f#00\nreplace 1:");
        assert!(err.contains("replace hunk requires at least one body row"));
    }

    #[test]
    fn range_reversed_errors() {
        let err = parse_error("@f#00\nreplace 5..3:\n+body");
        assert!(err.contains("ends before it starts"));
    }

    #[test]
    fn multiple_hunks() {
        let (p, w) = parse_success(
            "@f#00\n\
             replace 1:\n+first\n\
             delete 3\n\
             insert tail:\n+tail",
        );
        assert_eq!(p.hunks.len(), 3);
        assert!(w.is_empty());
    }

    #[test]
    fn insert_tail_requires_body() {
        let err = parse_error("@f#00\ninsert tail:");
        assert!(err.contains("insert hunk requires at least one body row"));
    }
}
