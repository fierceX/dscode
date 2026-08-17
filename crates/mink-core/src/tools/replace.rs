//! Content-anchored Replace matching.
//!
//! The matching and indentation contracts are behavior-level ports of the
//! current oh-my-pi Replace implementation. Mink keeps the pure core local so
//! it can enforce its own file, approval, artifact, and cancellation bounds.

use anyhow::{Result, bail, ensure};

const DOMINANT_FUZZY_MIN_CONFIDENCE: f64 = 0.97;
const DOMINANT_FUZZY_DELTA: f64 = 0.08;
const MAX_DIAGNOSTIC_MATCHES: usize = 5;
const DIAGNOSTIC_CONTEXT_LINES: usize = 5;
const DIAGNOSTIC_LINE_CHARS: usize = 80;
/// A fail-closed guard absent from upstream's pure helper. It prevents a
/// minified file from turning one fuzzy comparison into billions of DP cells.
const MAX_FUZZY_DP_CELLS: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceEntry {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone)]
pub struct ReplaceResult {
    pub content: String,
    pub count: usize,
    pub strategy: &'static str,
}

#[derive(Debug, Clone)]
struct Match {
    start: usize,
    end: usize,
    start_line: usize,
    confidence: f64,
    exact: bool,
}

#[derive(Debug, Default)]
struct MatchOutcome {
    matched: Option<Match>,
    closest: Option<Match>,
    occurrences: usize,
    occurrence_matches: Vec<Match>,
    fuzzy_matches: usize,
}

pub fn replace_text(
    content: &str,
    old_text: &str,
    new_text: &str,
    all: bool,
    allow_fuzzy: bool,
    threshold: f64,
    path: &str,
) -> Result<ReplaceResult> {
    ensure!(!old_text.is_empty(), "Error: old_text must not be empty");

    if all {
        let exact = exact_matches(content, old_text, &[]);
        if !exact.is_empty() {
            let result = content.replace(old_text, new_text);
            if result == content {
                if old_text == new_text {
                    return Ok(ReplaceResult {
                        content: result,
                        count: 0,
                        strategy: "idempotent",
                    });
                }
                bail!("Error: edit to {path} resulted in no changes");
            }
            return Ok(ReplaceResult {
                content: result,
                count: exact.len(),
                strategy: "exact",
            });
        }

        // `old == new` with no exact match: succeed idempotently only when a
        // fuzzy candidate actually exists (the file holds a near-identical
        // text and the edit must not normalize it); with no candidate at all
        // the call keeps failing closed instead of faking a successful update.
        if old_text == new_text {
            let probe = find_match(content, old_text, allow_fuzzy, threshold, &[]);
            if probe.matched.is_some() {
                return Ok(ReplaceResult {
                    content: content.to_string(),
                    count: 0,
                    strategy: "idempotent",
                });
            }
        }

        // Fuzzy all-replace path (old != new): replace every near candidate.
        let mut replacements: Vec<(Match, String)> = Vec::new();
        let mut last_outcome = find_match(content, old_text, allow_fuzzy, threshold, &[]);
        while let Some(matched) = last_outcome.matched.as_ref() {
            let actual = &content[matched.start..matched.end];
            let replacement = adjust_indentation(old_text, actual, new_text);
            if replacement == actual {
                break;
            }
            replacements.push((matched.clone(), replacement));

            let excluded = replacements
                .iter()
                .map(|(matched, _)| (matched.start, matched.end))
                .collect::<Vec<_>>();
            last_outcome = find_match(content, old_text, allow_fuzzy, threshold, &excluded);
        }

        if replacements.is_empty() {
            bail!(format_match_error(
                content,
                path,
                old_text,
                &last_outcome,
                allow_fuzzy,
                threshold
            ));
        }
        replacements.sort_by_key(|(matched, _)| matched.start);
        let mut result = String::with_capacity(content.len());
        let mut source_index = 0;
        for (matched, replacement) in &replacements {
            debug_assert!(matched.start >= source_index);
            result.push_str(&content[source_index..matched.start]);
            result.push_str(replacement);
            source_index = matched.end;
        }
        result.push_str(&content[source_index..]);
        ensure!(
            result != content || old_text == new_text,
            "Error: edit to {path} resulted in no changes"
        );
        if result == content {
            return Ok(ReplaceResult {
                content: result,
                count: 0,
                strategy: "idempotent",
            });
        }
        return Ok(ReplaceResult {
            content: result,
            count: replacements.len(),
            strategy: "fuzzy",
        });
    }

    let outcome = find_match(content, old_text, allow_fuzzy, threshold, &[]);
    if outcome.occurrences > 1 {
        bail!(format_occurrence_error(
            content,
            path,
            &outcome.occurrence_matches,
            outcome.occurrences
        ));
    }
    let Some(matched) = outcome.matched else {
        bail!(format_match_error(
            content,
            path,
            old_text,
            &outcome,
            allow_fuzzy,
            threshold
        ));
    };
    // Single-target `old == new`: the target exists (matched above) and the
    // edit is a no-op by definition — never rewrite the file.
    if old_text == new_text {
        return Ok(ReplaceResult {
            content: content.to_string(),
            count: 0,
            strategy: "idempotent",
        });
    }
    let actual = &content[matched.start..matched.end];
    let replacement = adjust_indentation(old_text, actual, new_text);
    let mut result = String::with_capacity(content.len() - actual.len() + replacement.len());
    result.push_str(&content[..matched.start]);
    result.push_str(&replacement);
    result.push_str(&content[matched.end..]);
    ensure!(
        result != content || old_text == new_text,
        "Error: edit to {path} resulted in no changes"
    );
    if result == content {
        return Ok(ReplaceResult {
            content: result,
            count: 0,
            strategy: "idempotent",
        });
    }
    Ok(ReplaceResult {
        content: result,
        count: 1,
        strategy: if matched.exact { "exact" } else { "fuzzy" },
    })
}

