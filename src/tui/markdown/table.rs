use super::types::{TableAlign, TableRows};
use super::util::truncate_visual;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub(crate) fn parse_table(lines: &[&str]) -> Option<(TableRows, usize)> {
    if lines.len() < 2 {
        return None;
    }

    let header = parse_table_row(lines[0])?;
    let alignments = parse_table_alignment(lines[1], header.len())?;
    let mut rows = Vec::new();
    let mut consumed = 2usize;
    for line in lines.iter().skip(2) {
        let Some(row) = parse_table_row(line) else {
            break;
        };
        if row.len() != header.len() {
            break;
        }
        rows.push(row);
        consumed += 1;
    }

    Some((
        TableRows {
            header,
            alignments,
            rows,
        },
        consumed,
    ))
}

fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }

    let mut cells = split_escaped_pipes(trimmed);
    if trimmed.starts_with('|') && cells.first().is_some_and(|cell| cell.is_empty()) {
        cells.remove(0);
    }
    if ends_with_unescaped_pipe(trimmed) && cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }

    let cells: Vec<String> = cells
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect();
    (cells.len() >= 2).then_some(cells)
}

fn split_escaped_pipes(text: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
        {
            if next == '|' {
                cell.push('|');
                chars.next();
                continue;
            }
            cell.push(ch);
            cell.push(next);
            chars.next();
            continue;
        }
        if ch == '|' {
            cells.push(std::mem::take(&mut cell));
        } else {
            cell.push(ch);
        }
    }
    cells.push(cell);
    cells
}

fn ends_with_unescaped_pipe(text: &str) -> bool {
    if !text.ends_with('|') {
        return false;
    }
    let preceding_backslashes = text
        .chars()
        .rev()
        .skip(1)
        .take_while(|&ch| ch == '\\')
        .count();
    preceding_backslashes % 2 == 0
}

fn parse_table_alignment(line: &str, expected_cols: usize) -> Option<Vec<TableAlign>> {
    let cells = parse_table_row(line)?;
    if cells.len() != expected_cols {
        return None;
    }

    let mut alignments = Vec::with_capacity(cells.len());
    for cell in cells {
        let marker = cell.trim();
        if !marker.contains('-') || !marker.chars().all(|ch| ch == '-' || ch == ':') {
            return None;
        }
        let left = marker.starts_with(':');
        let right = marker.ends_with(':');
        alignments.push(match (left, right) {
            (true, true) => TableAlign::Center,
            (false, true) => TableAlign::Right,
            _ => TableAlign::Left,
        });
    }
    Some(alignments)
}

pub(crate) fn render_table(lines: &mut Vec<Line<'static>>, table: &TableRows, base: Style) {
    const MAX_CELL_WIDTH: usize = 32;
    if table.header.is_empty()
        || table.alignments.len() != table.header.len()
        || table.rows.iter().any(|row| row.len() != table.header.len())
    {
        render_malformed_table(lines, table, base);
        return;
    }

    let mut widths: Vec<usize> = table
        .header
        .iter()
        .map(|cell| unicode_width::UnicodeWidthStr::width(cell.as_str()).min(MAX_CELL_WIDTH))
        .collect();

    for row in &table.rows {
        for (idx, cell) in row.iter().enumerate() {
            let width = unicode_width::UnicodeWidthStr::width(cell.as_str()).min(MAX_CELL_WIDTH);
            widths[idx] = widths[idx].max(width);
        }
    }

    lines.push(table_line(
        &table.header,
        &table.alignments,
        &widths,
        base.add_modifier(Modifier::BOLD).fg(Color::Cyan),
    ));
    lines.push(Line::from(Span::styled(
        table_separator(&widths),
        Style::default().fg(Color::DarkGray),
    )));
    for row in &table.rows {
        lines.push(table_line(row, &table.alignments, &widths, base));
    }
}

fn render_malformed_table(lines: &mut Vec<Line<'static>>, table: &TableRows, base: Style) {
    let mut raw_rows = Vec::with_capacity(table.rows.len() + 1);
    raw_rows.push(table.header.join(" | "));
    raw_rows.extend(table.rows.iter().map(|row| row.join(" | ")));
    for row in raw_rows {
        lines.push(Line::from(Span::styled(row, base)));
    }
}

fn table_line(
    cells: &[String],
    alignments: &[TableAlign],
    widths: &[usize],
    style: Style,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, cell) in cells.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " │ ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            align_cell(cell, widths[idx], alignments[idx]),
            style,
        ));
    }
    Line::from(spans)
}

fn table_separator(widths: &[usize]) -> String {
    widths
        .iter()
        .map(|width| "─".repeat(*width))
        .collect::<Vec<_>>()
        .join("─┼─")
}

fn align_cell(cell: &str, width: usize, align: TableAlign) -> String {
    let text = truncate_visual(cell, width);
    let text_width = unicode_width::UnicodeWidthStr::width(text.as_str());
    let pad = width.saturating_sub(text_width);
    match align {
        TableAlign::Left => format!("{text}{}", " ".repeat(pad)),
        TableAlign::Right => format!("{}{text}", " ".repeat(pad)),
        TableAlign::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
    }
}
