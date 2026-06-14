use ratatui::text::{Line, Span};

pub(crate) fn wrap_lines_word(lines: &[Line<'static>], max_w: u16) -> Vec<Line<'static>> {
    let mw = max_w.max(1) as usize;
    let mut out = Vec::new();
    for line in lines {
        if line.spans.is_empty() {
            out.push(Line::from(""));
            continue;
        }

        let mut cur: Vec<Span<'static>> = Vec::new();
        let mut cur_w = 0usize;
        for span in &line.spans {
            let style = span.style;
            let mut buf = String::new();
            for ch in span.content.chars() {
                if ch == '\n' {
                    if !buf.is_empty() {
                        cur.push(Span::styled(std::mem::take(&mut buf), style));
                    }
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                    continue;
                }

                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if cur_w + ch_w > mw && (!cur.is_empty() || !buf.is_empty()) {
                    if !buf.is_empty() {
                        cur.push(Span::styled(std::mem::take(&mut buf), style));
                    }
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                    if ch == ' ' {
                        continue;
                    }
                }
                buf.push(ch);
                cur_w += ch_w;
            }
            if !buf.is_empty() {
                cur.push(Span::styled(buf, style));
            }
        }
        if !cur.is_empty() {
            out.push(Line::from(cur));
        }
    }
    out
}

pub(crate) fn truncate_visual(text: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut width = 0usize;
    let mut cut = false;
    for ch in text.chars() {
        let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_w > max_w {
            cut = true;
            break;
        }
        out.push(ch);
        width += ch_w;
    }

    if cut {
        let ellipsis_w = unicode_width::UnicodeWidthChar::width('…').unwrap_or(1);
        while width + ellipsis_w > max_w && !out.is_empty() {
            if let Some(ch) = out.pop() {
                width =
                    width.saturating_sub(unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0));
            }
        }
        if ellipsis_w <= max_w {
            out.push('…');
        }
    }
    out
}