fn find_match(
    content: &str,
    target: &str,
    allow_fuzzy: bool,
    threshold: f64,
    excluded: &[(usize, usize)],
) -> MatchOutcome {
    let exact = exact_matches(content, target, excluded);
    if !exact.is_empty() {
        if exact.len() == 1 {
            return MatchOutcome {
                matched: exact.first().cloned(),
                ..MatchOutcome::default()
            };
        }
        return MatchOutcome {
            occurrences: exact.len(),
            occurrence_matches: exact.into_iter().take(MAX_DIAGNOSTIC_MATCHES).collect(),
            ..MatchOutcome::default()
        };
    }

    let content_lines = content.split('\n').collect::<Vec<_>>();
    let target_lines = target.split('\n').collect::<Vec<_>>();
    if target_lines.is_empty() || target_lines.len() > content_lines.len() {
        return MatchOutcome::default();
    }
    let offsets = line_offsets(content);
    let target_with_depth = normalize_lines(&target_lines, true);
    let target_without_depth = normalize_lines(&target_lines, false);
    let mut best: Option<Match> = None;
    let mut second_best: f64 = -1.0;
    let mut above_threshold = 0;

    for index in 0..=content_lines.len() - target_lines.len() {
        let start = offsets[index];
        let end = line_window_end(content, &offsets, index, target_lines.len());
        if excluded
            .iter()
            .any(|(excluded_start, excluded_end)| start < *excluded_end && end > *excluded_start)
        {
            continue;
        }
        let window = &content_lines[index..index + target_lines.len()];
        let with_depth = normalize_lines(window, true);
        let mut score = average_similarity(&with_depth, &target_with_depth, threshold);
        if score < threshold && score >= 0.8 {
            let without_depth = normalize_lines(window, false);
            score = score.max(average_similarity(
                &without_depth,
                &target_without_depth,
                threshold,
            ));
        }
        if score >= threshold {
            above_threshold += 1;
        }
        let candidate = Match {
            start,
            end,
            start_line: index + 1,
            confidence: score,
            exact: false,
        };
        match &best {
            Some(current) if score <= current.confidence => {
                second_best = second_best.max(score);
            }
            Some(current) => {
                second_best = second_best.max(current.confidence);
                best = Some(candidate);
            }
            None => best = Some(candidate),
        }
    }

    let Some(best) = best else {
        return MatchOutcome::default();
    };
    let matched = if allow_fuzzy
        && best.confidence >= threshold
        && (above_threshold == 1
            || (above_threshold > 1
                && best.confidence >= DOMINANT_FUZZY_MIN_CONFIDENCE
                && best.confidence - second_best >= DOMINANT_FUZZY_DELTA))
    {
        Some(best.clone())
    } else {
        None
    };
    MatchOutcome {
        matched,
        closest: Some(best),
        fuzzy_matches: above_threshold,
        ..MatchOutcome::default()
    }
}

