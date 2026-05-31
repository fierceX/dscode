use crate::tui::state::MsgKind;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

mod block;
mod diff;
mod inline;
mod normalize;
mod table;
mod types;
mod util;

#[cfg(test)]
pub(crate) use block::parse_blocks;
pub(crate) use block::render_markdown;
pub(crate) use normalize::normalize_markdown_input;
#[cfg(test)]
pub(crate) use normalize::strip_ansi;
#[cfg(test)]
pub(crate) use table::render_table;
#[cfg(test)]
pub(crate) use types::{InlineNode, MdBlock, TableAlign, TableRows};
pub(crate) use util::{truncate_visual, wrap_lines_word};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MarkdownMode {
    Full,
    Plain,
    ToolOutput,
    Diff,
}

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

fn mode_for_kind(kind: MsgKind, text: &str) -> MarkdownMode {
    match kind {
        MsgKind::Text | MsgKind::StreamText => MarkdownMode::Full,
        MsgKind::ToolResult if diff::is_diff_like(text) => MarkdownMode::Diff,
        MsgKind::ToolResult => MarkdownMode::ToolOutput,
        _ => MarkdownMode::Plain,
    }
}

pub(crate) fn push_msg(lines: &mut Vec<Line<'static>>, text: &str, kind: MsgKind) {
    if text.is_empty() {
        return;
    }

    let normalized = normalize_markdown_input(text, false);
    let mode = mode_for_kind(kind, &normalized);
    match mode {
        MarkdownMode::Full => render_markdown(lines, &normalized, style_for_kind(kind)),
        MarkdownMode::Diff => diff::render_diff(lines, &normalized),
        MarkdownMode::Plain | MarkdownMode::ToolOutput => {
            push_plain(lines, &normalized, style_for_kind(kind));
        }
    }
}

fn push_plain(lines: &mut Vec<Line<'static>>, text: &str, style: Style) {
    for raw in text.split('\n') {
        lines.push(Line::from(Span::styled(raw.to_string(), style)));
    }
}

pub(crate) fn render_md_with_tables(lines: &mut Vec<Line<'static>>, text: &str) {
    let normalized = normalize_markdown_input(text, false);
    render_markdown(lines, &normalized, Style::default());
}
