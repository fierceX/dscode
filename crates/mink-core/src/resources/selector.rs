use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPathSelection {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub raw: bool,
}

pub fn split_read_path_selection(input: &str) -> Result<ReadPathSelection> {
    let mut rest = input;
    let normalized;
    let mut raw = false;

    if let Some(stripped) = rest.strip_suffix(":raw") {
        rest = stripped;
        raw = true;
    }
    if let Some(stripped) = rest.strip_prefix("raw:") {
        rest = stripped;
        raw = true;
    }
    if let Some((base, tail)) = rest.rsplit_once(":raw:") {
        normalized = format!("{base}:{tail}");
        rest = &normalized;
        raw = true;
    }

    let mut offset = None;
    let mut limit = None;
    let mut path = rest;

    if let Some((base, suffix)) = rest.rsplit_once(':')
        && let Some((start, parsed_limit)) = parse_line_selector(suffix)?
    {
        path = base;
        offset = Some(start);
        limit = parsed_limit;
    }

    if path.is_empty() {
        bail!(
            "Error: no path provided; a line selector must be appended to a path (e.g. 'a.md:45-50')"
        );
    }

    Ok(ReadPathSelection {
        path: path.to_string(),
        offset,
        limit,
        raw,
    })
}

pub fn select_text_lines(
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    path: &str,
) -> Result<String> {
    if offset.is_none() && limit.is_none() {
        return Ok(text.to_string());
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !lines.is_empty() && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let total = lines.len();
    let start_line = offset.unwrap_or(1);
    if start_line > total {
        if total == 0 && start_line == 1 {
            return Ok(String::new());
        }
        bail!("Error: offset {start_line} exceeds total lines {total} in {path}");
    }
    let start = start_line - 1;
    let end = limit.map_or(total, |count| start.saturating_add(count).min(total));
    Ok(lines[start..end].join("\n"))
}

fn parse_line_selector(suffix: &str) -> Result<Option<(usize, Option<usize>)>> {
    if suffix.is_empty() {
        return Ok(None);
    }
    if !suffix
        .chars()
        .all(|ch| ch.is_ascii_digit() || ch == '-' || ch == '+')
    {
        return Ok(None);
    }
    let parse_line = |raw: &str| -> Result<usize> {
        let value = raw
            .parse::<usize>()
            .map_err(|_| anyhow!("Error: invalid line selector: {suffix}"))?;
        if value == 0 {
            bail!("Error: line selectors are 1-indexed; got 0");
        }
        Ok(value)
    };

    if let Some((start_raw, count_raw)) = suffix.split_once('+') {
        let start = parse_line(start_raw)?;
        let count = count_raw
            .parse::<usize>()
            .map_err(|_| anyhow!("Error: invalid line selector: {suffix}"))?;
        if count == 0 {
            bail!("Error: line selector count must be >= 1");
        }
        return Ok(Some((start, Some(count))));
    }

    if let Some((start_raw, end_raw)) = suffix.split_once('-') {
        let start = parse_line(start_raw)?;
        if end_raw.is_empty() {
            return Ok(Some((start, None)));
        }
        let end = parse_line(end_raw)?;
        if end < start {
            bail!("Error: line selector range ends before it starts: {suffix}");
        }
        return Ok(Some((start, Some(end - start + 1))));
    }

    Ok(Some((parse_line(suffix)?, None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_count_does_not_overflow() {
        // `path:2+usize::MAX` must read to EOF, not panic or wrap to an
        // inverted `lines[start..end]` slice.
        let text = "a\nb\nc\nd";
        let out = select_text_lines(text, Some(2), Some(usize::MAX), "f").unwrap();
        assert_eq!(out, "b\nc\nd");
    }

    #[test]
    fn offset_beyond_total_errors() {
        let err = select_text_lines("a\nb", Some(9), None, "f").unwrap_err();
        assert!(err.to_string().contains("offset 9 exceeds total lines 2"));
    }

    #[test]
    fn empty_text_line_one_is_empty_not_phantom() {
        assert_eq!(select_text_lines("", Some(1), None, "f").unwrap(), "");
        assert!(select_text_lines("", Some(2), None, "f").is_err());
    }

    #[test]
    fn range_and_open_ended_forms() {
        let text = "l1\nl2\nl3";
        assert_eq!(
            select_text_lines(text, Some(2), Some(2), "f").unwrap(),
            "l2\nl3"
        );
        assert_eq!(
            select_text_lines(text, Some(2), None, "f").unwrap(),
            "l2\nl3"
        );
        assert_eq!(select_text_lines(text, None, None, "f").unwrap(), text);
        assert_eq!(
            select_text_lines(text, Some(1), Some(1), "f").unwrap(),
            "l1"
        );
    }
}