fn exact_matches(content: &str, target: &str, excluded: &[(usize, usize)]) -> Vec<Match> {
    let mut found = Vec::new();
    let mut search_start = 0;
    while search_start <= content.len().saturating_sub(target.len()) {
        let Some(relative) = content[search_start..].find(target) else {
            break;
        };
        let start = search_start + relative;
        let end = start + target.len();
        if !excluded
            .iter()
            .any(|(excluded_start, excluded_end)| start < *excluded_end && end > *excluded_start)
        {
            found.push(Match {
                start,
                end,
                start_line: content[..start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
                confidence: 1.0,
                exact: true,
            });
        }
        search_start = end.max(start + 1);
    }
    found
}

fn line_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    offsets
}

fn line_window_end(content: &str, offsets: &[usize], index: usize, line_count: usize) -> usize {
    let next_line = index + line_count;
    if next_line < offsets.len() {
        offsets[next_line].saturating_sub(1)
    } else {
        content.len()
    }
}

fn normalize_lines(lines: &[&str], include_depth: bool) -> Vec<String> {
    let depths = include_depth.then(|| relative_indent_depths(lines));
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let prefix = depths
                .as_ref()
                .map_or_else(|| "|".to_string(), |depths| format!("{}|", depths[index]));
            format!("{prefix}{}", normalize_fuzzy(line.trim()))
        })
        .collect()
}

fn relative_indent_depths(lines: &[&str]) -> Vec<usize> {
    let indents = lines
        .iter()
        .map(|line| leading_whitespace(line).len())
        .collect::<Vec<_>>();
    let minimum = lines
        .iter()
        .zip(&indents)
        .filter(|(line, _)| !line.trim().is_empty())
        .map(|(_, indent)| *indent)
        .min()
        .unwrap_or(0);
    let unit = indents
        .iter()
        .filter_map(|indent| indent.checked_sub(minimum))
        .filter(|step| *step > 0)
        .min()
        .unwrap_or(1);
    lines
        .iter()
        .zip(indents)
        .map(|(line, indent)| {
            if line.trim().is_empty() {
                0
            } else {
                (indent.saturating_sub(minimum) + unit / 2) / unit
            }
        })
        .collect()
}

fn average_similarity(actual: &[String], target: &[String], threshold: f64) -> f64 {
    let mut total = 0.0;
    for (index, (actual, target)) in actual.iter().zip(target).enumerate() {
        let remaining = actual.len() - index - 1;
        let minimum_line_score =
            (threshold * actual.len() as f64 - total - remaining as f64).clamp(0.0, 1.0);
        let score = bounded_similarity(actual, target, minimum_line_score).unwrap_or(0.0);
        total += score;
        if (total + remaining as f64) / (actual.len() as f64) < threshold.min(0.8) {
            break;
        }
    }
    total / actual.len().max(1) as f64
}

