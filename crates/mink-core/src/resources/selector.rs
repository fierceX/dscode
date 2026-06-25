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
        && !looks_like_url_host_port(rest)
        && let Some((start, parsed_limit)) = parse_line_selector(suffix)?
    {
        path = base;
        offset = Some(start);
        limit = parsed_limit;
    }

    if path.is_empty() {
        bail!("Error: no path provided");
    }

    Ok(ReadPathSelection {
        path: path.to_string(),
        offset,
        limit,
        raw,
    })
}

pub fn select_text_lines(text: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_string();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !lines.is_empty() && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let total = lines.len();
    let start = offset.unwrap_or(1).saturating_sub(1).min(total);
    let end = limit.map_or(total, |count| (start + count).min(total));
    lines[start..end].join("\n")
}

fn looks_like_url_host_port(input: &str) -> bool {
    if !is_web_url(input) {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(input) else {
        return false;
    };
    url.port().is_some() && url.path() == "/" && url.query().is_none() && url.fragment().is_none()
}

fn is_web_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
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
