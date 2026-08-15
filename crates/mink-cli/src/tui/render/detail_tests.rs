use super::*;
use ratatui::style::{Color, Style};

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn detail_lines_wrap_before_viewport_scrolling() {
    let style = Style::default().fg(Color::Cyan);
    let logical = vec![
        Line::from(Span::styled("计划内容中文测试", style)),
        Line::from("todo-T0001-abcdefghijk"),
        Line::from(r#"{"artifact":"abcdefghijklmnopqrstuvwxyz"}"#),
    ];

    let wrapped = wrap_lines_word(&logical, 8);

    assert!(wrapped.len() > logical.len());
    assert!(
        wrapped
            .iter()
            .all(|line| { unicode_width::UnicodeWidthStr::width(line_text(line).as_str()) <= 8 })
    );
    assert_eq!(wrapped[0].spans[0].style, style);
    assert_eq!(
        wrapped.iter().map(line_text).collect::<String>(),
        logical.iter().map(line_text).collect::<String>()
    );
}