fn bounded_similarity(a: &str, b: &str, minimum: f64) -> Option<f64> {
    if a == b {
        return Some(1.0);
    }
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    let maximum = a.len().max(b.len());
    if maximum == 0 {
        return Some(1.0);
    }
    let max_distance = ((1.0 - minimum.clamp(0.0, 1.0)) * maximum as f64).floor() as usize;
    if a.len().abs_diff(b.len()) > max_distance {
        return None;
    }
    let band_width = max_distance
        .saturating_mul(2)
        .saturating_add(1)
        .min(b.len() + 1);
    if a.len().saturating_mul(band_width) > MAX_FUZZY_DP_CELLS {
        return None;
    }

    let sentinel = max_distance.saturating_add(1);
    let mut previous = vec![sentinel; b.len() + 1];
    for (index, value) in previous
        .iter_mut()
        .enumerate()
        .take(max_distance.min(b.len()) + 1)
    {
        *value = index;
    }
    let mut current = vec![sentinel; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current.fill(sentinel);
        let row = i + 1;
        if row <= max_distance {
            current[0] = row;
        }
        let start = row.saturating_sub(max_distance).max(1);
        let end = row.saturating_add(max_distance).min(b.len());
        let mut row_min = sentinel;
        for column in start..=end {
            let cost = usize::from(*left != b[column - 1]);
            current[column] = previous[column]
                .saturating_add(1)
                .min(current[column - 1].saturating_add(1))
                .min(previous[column - 1].saturating_add(cost));
            row_min = row_min.min(current[column]);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[b.len()];
    (distance <= max_distance).then_some(1.0 - distance as f64 / maximum as f64)
}

fn normalize_fuzzy(value: &str) -> String {
    let mut result = String::new();
    let mut whitespace = false;
    for ch in value.trim().chars() {
        let ch = match ch {
            '„' | '‟' | '«' | '»' => '"',
            '‚' | '‛' | '`' | '´' => '\'',
            '‐' | '‑' | '‒' | '–' | '—' | '−' => '-',
            ' ' | '\t' => {
                whitespace = true;
                continue;
            }
            ch => ch,
        };
        if whitespace && !result.is_empty() {
            result.push(' ');
        }
        whitespace = false;
        result.push(ch);
    }
    result
}

#[derive(Debug)]
struct IndentProfile<'a> {
    lines: Vec<&'a str>,
    character: Option<char>,
    space_only: bool,
    tab_only: bool,
    mixed: bool,
    unit: usize,
    non_empty_count: usize,
}

fn indent_profile(text: &str) -> IndentProfile<'_> {
    let lines = text.split('\n').collect::<Vec<_>>();
    let mut indent_counts = Vec::new();
    let mut character = None;
    let mut space_only = true;
    let mut tab_only = true;
    let mut mixed = false;
    let mut non_empty_count = 0;
    let mut unit = 0;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        non_empty_count += 1;
        let indent = leading_whitespace(line);
        indent_counts.push(indent.len());
        if indent.contains(' ') {
            tab_only = false;
        }
        if indent.contains('\t') {
            space_only = false;
        }
        if indent.contains(' ') && indent.contains('\t') {
            mixed = true;
        }
        if let Some(current) = indent.chars().next() {
            if let Some(existing) = character {
                mixed |= existing != current;
            } else {
                character = Some(current);
            }
        }
    }
    if space_only && non_empty_count > 0 {
        unit = indent_counts
            .iter()
            .copied()
            .filter(|count| *count > 0)
            .reduce(gcd)
            .unwrap_or(0);
    } else if tab_only && non_empty_count > 0 {
        unit = 1;
    }
    IndentProfile {
        lines,
        character,
        space_only,
        tab_only,
        mixed,
        unit,
        non_empty_count,
    }
}

fn gcd(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn leading_whitespace(line: &str) -> &str {
    let length = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[..length]
}

fn indentation_only_rewrite(old_text: &str, new_text: &str) -> bool {
    let old = old_text.split('\n').collect::<Vec<_>>();
    let new = new_text.split('\n').collect::<Vec<_>>();
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(left, right)| left.trim() == right.trim())
}

