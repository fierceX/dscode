use super::inline::{parse_inline, render_inline_spans};
use super::table::{parse_table, render_table};
use super::types::MdBlock;
use crate::tui::theme;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(crate) fn render_markdown(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    base: Style,
    max_width: u16,
) {
    let blocks = parse_blocks(text);
    render_blocks(lines, &blocks, base, max_width);
}

pub(crate) fn parse_blocks(text: &str) -> Vec<MdBlock> {
    // `str::lines` preserves intentional blank lines but does not manufacture
    // an extra Markdown block solely because a streamed fragment ends in `\n`.
    let raw_lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut in_code = false;
    let mut code_lang: Option<String> = None;
    let mut code_lines: Vec<String> = Vec::new();
    let mut idx = 0usize;
    while idx < raw_lines.len() {
        let raw = raw_lines[idx];
        let trimmed = raw.trim_start();
        if let Some(lang) = fence_lang(trimmed) {
            if in_code {
                blocks.push(MdBlock::CodeBlock {
                    lang: code_lang.take(),
                    lines: std::mem::take(&mut code_lines),
                });
                in_code = false;
            } else {
                in_code = true;
                code_lang = (!lang.is_empty()).then(|| lang.to_string());
            }
            idx += 1;
            continue;
        }

        if in_code {
            code_lines.push(raw.to_string());
            idx += 1;
            continue;
        }

        if raw.trim().is_empty() {
            blocks.push(MdBlock::Blank);
            idx += 1;
            continue;
        }

        if let Some((table_lines, consumed)) = parse_table(&raw_lines[idx..]) {
            blocks.push(MdBlock::Table(table_lines));
            idx += consumed;
            continue;
        }

        if let Some((level, content)) = parse_heading(raw) {
            blocks.push(MdBlock::Heading {
                level,
                content: parse_inline(content),
            });
            idx += 1;
            continue;
        }

        if let Some(content) = raw.trim_start().strip_prefix("> ") {
            blocks.push(MdBlock::BlockQuote(parse_inline(content)));
            idx += 1;
            continue;
        }

        if let Some((marker, content)) = parse_list_item(raw) {
            blocks.push(MdBlock::ListItem {
                marker: marker.to_string(),
                content: parse_inline(content),
            });
            idx += 1;
            continue;
        }

        blocks.push(MdBlock::Paragraph(parse_inline(raw)));
        idx += 1;
    }
    if in_code {
        blocks.push(MdBlock::CodeBlock {
            lang: code_lang,
            lines: code_lines,
        });
    }
    blocks
}

fn render_blocks(lines: &mut Vec<Line<'static>>, blocks: &[MdBlock], base: Style, max_width: u16) {
    for block in blocks {
        match block {
            MdBlock::Blank => lines.push(Line::from("")),
            MdBlock::Paragraph(content) => {
                lines.push(Line::from(render_inline_spans(content, base)))
            }
            MdBlock::Heading { level, content } => {
                let style = if *level == 1 {
                    theme::primary_bold()
                } else {
                    base.add_modifier(Modifier::BOLD)
                };
                lines.push(Line::from(render_inline_spans(content, style)));
            }
            MdBlock::BlockQuote(content) => {
                let mut spans = vec![Span::styled("| ", theme::muted())];
                spans.extend(render_inline_spans(content, theme::muted()));
                lines.push(Line::from(spans));
            }
            MdBlock::ListItem { marker, content } => {
                let mut spans = vec![Span::styled(marker.clone(), theme::info()), Span::raw(" ")];
                spans.extend(render_inline_spans(content, base));
                lines.push(Line::from(spans));
            }
            MdBlock::CodeBlock { lang, lines: code } => {
                if let Some(lang) = lang {
                    lines.push(Line::from(Span::styled(
                        format!("-- {} --", lang),
                        theme::muted(),
                    )));
                }
                for raw in code {
                    lines.push(Line::from(Span::styled(raw.clone(), theme::muted())));
                }
            }
            MdBlock::Table(table) => render_table(lines, table, base, max_width),
        }
    }
}

fn fence_lang(line: &str) -> Option<&str> {
    line.strip_prefix("```")
        .or_else(|| line.strip_prefix("~~~"))
        .map(str::trim)
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&ch| ch == '#').count();
    if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
        Some((level, trimmed[level + 1..].trim()))
    } else {
        None
    }
}

fn parse_list_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if let Some(content) = trimmed.strip_prefix("- ") {
        return Some(("-", content));
    }
    if let Some(content) = trimmed.strip_prefix("* ") {
        return Some(("*", content));
    }
    if let Some(content) = trimmed.strip_prefix("+ ") {
        return Some(("+", content));
    }
    let dot = trimmed.find(". ")?;
    if dot > 0 && trimmed[..dot].chars().all(|ch| ch.is_ascii_digit()) {
        return Some((&trimmed[..=dot], &trimmed[dot + 2..]));
    }
    None
}
