//! Non-Block Hashline parser and pure applier.
//!
//! This tracks the current oh-my-pi `PUT`/`CUT` protocol. Syntactic-block
//! locators remain intentionally unsupported because Mink does not ship the
//! tree-sitter resolver used upstream.

use anyhow::{Result, anyhow, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

const MAX_RANGE_LINES: usize = 100_000;
const MAX_OPERATIONS: usize = 10_000;
const MAX_REGISTER_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    Head,
    Tail,
    Before(usize),
    After(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteTarget {
    Gap(Cursor),
    Range { start: usize, end: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Put {
        start: usize,
        end: usize,
        body: Vec<String>,
    },
    Cut {
        start: usize,
        end: usize,
        register: Option<String>,
    },
    Insert {
        cursor: Cursor,
        body: Vec<String>,
    },
    Paste {
        target: PasteTarget,
        register: Option<String>,
    },
    Remove,
    Move {
        destination: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: String,
    pub tag: String,
    pub operations: Vec<Operation>,
    pub warnings: Vec<String>,
}

/// True when applying `operations` to `text` would not change it because the
/// requested final content is already present at the exact target positions.
/// Used to turn an explainable no-op into an idempotent success; anything
/// ambiguous returns false so callers keep failing closed.
pub fn already_applied(text: &str, operations: &[Operation]) -> bool {
    let lines = crate::tools::snapshot::split_content_lines(text);
    let matches = |start0: usize, body: &[String]| -> bool {
        if body.len() > lines.len().saturating_sub(start0) {
            return false;
        }
        body.iter()
            .zip(lines[start0..start0 + body.len()].iter())
            .all(|(wanted, actual)| wanted == actual)
    };
    operations.iter().all(|operation| match operation {
        Operation::Put { start, end, body } => {
            if *start < 1 || *end < *start || *end > lines.len() {
                return false;
            }
            matches(start - 1, body)
        }
        Operation::Insert { cursor, body } => match cursor {
            Cursor::Head => matches(0, body),
            Cursor::Tail => {
                let start0 = lines.len().saturating_sub(body.len());
                matches(start0, body)
            }
            Cursor::Before(n) => matches(n.saturating_sub(1), body),
            Cursor::After(n) => matches(*n, body),
        },
        _ => false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clipboard {
    anonymous: Option<Vec<String>>,
    pending_anonymous_cuts: Vec<String>,
    named: BTreeMap<String, Vec<String>>,
}

impl Clipboard {
    pub fn with_named(named: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            named,
            ..Self::default()
        }
    }

    pub fn named(&self) -> &BTreeMap<String, Vec<String>> {
        &self.named
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    pub text: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
enum PendingKind {
    Put { start: usize, end: usize },
    Insert { cursor: Cursor },
}

#[derive(Debug, Clone)]
enum BodyRow {
    Explicit(String),
    Bare(String),
    Minus(String),
    Blank,
}

#[derive(Debug, Clone)]
struct Pending {
    kind: PendingKind,
    rows: Vec<BodyRow>,
    deferred_blanks: usize,
    line_num: usize,
}

pub fn parse(input: &str) -> Result<Patch> {
    ensure!(
        !input.trim().is_empty(),
        "Error: hashline input must not be empty"
    );
    let normalized = input
        .strip_prefix('\u{feff}')
        .unwrap_or(input)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines = normalized.split('\n').collect::<Vec<_>>();
    let mut sections = Vec::new();
    let mut current: Option<Section> = None;
    let mut pending: Option<Pending> = None;

    for (index, line) in lines.iter().enumerate() {
        let line_num = index + 1;
        let trimmed = line.trim();
        if trimmed == "*** Begin Patch" {
            continue;
        }
        if trimmed == "*** End Patch" || trimmed == "*** Abort" {
            break;
        }

        if let Some(parsed) = try_parse_header(trimmed, line_num)? {
            if let Some(pending) = pending.take() {
                flush_pending(current.as_mut(), pending)?;
            }
            if let Some(section) = current.take()
                && !section.operations.is_empty()
            {
                sections.push(section);
            }
            current = Some(parsed);
            continue;
        }

        if !is_operation_line(trimmed)
            && let Some(pending) = pending.as_mut()
        {
            if trimmed.is_empty() {
                pending.deferred_blanks += 1;
            } else {
                for _ in 0..pending.deferred_blanks {
                    pending.rows.push(BodyRow::Blank);
                }
                pending.deferred_blanks = 0;
                if let Some(payload) = line.strip_prefix('+') {
                    pending.rows.push(BodyRow::Explicit(payload.to_string()));
                } else if line.starts_with('-') {
                    pending.rows.push(BodyRow::Minus((*line).to_string()));
                } else {
                    pending.rows.push(BodyRow::Bare((*line).to_string()));
                }
            }
            continue;
        }

        if let Some(pending) = pending.take() {
            flush_pending(current.as_mut(), pending)?;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(section) = current.as_mut() else {
            if trimmed.starts_with('@') {
                bail!("Error: legacy @PATH#TAG headers are unsupported; use [PATH#TAG]");
            }
            bail!("Error: line {line_num}: hashline input must begin with [PATH#TAG]");
        };
        parse_operation(trimmed, line_num, section, &mut pending)?;
        ensure!(
            section.operations.len() <= MAX_OPERATIONS,
            "Error: too many hashline operations"
        );
    }

    if let Some(pending) = pending.take() {
        flush_pending(current.as_mut(), pending)?;
    }
    if let Some(section) = current.take()
        && !section.operations.is_empty()
    {
        sections.push(section);
    }
    ensure!(
        !sections.is_empty(),
        "Error: hashline input did not contain a [PATH#TAG] section"
    );
    for section in &sections {
        let file_ops = section
            .operations
            .iter()
            .filter(|operation| matches!(operation, Operation::Remove | Operation::Move { .. }))
            .count();
        ensure!(
            file_ops <= 1,
            "Error: section {} contains multiple file operations",
            section.path
        );
        if let Some(position) = section
            .operations
            .iter()
            .position(|operation| matches!(operation, Operation::Remove | Operation::Move { .. }))
        {
            ensure!(
                position + 1 == section.operations.len(),
                "Error: REM or MV must be the final operation in section {}",
                section.path
            );
        }
    }
    Ok(Patch { sections })
}

fn try_parse_header(line: &str, line_num: usize) -> Result<Option<Section>> {
    if !line.starts_with('[') {
        return Ok(None);
    }
    let body = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| anyhow!("Error: line {line_num}: malformed [PATH#TAG] header"))?;
    let body = strip_apply_patch_path_noise(body.trim());
    let (path, tag) = body.rsplit_once('#').ok_or_else(|| {
        anyhow!("Error: line {line_num}: header must include a four-hex snapshot tag")
    })?;
    let path = unquote_path(path.trim());
    ensure!(
        !path.is_empty(),
        "Error: line {line_num}: header path is empty"
    );
    ensure!(
        !path.contains('#'),
        "Error: line {line_num}: header path may not contain '#'"
    );
    ensure!(
        tag.len() == 4 && tag.chars().all(|ch| ch.is_ascii_hexdigit()),
        "Error: line {line_num}: snapshot tag must contain exactly four hexadecimal characters"
    );
    Ok(Some(Section {
        path,
        tag: tag.to_ascii_uppercase(),
        operations: Vec::new(),
        warnings: Vec::new(),
    }))
}

fn strip_apply_patch_path_noise(path: &str) -> &str {
    let mut value = path.trim_start_matches('*').trim_start();
    for prefix in [
        "Update File:",
        "Update:",
        "Add File:",
        "Delete File:",
        "Move to:",
    ] {
        if value
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            value = value[prefix.len()..].trim_start();
            break;
        }
    }
    value
}

fn unquote_path(path: &str) -> String {
    if path.len() >= 2 {
        let first = path.as_bytes()[0];
        let last = path.as_bytes()[path.len() - 1];
        if matches!(first, b'\'' | b'"') && first == last {
            return path[1..path.len() - 1].to_string();
        }
    }
    path.to_string()
}

fn is_operation_line(line: &str) -> bool {
    ["PUT", "CUT"]
        .iter()
        .any(|keyword| keyword_rest_loose(line, keyword).is_some())
        || ["REM", "MV"].iter().any(|keyword| {
            keyword_rest(line, keyword).is_some() || line.eq_ignore_ascii_case(keyword)
        })
        || line.starts_with('[')
        || line.starts_with("@@")
        || line.starts_with("***")
        || line.starts_with("diff --git ")
}

fn keyword_rest<'a>(line: &'a str, keyword: &'a str) -> Option<&'a str> {
    let head = line.get(..keyword.len())?;
    (head.eq_ignore_ascii_case(keyword)
        && line
            .as_bytes()
            .get(keyword.len())
            .is_some_and(u8::is_ascii_whitespace))
    .then(|| line[keyword.len()..].trim_start())
}

/// Like `keyword_rest`, but also accepts the missing-space shape models
/// habitually emit (`PUT18.=18:`, `PUT>40:`, `CUT5.=8`). Only locators that
/// start with a digit or `<`/`>`/`:` are normalized, so ordinary words that
/// merely begin with the keyword letters are never misread.
fn keyword_rest_loose<'a>(line: &'a str, keyword: &'a str) -> Option<&'a str> {
    if let Some(rest) = keyword_rest(line, keyword) {
        return Some(rest);
    }
    let head = line.get(..keyword.len())?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let b = line.as_bytes().get(keyword.len()).copied()?;
    (b.is_ascii_digit() || b == b'>' || b == b'<' || b == b':').then(|| &line[keyword.len()..])
}

fn parse_operation(
    line: &str,
    line_num: usize,
    section: &mut Section,
    pending: &mut Option<Pending>,
) -> Result<()> {
    if line.starts_with("@@") {
        bail!("Error: line {line_num}: unified-diff hunk header is not valid in hashline");
    }
    if line.starts_with("***") || line.starts_with("diff --git ") {
        bail!("Error: line {line_num}: apply_patch/unified-diff syntax is not valid in hashline");
    }
    if line.contains('*')
        && (keyword_rest(line, "PUT").is_some() || keyword_rest(line, "CUT").is_some())
    {
        bail!(
            "Error: line {line_num}: Block hashline operations using `*` are unsupported in Mink"
        );
    }
    if line.eq_ignore_ascii_case("REM") {
        section.operations.push(Operation::Remove);
        return Ok(());
    }
    if let Some(destination) = keyword_rest(line, "MV") {
        ensure!(
            !destination.is_empty(),
            "Error: line {line_num}: MV requires a destination path"
        );
        section.operations.push(Operation::Move {
            destination: unquote_path(destination),
        });
        return Ok(());
    }
    if let Some(rest) = keyword_rest_loose(line, "CUT") {
        if keyword_rest(line, "CUT").is_none() {
            section
                .warnings
                .push("Normalized missing space: `CUT5.` parsed as `CUT 5.`".to_string());
        }
        let (rest, had_colon) = strip_optional_colon(rest);
        if had_colon {
            section
                .warnings
                .push("Ignored trailing `:` on CUT; CUT takes no body rows.".to_string());
        }
        let (locator, register) = split_register(rest, line_num)?;
        let (start, end) = parse_range(locator, line_num)?;
        section.operations.push(Operation::Cut {
            start,
            end,
            register,
        });
        return Ok(());
    }
    if let Some(rest) = keyword_rest_loose(line, "PUT") {
        if keyword_rest(line, "PUT").is_none() {
            section
                .warnings
                .push("Normalized missing space: `PUT2.` parsed as `PUT 2.`".to_string());
        }
        let (rest, had_colon) = strip_optional_colon(rest);
        let (locator, register) = split_register(rest, line_num)?;
        if locator == ">$" {
            return put_gap(
                Cursor::Tail,
                register,
                had_colon,
                line_num,
                section,
                pending,
            );
        }
        if let Some(raw) = locator.strip_prefix('<') {
            let line = parse_line_number(raw.trim(), line_num)?;
            let cursor = if line == 1 {
                Cursor::Head
            } else {
                Cursor::Before(line)
            };
            return put_gap(cursor, register, had_colon, line_num, section, pending);
        }
        if let Some(raw) = locator.strip_prefix('>') {
            let line = parse_line_number(raw.trim(), line_num)?;
            return put_gap(
                Cursor::After(line),
                register,
                had_colon,
                line_num,
                section,
                pending,
            );
        }
        let (start, end) = parse_range(locator, line_num)?;
        if let Some(register) = register {
            ensure!(
                !had_colon,
                "Error: line {line_num}: register PUT takes no body rows"
            );
            section.operations.push(Operation::Paste {
                target: PasteTarget::Range { start, end },
                register: Some(register),
            });
        } else {
            ensure!(
                had_colon,
                "Error: line {line_num}: span PUT without a body must name a register"
            );
            *pending = Some(Pending {
                kind: PendingKind::Put { start, end },
                rows: Vec::new(),
                deferred_blanks: 0,
                line_num,
            });
        }
        return Ok(());
    }
    if line.starts_with('+') {
        bail!(
            "Error: line {line_num}: payload line has no preceding hunk header; CUT/register/file operations take no body rows"
        );
    }
    bail!(
        "Error: line {line_num}: unknown hashline operation {line:?}; expected PUT, CUT, REM, or MV"
    )
}

fn strip_optional_colon(input: &str) -> (&str, bool) {
    let trimmed = input.trim_end();
    trimmed
        .strip_suffix(':')
        .map_or((trimmed, false), |value| (value.trim_end(), true))
}

fn split_register(input: &str, line_num: usize) -> Result<(&str, Option<String>)> {
    let Some(position) = input.rfind(" @") else {
        return Ok((input.trim(), None));
    };
    let name = input[position + 2..].trim();
    ensure!(
        !name.is_empty()
            && name.len() <= MAX_REGISTER_NAME
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')),
        "Error: line {line_num}: register names use 1-{MAX_REGISTER_NAME} ASCII letters, digits, `_`, or `-`"
    );
    Ok((input[..position].trim(), Some(name.to_string())))
}

fn put_gap(
    cursor: Cursor,
    register: Option<String>,
    had_colon: bool,
    line_num: usize,
    section: &mut Section,
    pending: &mut Option<Pending>,
) -> Result<()> {
    if had_colon {
        ensure!(
            register.is_none(),
            "Error: line {line_num}: register PUT takes no body rows"
        );
        *pending = Some(Pending {
            kind: PendingKind::Insert { cursor },
            rows: Vec::new(),
            deferred_blanks: 0,
            line_num,
        });
    } else {
        section.operations.push(Operation::Paste {
            target: PasteTarget::Gap(cursor),
            register,
        });
    }
    Ok(())
}

fn flush_pending(section: Option<&mut Section>, pending: Pending) -> Result<()> {
    let section =
        section.ok_or_else(|| anyhow!("Error: internal hashline parser lost its section"))?;
    let mut rows = pending.rows;
    let explicit = rows.iter().any(|row| matches!(row, BodyRow::Explicit(_)));
    let had_bare_rows = rows.iter().any(|row| matches!(row, BodyRow::Bare(_)));
    let mut bullet_auto_piped = false;
    let minus = rows
        .iter()
        .filter_map(|row| match row {
            BodyRow::Minus(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !minus.is_empty() {
        let all_bullets = minus.iter().all(|row| markdown_bullet(row));
        let explicit_bullet = rows
            .iter()
            .filter_map(|row| match row {
                BodyRow::Explicit(value) => Some(value),
                _ => None,
            })
            .any(|line| markdown_bullet(line));
        if all_bullets && (!explicit || explicit_bullet) {
            bullet_auto_piped = true;
            section
                .warnings
                .push(
                    "Auto-prefixed bare `- ` bullet row(s) as literal content. `-` rows never remove lines — the range does that; always prefix literal body rows with `+`: `+- item`."
                        .to_string(),
                );
            for row in &mut rows {
                if let BodyRow::Minus(value) = row {
                    *row = BodyRow::Bare(value.clone());
                }
            }
        } else if explicit && !all_bullets {
            rows.retain(|row| !matches!(row, BodyRow::Minus(_)));
            section.warnings.push(
                "Ignored unified-diff `-old` row(s); the range already removes old content, so only `+new` rows were kept."
                    .to_string(),
            );
        } else {
            bail!(
                "Error: line {}: `-` rows are not valid hashline body rows; use `+- item` for Markdown bullets",
                pending.line_num
            );
        }
    }

    let bare_indices = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            matches!(row, BodyRow::Bare(value) if !value.trim().is_empty()).then_some(index)
        })
        .collect::<Vec<_>>();
    if !bare_indices.is_empty() {
        let stripped = bare_indices
            .iter()
            .filter_map(|index| match &rows[*index] {
                BodyRow::Bare(value) => strip_read_prefix(value).map(ToString::to_string),
                _ => None,
            })
            .collect::<Vec<_>>();
        let all_prefixed = stripped.len() == bare_indices.len();
        let all_literal_values =
            all_prefixed && stripped.iter().all(|value| literal_mapping_value(value));
        if all_prefixed && !all_literal_values {
            for (index, value) in bare_indices.into_iter().zip(stripped) {
                rows[index] = BodyRow::Bare(value);
            }
        }
        if had_bare_rows && !bullet_auto_piped {
            section.warnings.push(
                "Auto-prefixed bare body row(s) with `+`. Body rows must be `+TEXT` literal lines."
                    .to_string(),
            );
        }
    }
    let body = rows
        .into_iter()
        .map(|row| match row {
            BodyRow::Explicit(value) | BodyRow::Bare(value) | BodyRow::Minus(value) => value,
            BodyRow::Blank => String::new(),
        })
        .collect::<Vec<_>>();
    match pending.kind {
        PendingKind::Put { start, end } => {
            if body.is_empty() {
                section.warnings.push(
                    "Interpreted an empty `PUT` body as deletion. Use `CUT N.=M` or `CUT N*` for bodyless deletes."
                        .to_string(),
                );
            }
            section.operations.push(Operation::Put { start, end, body })
        }
        PendingKind::Insert { cursor } => {
            ensure!(
                !body.is_empty(),
                "Error: line {}: PUT insert promises body rows",
                pending.line_num
            );
            section.operations.push(Operation::Insert { cursor, body });
        }
    }
    Ok(())
}

fn markdown_bullet(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("- ")
        .is_some_and(|rest| !rest.is_empty())
}

fn strip_read_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let colon = trimmed.find(':')?;
    (colon > 0 && trimmed[..colon].chars().all(|ch| ch.is_ascii_digit()))
        .then_some(&trimmed[colon + 1..])
}

fn literal_mapping_value(value: &str) -> bool {
    let value = value.trim().trim_end_matches(',').trim();
    (value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))))
        || value.parse::<f64>().is_ok()
}

fn parse_range(raw: &str, line_num: usize) -> Result<(usize, usize)> {
    let raw = raw.trim();
    let first_end = raw
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(raw.len());
    let start = parse_line_number(&raw[..first_end], line_num)?;
    let remainder = &raw[first_end..];
    let mut separator_end = 0;
    for (index, ch) in remainder.char_indices() {
        if ch.is_whitespace() || matches!(ch, '-' | '.' | '=' | '…') {
            separator_end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if remainder.is_empty() {
        return Ok((start, start));
    }
    ensure!(
        separator_end > 0,
        "Error: line {line_num}: invalid range {raw:?}"
    );
    let end = parse_line_number(remainder[separator_end..].trim(), line_num)?;
    ensure!(
        start <= end,
        "Error: line {line_num}: range start {start} is after end {end}"
    );
    ensure!(
        end - start < MAX_RANGE_LINES,
        "Error: line {line_num}: range expands beyond {MAX_RANGE_LINES} lines"
    );
    Ok((start, end))
}

fn parse_line_number(raw: &str, line_num: usize) -> Result<usize> {
    let value = raw.parse::<usize>().map_err(|_| {
        anyhow!("Error: line {line_num}: expected a positive line number, got {raw:?}")
    })?;
    ensure!(
        value > 0,
        "Error: line {line_num}: line numbers are one-based"
    );
    Ok(value)
}

pub fn anchor_lines(section: &Section) -> BTreeSet<usize> {
    let mut anchors = BTreeSet::new();
    for operation in &section.operations {
        match operation {
            Operation::Put { start, end, .. }
            | Operation::Cut { start, end, .. }
            | Operation::Paste {
                target: PasteTarget::Range { start, end },
                ..
            } => anchors.extend(*start..=*end),
            Operation::Insert { cursor, .. }
            | Operation::Paste {
                target: PasteTarget::Gap(cursor),
                ..
            } => match cursor {
                Cursor::Before(line) | Cursor::After(line) => {
                    anchors.insert(*line);
                }
                Cursor::Head | Cursor::Tail => {}
            },
            Operation::Remove | Operation::Move { .. } => {}
        }
    }
    anchors
}

pub fn is_head_tail_only(section: &Section) -> bool {
    section.operations.iter().all(|operation| {
        matches!(
            operation,
            Operation::Insert {
                cursor: Cursor::Head | Cursor::Tail,
                ..
            } | Operation::Paste {
                target: PasteTarget::Gap(Cursor::Head | Cursor::Tail),
                ..
            }
        )
    })
}

pub fn remap_anchors(section: &Section, offset: isize) -> Result<Section> {
    let shift = |line: usize| -> Result<usize> {
        line.checked_add_signed(offset)
            .filter(|line| *line > 0)
            .ok_or_else(|| anyhow!("Error: stale recovery mapped an anchor outside the file"))
    };
    let mut section = section.clone();
    for operation in &mut section.operations {
        match operation {
            Operation::Put { start, end, .. }
            | Operation::Cut { start, end, .. }
            | Operation::Paste {
                target: PasteTarget::Range { start, end },
                ..
            } => {
                *start = shift(*start)?;
                *end = shift(*end)?;
            }
            Operation::Insert { cursor, .. }
            | Operation::Paste {
                target: PasteTarget::Gap(cursor),
                ..
            } => match cursor {
                Cursor::Before(line) | Cursor::After(line) => *line = shift(*line)?,
                Cursor::Head | Cursor::Tail => {}
            },
            Operation::Remove | Operation::Move { .. } => {}
        }
    }
    Ok(section)
}

pub fn apply(text: &str, section: &Section, clipboard: &mut Clipboard) -> Result<ApplyResult> {
    let had_trailing_newline = text.ends_with('\n');
    let lines = crate::tools::snapshot::split_content_lines(text);
    let mut deleted = BTreeSet::new();
    let mut insertions: BTreeMap<usize, Vec<Vec<String>>> = BTreeMap::new();
    let mut warnings = section.warnings.clone();

    for operation in &section.operations {
        match operation {
            Operation::Put { start, end, .. }
            | Operation::Cut { start, end, .. }
            | Operation::Paste {
                target: PasteTarget::Range { start, end },
                ..
            } => {
                validate_range(*start, *end, lines.len(), &section.path)?;
                ensure_target_free(&mut deleted, *start, *end)?;
            }
            _ => {}
        }
    }
    let section_delta = section_delimiter_delta(&lines, section);

    for operation in &section.operations {
        match operation {
            Operation::Put { start, end, body } => {
                let repair = repair_replacement(&lines, *start, *end, body, section_delta)?;
                for line in repair.keep_lines {
                    deleted.remove(&line);
                }
                warnings.extend(repair.warnings);
                if !repair.body.is_empty() {
                    insertions.entry(start - 1).or_default().push(repair.body);
                }
            }
            Operation::Cut {
                start,
                end,
                register,
            } => {
                let captured = lines[start - 1..*end].to_vec();
                if let Some(register) = register {
                    clipboard.named.insert(register.clone(), captured);
                } else {
                    clipboard.anonymous = Some(captured);
                    clipboard
                        .pending_anonymous_cuts
                        .push(format!("CUT {start}.={end}"));
                }
            }
            Operation::Insert { cursor, body } => {
                let (boundary, shifted) =
                    insertion_boundary(cursor, body, &lines, &deleted, &section.path)?;
                if let Some((from, to)) = shifted {
                    warnings.push(format!(
                        "shifted PUT > landing from line {from} to structural closer line {to}"
                    ));
                }
                insertions.entry(boundary).or_default().push(body.clone());
            }
            Operation::Paste { target, register } => {
                let body = if let Some(register) = register {
                    clipboard
                        .named
                        .get(register)
                        .cloned()
                        .ok_or_else(|| anyhow!("Error: unknown Hashline register @{register}"))?
                } else {
                    ensure!(
                        clipboard.pending_anonymous_cuts.len() <= 1,
                        "Error: anonymous PUT is ambiguous after multiple CUT operations; name the register"
                    );
                    let body = clipboard.anonymous.clone().ok_or_else(|| {
                        anyhow!(
                            "Error: anonymous PUT requires a prior unlabeled CUT in this Edit call"
                        )
                    })?;
                    clipboard.pending_anonymous_cuts.clear();
                    body
                };
                match target {
                    PasteTarget::Gap(cursor) => {
                        let (boundary, shifted) =
                            insertion_boundary(cursor, &body, &lines, &deleted, &section.path)?;
                        if let Some((from, to)) = shifted {
                            warnings.push(format!(
                                "shifted register PUT landing from line {from} to {to}"
                            ));
                        }
                        insertions.entry(boundary).or_default().push(body);
                    }
                    PasteTarget::Range { start, .. } => {
                        insertions.entry(start - 1).or_default().push(body);
                    }
                }
            }
            Operation::Remove | Operation::Move { .. } => {}
        }
    }

    let mut output = Vec::new();
    for boundary in 0..=lines.len() {
        if let Some(groups) = insertions.get(&boundary) {
            for group in groups {
                output.extend(group.iter().cloned());
            }
        }
        if boundary < lines.len() && !deleted.contains(&(boundary + 1)) {
            output.push(lines[boundary].clone());
        }
    }
    let mut result = output.join("\n");
    if had_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(ApplyResult {
        text: result,
        warnings,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DelimiterBalance {
    paren: isize,
    bracket: isize,
    brace: isize,
}

impl DelimiterBalance {
    fn add(self, other: Self) -> Self {
        Self {
            paren: self.paren + other.paren,
            bracket: self.bracket + other.bracket,
            brace: self.brace + other.brace,
        }
    }

    fn subtract(self, other: Self) -> Self {
        Self {
            paren: self.paren - other.paren,
            bracket: self.bracket - other.bracket,
            brace: self.brace - other.brace,
        }
    }

    fn is_zero(self) -> bool {
        self == Self::default()
    }
}

fn section_delimiter_delta(lines: &[String], section: &Section) -> Option<DelimiterBalance> {
    let mut delta = DelimiterBalance::default();
    for operation in &section.operations {
        let contribution = match operation {
            Operation::Put { start, end, body } => {
                balance(body).subtract(balance(&lines[start - 1..*end]))
            }
            Operation::Cut { start, end, .. } => {
                DelimiterBalance::default().subtract(balance(&lines[start - 1..*end]))
            }
            Operation::Insert { body, .. } => balance(body),
            Operation::Paste { .. } => return None,
            Operation::Remove | Operation::Move { .. } => DelimiterBalance::default(),
        };
        delta = delta.add(contribution);
    }
    Some(delta)
}

#[derive(Debug)]
struct ReplacementRepair {
    body: Vec<String>,
    keep_lines: Vec<usize>,
    warnings: Vec<String>,
}

fn repair_replacement(
    lines: &[String],
    start: usize,
    end: usize,
    body: &[String],
    section_delta: Option<DelimiterBalance>,
) -> Result<ReplacementRepair> {
    let mut body = body.to_vec();
    let mut warnings = Vec::new();
    repair_replacement_indentation(lines, start, end, &mut body, &mut warnings);

    let leading_limit = body.len().min(start.saturating_sub(1));
    let mut leading = 0;
    for count in 1..=leading_limit {
        if body[..count] == lines[start - 1 - count..start - 1] {
            leading = count;
        }
    }
    let trailing_limit = body.len().min(lines.len().saturating_sub(end));
    let mut trailing = 0;
    for count in 1..=trailing_limit {
        if body[body.len() - count..] == lines[end..end + count] {
            trailing = count;
        }
    }
    if leading > 0 && trailing > 0 && leading + trailing < body.len() {
        let dropped = balance(&body[..leading]).add(balance(&body[body.len() - trailing..]));
        let delta = balance(&body).subtract(balance(&lines[start - 1..end]));
        if dropped.is_zero() || dropped == delta {
            body = body[leading..body.len() - trailing].to_vec();
            warnings.push(format!(
                "Auto-repaired a replacement boundary echo at line {start}: dropped {leading} leading and {trailing} trailing payload line(s) already present outside the range. Issue the payload as the final desired content for the selected range only — never restate unchanged lines bordering the range."
            ));
            return Ok(ReplacementRepair {
                body,
                keep_lines: Vec::new(),
                warnings,
            });
        }
    }

    let source = &lines[start - 1..end];
    let delta = balance(&body).subtract(balance(source));
    if !delta.is_zero() {
        if trailing > 0 && balance(&body[body.len() - trailing..]) == delta {
            body.truncate(body.len() - trailing);
            warnings.push(format!(
                "Auto-repaired a delimiter-balance mismatch in the replacement at line {start}: dropped {trailing} duplicated trailing payload line(s) already present below the range. Issue the payload as the final desired content only — never restate or omit a closing bracket bordering the range."
            ));
            return Ok(ReplacementRepair {
                body,
                keep_lines: Vec::new(),
                warnings,
            });
        }
        if leading > 0 && balance(&body[..leading]) == delta {
            body.drain(..leading);
            warnings.push(format!(
                "Auto-repaired a delimiter-balance mismatch in the replacement at line {start}: dropped {leading} duplicated leading payload line(s) already present above the range. Issue the payload as the final desired content only — never restate or omit a closing bracket bordering the range."
            ));
            return Ok(ReplacementRepair {
                body,
                keep_lines: Vec::new(),
                warnings,
            });
        }
    }

    // A neutral one-sided echo on a multi-line rewrite is the common
    // off-by-one keeper mistake. Reject the short/ambiguous form instead of
    // silently deleting source lines not represented by the payload.
    if (leading > 0) ^ (trailing > 0) {
        let (side, count, echo) = if leading > 0 {
            ("leading", leading, &body[..leading])
        } else {
            ("trailing", trailing, &body[body.len() - trailing..])
        };
        let range_len = end - start + 1;
        if range_len > 1 && balance(echo).is_zero() && count < body.len() {
            if body.len() < range_len + count {
                let where_text = if side == "leading" {
                    "opens by restating"
                } else {
                    "ends by restating"
                };
                bail!(
                    "`PUT {start}.={end}:` rejected: the body {where_text} the {count} line(s) just {} the range, but is too short to be the full final content of the widened range — applying it as-is or auto-repairing would delete range line(s) the body never restates. Re-issue with the range covering exactly the lines that change and the body as their complete final content: drop the restated keeper from the body, or widen the range to consume it.",
                    if side == "leading" { "above" } else { "below" }
                );
            }
            if side == "leading" {
                body.drain(..count);
            } else {
                body.truncate(body.len() - count);
            }
            warnings.push(format!(
                "Auto-repaired a replacement boundary echo at line {start}: dropped {count} {side} payload line(s) identical to the surviving line(s) just {} the range. The range was one line short of the content you retyped — issue the payload as the final content for the selected range only, and widen the range to consume any keeper you restate.",
                if side == "leading" { "above" } else { "below" }
            ));
            return Ok(ReplacementRepair {
                body,
                keep_lines: Vec::new(),
                warnings,
            });
        }
    }

    // When the payload leaves unmatched openers and the selected range ends in
    // bare structural closers, retain only the suffix needed to rebalance it.
    let delta = balance(&body).subtract(balance(source));
    let mut keep_lines = Vec::new();
    let mut remaining = delta;
    for line in (start..=end).rev() {
        let text = &lines[line - 1];
        if !is_structural_closer(text) {
            break;
        }
        let closer = balance(std::slice::from_ref(text));
        let next = remaining.subtract(DelimiterBalance {
            paren: -closer.paren,
            bracket: -closer.bracket,
            brace: -closer.brace,
        });
        if next.paren.abs() <= remaining.paren.abs()
            && next.bracket.abs() <= remaining.bracket.abs()
            && next.brace.abs() <= remaining.brace.abs()
        {
            keep_lines.push(line);
            remaining = next;
        }
        if remaining.is_zero() {
            break;
        }
    }
    let section_can_cover = section_delta.is_some_and(|total| balance_covers(total, delta));
    if !keep_lines.is_empty() && remaining.is_zero() && section_can_cover {
        let closer_indent = leading_indent(&lines[keep_lines[0] - 1]);
        let claims_inside = body
            .iter()
            .filter(|line| !line.trim().is_empty())
            .any(|line| leading_indent(line).len() > closer_indent.len());
        if !claims_inside {
            bail!(
                "`PUT {start}.={end}:` rejected: replacing a structural closer with content at the closer's own indentation is unsafe because whether it belongs before or after the closer is ambiguous"
            );
        }
        keep_lines.sort_unstable();
        warnings.push(format!(
            "Auto-repaired a delimiter-balance mismatch in the replacement at line {start}: kept {} structural closing line(s) the range deleted without restating. Issue the payload as the final desired content only — never restate or omit a closing bracket bordering the range.",
            keep_lines.len()
        ));
    } else {
        keep_lines.clear();
    }
    Ok(ReplacementRepair {
        body,
        keep_lines,
        warnings,
    })
}

fn balance_covers(available: DelimiterBalance, needed: DelimiterBalance) -> bool {
    fn covers(available: isize, needed: isize) -> bool {
        needed == 0 || available.signum() == needed.signum() && available.abs() >= needed.abs()
    }
    covers(available.paren, needed.paren)
        && covers(available.bracket, needed.bracket)
        && covers(available.brace, needed.brace)
}

fn repair_replacement_indentation(
    lines: &[String],
    start: usize,
    end: usize,
    body: &mut [String],
    warnings: &mut Vec<String>,
) {
    if body.len() != end - start + 1 || start <= 1 {
        return;
    }
    let preceding = &lines[start - 2];
    let source_first = &lines[start - 1];
    let payload_first = &body[0];
    let preceding_indent = leading_indent(preceding);
    if !preceding.trim_end().ends_with('{')
        || source_first.len() - source_first.trim_start().len() <= preceding_indent.len()
        || payload_first.len() - payload_first.trim_start().len() > preceding_indent.len()
    {
        return;
    }
    let mut shift: Option<&str> = None;
    let mut matches = 0usize;
    for (source, payload) in lines[start - 1..end].iter().zip(body.iter()) {
        if source.trim().is_empty() || source.trim_start() != payload.trim_start() {
            continue;
        }
        let source_indent = leading_indent(source);
        let payload_indent = leading_indent(payload);
        let Some(candidate) = source_indent.strip_suffix(payload_indent) else {
            return;
        };
        if shift.is_some_and(|current| current != candidate) {
            return;
        }
        shift = Some(candidate);
        matches += 1;
    }
    let Some(shift) = shift else {
        return;
    };
    if shift.is_empty() || matches < 2 || matches * 2 <= body.len() {
        return;
    }
    for line in body.iter_mut().filter(|line| !line.trim().is_empty()) {
        line.insert_str(0, shift);
    }
    warnings.push(
        "Auto-indented a replacement body to match unchanged structural rows in its source range."
            .to_string(),
    );
}

fn balance(lines: &[String]) -> DelimiterBalance {
    let mut result = DelimiterBalance::default();
    let mut block_comment = false;
    let mut quote = None;
    for line in lines {
        let chars = line.chars().collect::<Vec<_>>();
        let mut index = 0usize;
        while index < chars.len() {
            let ch = chars[index];
            let next = chars.get(index + 1).copied();
            if block_comment {
                if ch == '*' && next == Some('/') {
                    block_comment = false;
                    index += 2;
                    continue;
                }
                index += 1;
                continue;
            }
            if let Some(active) = quote {
                if ch == '\\' {
                    index += 2;
                    continue;
                }
                if ch == active {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if ch == '/' && next == Some('/') {
                break;
            }
            if ch == '/' && next == Some('*') {
                block_comment = true;
                index += 2;
                continue;
            }
            if matches!(ch, '\'' | '"' | '`') {
                quote = Some(ch);
                index += 1;
                continue;
            }
            match ch {
                '(' => result.paren += 1,
                ')' => result.paren -= 1,
                '[' => result.bracket += 1,
                ']' => result.bracket -= 1,
                '{' => result.brace += 1,
                '}' => result.brace -= 1,
                _ => {}
            }
            index += 1;
        }
        if quote != Some('`') {
            quote = None;
        }
    }
    result
}

fn insertion_boundary(
    cursor: &Cursor,
    body: &[String],
    lines: &[String],
    targeted: &BTreeSet<usize>,
    path: &str,
) -> Result<(usize, Option<(usize, usize)>)> {
    let literal = cursor_boundary(cursor, lines.len(), path)?;
    let Cursor::After(anchor) = cursor else {
        return Ok((literal, None));
    };
    let Some(target_indent) = body_target_indent(body) else {
        return Ok((literal, None));
    };
    let anchor_indent = leading_indent(&lines[anchor - 1]);
    if target_indent.len() >= anchor_indent.len() || !anchor_indent.starts_with(target_indent) {
        return Ok((literal, None));
    }
    let mut landing = *anchor;
    for line in anchor + 1..=lines.len() {
        let text = &lines[line - 1];
        if text.trim().is_empty() {
            continue;
        }
        if !is_structural_closer(text) {
            break;
        }
        let indent = leading_indent(text);
        if !indent.starts_with(target_indent) || targeted.contains(&line) {
            return Ok((literal, None));
        }
        landing = line;
        if indent.len() == target_indent.len() {
            break;
        }
    }
    if landing == *anchor {
        Ok((literal, None))
    } else {
        Ok((landing, Some((*anchor, landing))))
    }
}

fn leading_indent(line: &str) -> &str {
    let length = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[..length]
}

fn body_target_indent(body: &[String]) -> Option<&str> {
    let mut rows = body
        .iter()
        .filter(|line| !line.trim().is_empty() && !is_structural_closer(line));
    let mut target = leading_indent(rows.next()?);
    for row in rows {
        let indent = leading_indent(row);
        if indent.starts_with(target) {
            continue;
        }
        if target.starts_with(indent) {
            target = indent;
        } else {
            return None;
        }
    }
    Some(target)
}

fn is_structural_closer(line: &str) -> bool {
    let trimmed = line.trim();
    let core = trimmed
        .strip_suffix(';')
        .or_else(|| trimmed.strip_suffix(','))
        .unwrap_or(trimmed);
    !core.is_empty() && core.chars().all(|ch| matches!(ch, ')' | ']' | '}'))
}

fn validate_range(start: usize, end: usize, line_count: usize, path: &str) -> Result<()> {
    ensure!(
        start > 0 && start <= end && end <= line_count,
        "Error: range {start}.={end} is outside {path} ({line_count} lines)"
    );
    Ok(())
}

fn ensure_target_free(targeted: &mut BTreeSet<usize>, start: usize, end: usize) -> Result<()> {
    for line in start..=end {
        ensure!(
            targeted.insert(line),
            "Error: anchor line {line} is already targeted by another hunk"
        );
    }
    Ok(())
}

fn cursor_boundary(cursor: &Cursor, line_count: usize, path: &str) -> Result<usize> {
    match cursor {
        Cursor::Head => Ok(0),
        Cursor::Tail => Ok(line_count),
        Cursor::Before(line) => {
            ensure!(
                *line > 0 && *line <= line_count,
                "Error: anchor line {line} is outside {path} ({line_count} lines)"
            );
            Ok(line - 1)
        }
        Cursor::After(line) => {
            ensure!(
                *line > 0 && *line <= line_count,
                "Error: anchor line {line} is outside {path} ({line_count} lines)"
            );
            Ok(*line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_missing_space_locators() {
        // 模型常写 `PUT18.=18:`（缺空格）——按 `PUT 18.=18:` 归一化并给 warning。
        let patch = parse("[a.rs#1A2B]\nPUT2.=2:\n+new").unwrap();
        assert_eq!(patch.sections[0].operations.len(), 1);
        assert_eq!(patch.sections[0].warnings.len(), 1);
        assert!(
            patch.sections[0].warnings[0].contains("Normalized missing space"),
            "{}",
            patch.sections[0].warnings[0]
        );
        let result = apply("a\nb\nc", &patch.sections[0], &mut Clipboard::default()).unwrap();
        assert_eq!(result.text, "a\nnew\nc");

        // gap 形态同样归一化：`PUT>40:` / `PUT<1:` / `CUT5.=8`。
        let patch = parse("[a.rs#1A2B]\nPUT>40:\n+tail").unwrap();
        assert_eq!(patch.sections[0].operations.len(), 1);
        let patch = parse("[a.rs#1A2B]\nCUT5.=8").unwrap();
        assert_eq!(patch.sections[0].operations.len(), 1);
        assert!(patch.sections[0].warnings[0].contains("Normalized missing space"));

        // 普通单词不会误归一化（PUT 后非定位符开头）。
        let err = parse("[a.rs#1A2B]\nPUTme 5.=5:\n+x").unwrap_err();
        assert!(err.to_string().contains("unknown hashline operation"));
    }

    #[test]
    fn parses_current_put_protocol() {
        let patch = parse("[a.rs#1A2B]\nPUT 2.=2:\n+B\nPUT >$:\n+c").unwrap();
        let result = apply("a\nb\n", &patch.sections[0], &mut Clipboard::default()).unwrap();
        assert_eq!(result.text, "a\nB\nc\n");
    }

    #[test]
    fn anonymous_and_named_registers_work() {
        let patch = parse("[a#AAAA]\nCUT 2.=2 @saved\nPUT <1 @saved").unwrap();
        let result = apply("a\nb\nc", &patch.sections[0], &mut Clipboard::default()).unwrap();
        assert_eq!(result.text, "b\na\nc");
    }

    #[test]
    fn rejects_block_locators() {
        assert!(
            parse("[a#AAAA]\nPUT 1*:\n+x")
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        assert!(
            parse("[a#AAAA]\nCUT 1*")
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        assert!(
            parse("[a#AAAA]\nPUT >1*:\n+x")
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        assert!(
            parse("[a#AAAA]\nPUT >1* @saved")
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
    }
}
