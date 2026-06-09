pub(crate) fn sanitize_tui_text(input: &str) -> String {
    normalize_control_text(&strip_ansi(input))
}

pub(crate) fn normalize_tui_input(input: &str) -> String {
    normalize_control_text(input)
}

fn normalize_control_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => {
                let Some(next) = chars.next() else {
                    break;
                };
                match next {
                    '[' => {
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    ']' => {
                        let mut saw_escape = false;
                        for c in chars.by_ref() {
                            if saw_escape {
                                if c == '\\' {
                                    break;
                                }
                                saw_escape = false;
                            }
                            if c == '\x07' {
                                break;
                            }
                            if c == '\x1b' {
                                saw_escape = true;
                            }
                        }
                    }
                    '(' | ')' | '*' | '+' | '-' | '.' | '/' => {
                        let _ = chars.next();
                    }
                    _ => {}
                }
            }
            '\u{009b}' => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            '\u{009d}' => {
                for c in chars.by_ref() {
                    if c == '\x07' {
                        break;
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    out
}
