use crate::tui::sanitize::{normalize_tui_input, sanitize_tui_text};
use crate::tui::state::TranscriptKind;
use crate::tui::theme;
use ratatui::{
    style::Style,
    text::{Line, Span},
};

mod block;
mod diff;
mod inline;
mod table;
mod types;
mod util;

#[cfg(test)]
pub(crate) use block::parse_blocks;
pub(crate) use block::render_markdown;
pub(crate) fn normalize_markdown_input(input: &str, preserve_ansi: bool) -> String {
    if preserve_ansi {
        normalize_tui_input(input)
    } else {
        sanitize_tui_text(input)
    }
}
#[cfg(test)]
pub(crate) use crate::tui::sanitize::strip_ansi;
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

pub(crate) fn style_for_kind(kind: TranscriptKind) -> Style {
    match kind {
        TranscriptKind::StreamThinking => theme::muted(),
        TranscriptKind::Text | TranscriptKind::StreamText => theme::text(),
        TranscriptKind::Tool => theme::muted(),
        TranscriptKind::Error => theme::error(),
        TranscriptKind::Info => theme::info(),
        TranscriptKind::SubAgent => theme::sub_agent(),
    }
}

/// Tools whose output may contain diff markers (`---`, `+++`, `@@`) and
/// benefit from syntax-highlighted diff rendering.
///
/// Only these tools are eligible for `MarkdownMode::Diff`. Other tools
/// (Read, Write, Glob, SubAgent, ...) are excluded because
/// their output is either structured text or raw file content that should
/// not be reinterpreted as a diff — even if the text coincidentally
/// contains diff-like lines (e.g. Read of a YAML front-matter file).
fn is_diff_eligible(tool_name: Option<&str>) -> bool {
    matches!(
        tool_name,
        Some("Edit" | "Bash" | "Python" | "PythonSandbox")
    )
}

fn mode_for_kind(kind: TranscriptKind, text: &str, tool_name: Option<&str>) -> MarkdownMode {
    match kind {
        TranscriptKind::Text | TranscriptKind::StreamText => MarkdownMode::Full,
        TranscriptKind::Tool if is_diff_eligible(tool_name) && diff::is_diff_like(text) => {
            MarkdownMode::Diff
        }
        TranscriptKind::Tool => MarkdownMode::ToolOutput,
        _ => MarkdownMode::Plain,
    }
}

#[cfg(test)]
pub(crate) fn push_msg(lines: &mut Vec<Line<'static>>, text: &str, kind: TranscriptKind) {
    push_msg_with_width(lines, text, kind, 80, None);
}

#[cfg(test)]
pub(crate) fn push_msg_with_tool(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    kind: TranscriptKind,
    tool_name: &str,
) {
    push_msg_with_width(lines, text, kind, 80, Some(tool_name));
}

pub(crate) fn push_msg_with_width(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    kind: TranscriptKind,
    max_width: u16,
    tool_name: Option<&str>,
) {
    if text.is_empty() {
        return;
    }

    let normalized = normalize_markdown_input(text, false);
    let mode = mode_for_kind(kind, &normalized, tool_name);
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
