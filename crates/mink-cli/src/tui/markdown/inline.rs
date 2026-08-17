use super::types::InlineNode;
use crate::tui::theme;
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

pub(crate) fn parse_inline(text: &str) -> Vec<InlineNode> {
    let mut nodes = Vec::new();
    let mut plain = String::new();
    let mut pos = 0usize;

    while pos < text.len() {
        let rest = &text[pos..];
        if let Some((node, consumed)) = parse_inline_token(rest) {
            if !plain.is_empty() {
                nodes.push(InlineNode::Text(std::mem::take(&mut plain)));
            }
            nodes.push(node);
            pos += consumed;
            continue;
        }

        let Some(ch) = rest.chars().next() else {
            break;
        };
        plain.push(ch);
        pos += ch.len_utf8();
    }

    if !plain.is_empty() {
        nodes.push(InlineNode::Text(plain));
    }
    nodes
}

fn parse_inline_token(rest: &str) -> Option<(InlineNode, usize)> {
    if let Some(after) = rest.strip_prefix('`')
        && let Some(end) = after.find('`')
    {
        return Some((InlineNode::Code(after[..end].to_string()), end + 2));
    }

    if let Some(after) = rest.strip_prefix("**")
        && let Some(end) = after.find("**")
    {
        return Some((InlineNode::Strong(parse_inline(&after[..end])), end + 4));
    }

    if let Some(after) = rest.strip_prefix('*')
        && !after.starts_with('*')
        && let Some(end) = after.find('*')
    {
        return Some((InlineNode::Emphasis(parse_inline(&after[..end])), end + 2));
    }

    parse_link_token(rest)
}

fn parse_link_token(rest: &str) -> Option<(InlineNode, usize)> {
    let after_open = rest.strip_prefix('[')?;
    let label_end = after_open.find("](")?;
    let href_start = label_end + 2;
    let href = &after_open[href_start..];
    let href_end = find_link_href_end(href)?;
    let label = &after_open[..label_end];
    Some((
        InlineNode::Link {
            text: parse_inline(label),
            href: unescape_link_href(&href[..href_end]),
        },
        href_start + href_end + 2,
    ))
}

fn find_link_href_end(href: &str) -> Option<usize> {
    let mut escaped = false;
    let mut paren_depth = 0usize;
    for (idx, ch) in href.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '(' => paren_depth += 1,
            ')' if paren_depth == 0 => return Some(idx),
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn unescape_link_href(href: &str) -> String {
    let mut out = String::with_capacity(href.len());
    let mut chars = href.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && matches!(next, ')' | '(' | '\\')
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

pub(crate) fn render_inline_spans(nodes: &[InlineNode], base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for node in nodes {
        match node {
            InlineNode::Text(text) => spans.push(Span::styled(text.clone(), base)),
            InlineNode::Code(text) => spans.push(Span::styled(text.clone(), theme::info())),
            InlineNode::Strong(children) => {
                spans.extend(render_inline_spans(
                    children,
                    base.add_modifier(Modifier::BOLD),
                ));
            }
            InlineNode::Emphasis(children) => {
                spans.extend(render_inline_spans(
                    children,
                    base.add_modifier(Modifier::ITALIC),
                ));
            }
            InlineNode::Link { text, href } => {
                let link_style = theme::link(base);
                spans.extend(render_inline_spans(text, link_style));
                if !href.is_empty() {
                    spans.push(Span::styled(format!(" ({href})"), theme::muted()));
                }
            }
        }
    }
    spans
}
