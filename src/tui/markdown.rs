use crate::tui::state::MsgKind;
use crate::tui::theme;
use ratatui::{
    style::Style,
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
        MsgKind::StreamThinking => theme::muted(),
        MsgKind::Text | MsgKind::StreamText => theme::text(),
        MsgKind::ToolCall => theme::primary_bold(),
        MsgKind::Error => theme::error(),
        MsgKind::Info => theme::info(),
        MsgKind::SubAgent => theme::sub_agent(),
        MsgKind::ToolResult => theme::muted(),
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

#[cfg(test)]
pub(crate) fn push_msg(lines: &mut Vec<Line<'static>>, text: &str, kind: MsgKind) {
    push_msg_with_width(lines, text, kind, 80);
}

pub(crate) fn push_msg_with_width(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    kind: MsgKind,
    max_width: u16,
) {
    if text.is_empty() {
        return;
    }

    let normalized = normalize_markdown_input(text, false);
    let mode = mode_for_kind(kind, &normalized);
    match mode {
        MarkdownMode::Full => render_markdown(lines, &normalized, style_for_kind(kind), max_width),
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

pub(crate) fn render_md_with_tables_with_width(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    max_width: u16,
) {
    let normalized = normalize_markdown_input(text, false);
    render_markdown(lines, &normalized, Style::default(), max_width);
}