fn adjust_indentation(old_text: &str, actual_text: &str, new_text: &str) -> String {
    if old_text == actual_text || indentation_only_rewrite(old_text, new_text) {
        return new_text.to_string();
    }
    let old = indent_profile(old_text);
    let actual = indent_profile(actual_text);
    let new = indent_profile(new_text);
    if old.non_empty_count == 0 || actual.non_empty_count == 0 || new.non_empty_count == 0 {
        return new_text.to_string();
    }
    if old.mixed || actual.mixed || new.mixed {
        return new_text.to_string();
    }
    if old.character.is_some() && actual.character.is_some() && old.character != actual.character {
        if actual.space_only && old.tab_only && new.tab_only && actual.unit > 0 {
            let compatible = old
                .lines
                .iter()
                .zip(&actual.lines)
                .filter(|(left, right)| !left.trim().is_empty() && !right.trim().is_empty())
                .all(|(left, right)| {
                    let old_indent = leading_whitespace(left);
                    old_indent.is_empty()
                        || leading_whitespace(right).len() == old_indent.len() * actual.unit
                });
            if compatible {
                return new_text
                    .split('\n')
                    .map(|line| {
                        let indent = leading_whitespace(line);
                        if indent.contains('\t') && !indent.contains(' ') {
                            format!(
                                "{}{}",
                                " ".repeat(indent.len() * actual.unit),
                                &line[indent.len()..]
                            )
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        return new_text.to_string();
    }

    let deltas = old
        .lines
        .iter()
        .zip(&actual.lines)
        .filter(|(left, right)| !left.trim().is_empty() && !right.trim().is_empty())
        .map(|(left, right)| {
            leading_whitespace(right).len() as isize - leading_whitespace(left).len() as isize
        })
        .collect::<Vec<_>>();
    let Some(delta) = deltas.first().copied() else {
        return new_text.to_string();
    };
    if delta == 0 || deltas.iter().any(|candidate| *candidate != delta) {
        return new_text.to_string();
    }
    if new.character.is_some() && actual.character.is_some() && new.character != actual.character {
        return new_text.to_string();
    }
    let indent_character = actual.character.or(old.character).unwrap_or(' ');
    new_text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            if delta > 0 {
                format!(
                    "{}{}",
                    indent_character.to_string().repeat(delta as usize),
                    line
                )
            } else {
                let remove = (-delta) as usize;
                let indent = leading_whitespace(line);
                line[remove.min(indent.len())..].to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_occurrence_error(content: &str, path: &str, matches: &[Match], total: usize) -> String {
    let lines = content.split('\n').collect::<Vec<_>>();
    let mut message = format!("Error: found {total} occurrences in {path}");
    for matched in matches.iter().take(MAX_DIAGNOSTIC_MATCHES) {
        message.push_str("\n\n");
        let center = matched.start_line.saturating_sub(1);
        let start = center.saturating_sub(DIAGNOSTIC_CONTEXT_LINES);
        let end = (center + DIAGNOSTIC_CONTEXT_LINES + 1).min(lines.len());
        for (offset, line) in lines[start..end].iter().enumerate() {
            let mut rendered = line.chars().take(DIAGNOSTIC_LINE_CHARS).collect::<String>();
            if line.chars().count() > DIAGNOSTIC_LINE_CHARS {
                rendered.pop();
                rendered.push('…');
            }
            message.push_str(&format!("  {} | {}\n", start + offset + 1, rendered));
        }
    }
    message.push_str("\nAdd more context lines to disambiguate.");
    message
}

fn format_match_error(
    content: &str,
    path: &str,
    old_text: &str,
    outcome: &MatchOutcome,
    allow_fuzzy: bool,
    threshold: f64,
) -> String {
    if outcome.occurrences > 1 {
        return format!(
            "Error: found {} occurrences in {path}; add more context",
            outcome.occurrences
        );
    }
    let Some(closest) = &outcome.closest else {
        return if allow_fuzzy {
            format!("Could not find a close enough match in {path}.")
        } else {
            format!(
                "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
            )
        };
    };
    let actual_text = &content[closest.start..closest.end];
    let (expected_line, actual_line) = first_different_line(old_text, actual_text);
    let hint = if allow_fuzzy {
        if outcome.fuzzy_matches > 1 {
            format!(
                "Found {} high-confidence matches. Provide more context to make it unique.",
                outcome.fuzzy_matches
            )
        } else {
            format!(
                "Closest match was below the {:.0}% similarity threshold.",
                threshold * 100.0
            )
        }
    } else {
        "Fuzzy matching is disabled. Enable 'Edit fuzzy match' in settings to accept high-confidence matches."
            .to_string()
    };
    format!(
        "Could not find {} in {path}.\n\nClosest match ({:.0}% similar) at line {}:\n  - {}\n  + {}\n{}",
        if allow_fuzzy {
            "a close enough match"
        } else {
            "the exact text"
        },
        closest.confidence * 100.0,
        closest.start_line,
        truncate_line(expected_line),
        truncate_line(actual_line),
        hint
    )
}

fn first_different_line<'a>(expected: &'a str, actual: &'a str) -> (&'a str, &'a str) {
    let expected_lines = expected.split('\n').collect::<Vec<_>>();
    let actual_lines = actual.split('\n').collect::<Vec<_>>();
    for index in 0..expected_lines.len().max(actual_lines.len()) {
        let left = expected_lines.get(index).copied().unwrap_or("");
        let right = actual_lines.get(index).copied().unwrap_or("");
        if left != right {
            return (left, right);
        }
    }
    (
        expected_lines.first().copied().unwrap_or(""),
        actual_lines.first().copied().unwrap_or(""),
    )
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= DIAGNOSTIC_LINE_CHARS {
        return line.to_string();
    }
    let mut value = line.chars().take(DIAGNOSTIC_LINE_CHARS).collect::<String>();
    value.pop();
    value.push('…');
    value
}

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
