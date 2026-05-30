use crate::tui::state::MsgKind;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

pub(crate) fn style_for_kind(kind: MsgKind) -> Style {
    match kind {
        MsgKind::StreamThinking => Style::default().fg(Color::Rgb(139, 139, 139)),
        MsgKind::Text | MsgKind::StreamText => Style::default(),
        MsgKind::ToolCall => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        MsgKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        MsgKind::Info => Style::default().fg(Color::Yellow),
        MsgKind::SubAgent => Style::default().fg(Color::Magenta),
        MsgKind::ToolResult => Style::default().fg(Color::Rgb(100, 100, 100)),
    }
}

pub(crate) fn wrap_lines_word(lines: &[Line<'static>], max_w: u16) -> Vec<Line<'static>> {
    let mw = max_w.max(1) as usize;
    let mut out = Vec::new();
    for line in lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let style = line.spans.first().map(|s| s.style).unwrap_or_default();

        if text.is_empty() {
            out.push(Line::from(""));
            continue;
        }

        let mut cur = String::new();
        let mut cur_w = 0usize;

        for word in text.split_inclusive(' ') {
            let word_w = unicode_width::UnicodeWidthStr::width(word);
            if cur_w == 0 && word_w > mw {
                for ch in word.chars() {
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cur_w + ch_w > mw && !cur.is_empty() {
                        out.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
                        cur_w = 0;
                    }
                    cur.push(ch);
                    cur_w += ch_w;
                }
            } else if cur_w + word_w > mw {
                out.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
                let trimmed = word.trim_start_matches(' ');
                cur_w = unicode_width::UnicodeWidthStr::width(trimmed);
                cur.push_str(trimmed);
            } else {
                cur.push_str(word);
                cur_w += word_w;
            }
        }
        if !cur.is_empty() {
            out.push(Line::from(Span::styled(cur, style)));
        }
    }
    out
}

fn push_md(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.is_empty() {
        return;
    }
    let md: Text<'_> = tui_markdown::from_str(text);
    for line in md.lines {
        let spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| {
                let mut style = s.style;
                if style.fg.is_none() {
                    style = style.fg(Color::Reset);
                }
                Span::styled(s.content.to_string(), style)
            })
            .collect();
        lines.push(Line::from(spans));
    }
}

pub(crate) fn push_msg(lines: &mut Vec<Line<'static>>, text: &str, kind: MsgKind) {
    if text.is_empty() {
        return;
    }
    match kind {
        MsgKind::Text | MsgKind::StreamText => {
            render_md_with_tables(lines, text);
        }
        MsgKind::ToolResult => {
            let is_diff = text.lines().take(3).any(|l| {
                let t = l.trim();
                t.starts_with("--- ") || t.starts_with("+++ ") || t.starts_with("@@")
            });
            if is_diff {
                render_diff(lines, text);
            } else {
                let base = style_for_kind(kind);
                for raw in text.split('\n') {
                    lines.push(Line::from(Span::styled(raw.to_string(), base)));
                }
            }
        }
        _ => {
            let base = style_for_kind(kind);
            for raw in text.split('\n') {
                lines.push(Line::from(Span::styled(raw.to_string(), base)));
            }
        }
    }
}

fn render_diff(lines: &mut Vec<Line<'static>>, text: &str) {
    let gray = Style::default().fg(Color::Rgb(100, 100, 100));
    let red = Style::default().fg(Color::Rgb(255, 100, 100));
    let green = Style::default().fg(Color::Rgb(100, 200, 100));
    let cyan = Style::default().fg(Color::Cyan);
    let yellow = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    for raw in text.split('\n') {
        let clean = strip_ansi(raw);
        let trimmed = clean.trim();
        let style = if trimmed.starts_with("--- ") || trimmed.starts_with("+++ ") {
            yellow
        } else if trimmed.starts_with("@@") {
            cyan
        } else if clean.starts_with('-') && !clean.starts_with("---") {
            red
        } else if clean.starts_with('+') && !clean.starts_with("+++") {
            green
        } else {
            gray
        };
        lines.push(Line::from(Span::styled(clean, style)));
    }
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn render_md_with_tables(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.contains("|---") || text.contains("| --") {
        let mut md_buf = String::new();
        for raw in text.split('\n') {
            let trimmed = raw.trim();
            let is_table = trimmed.starts_with('|') || trimmed.contains("---");
            if is_table {
                if !md_buf.is_empty() {
                    push_md(lines, &md_buf);
                    md_buf.clear();
                }
                lines.push(Line::from(Span::raw(format!("{raw}\n"))));
            } else {
                if !md_buf.is_empty() {
                    md_buf.push('\n');
                }
                md_buf.push_str(raw);
            }
        }
        if !md_buf.is_empty() {
            push_md(lines, &md_buf);
        }
    } else {
        push_md(lines, text);
    }
}
