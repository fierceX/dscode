use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// Result of a truncation repair operation.
#[derive(Debug)]
pub struct TruncationResult {
    pub repaired: String,
    pub changed: bool,
    pub notes: Vec<String>,
    pub fallback: bool,
}

/// A recovered tool call.
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub name: String,
    pub arguments: String,
}

/// Bounds for regex input — DSML regex can be O(n²) on adversarial input.
const MAX_SCAVENGE_INPUT: usize = 100 * 1024;

// ---- Regex patterns ----

static XML_TOOL_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<tool_call>\s*(\{[^<]*\})\s*</tool_call>").unwrap());

static BRACKET_TOOL_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[TOOL_CALL\]\s*(\{[^\[]*\})\s*\[/TOOL_CALL\]").unwrap());

// ---- Main API ----

/// Scavenge tool calls from text content (reasoning or response text).
/// Tries multiple container formats while still routing through the current tool registry/schema.
pub fn scavenge_tool_calls(text: &str) -> Option<Vec<Value>> {
    if text.len() > MAX_SCAVENGE_INPUT {
        return None;
    }

    let mut results = Vec::new();

    // 1. Try DSML invoke format (DeepSeek-specific markup in reasoning_content)
    if let Some(dsml_calls) = scavenge_dsml(text) {
        for (name, args) in &dsml_calls {
            if let Ok(v) = serde_json::to_value(args) {
                let call = serde_json::json!({"name": name, "arguments": v});
                results.push(call);
            }
        }
        if !results.is_empty() {
            return Some(results);
        }
    }

    // 2. Try wrapper formats. The wrapper is only a container; the inner
    // JSON is still validated later by the current tool implementation.
    for re in [&*XML_TOOL_CALL_RE, &*BRACKET_TOOL_CALL_RE] {
        let caps: Vec<_> = re.captures_iter(text).collect();
        if !caps.is_empty() {
            for cap in &caps {
                if let Some(m) = cap.get(1)
                    && let Ok(v) = serde_json::from_str::<Value>(m.as_str())
                    && let Some(call) = coerce_to_tool_call(&v)
                {
                    results.push(call);
                }
            }
            if !results.is_empty() {
                return Some(results);
            }
        }
    }

    // 3. Bare JSON fallback. This is intentionally last because it has the
    // broadest match surface.
    if let Some(call) = scavenge_bare_json(text) {
        results.push(call);
        return Some(results);
    }

    None
}

/// Scavenge from both reasoning_content and content channels.
/// Deduplicates by (name, arguments) signature.
pub fn scavenge_combined(
    reasoning: Option<&str>,
    content: Option<&str>,
    max_calls: usize,
) -> (Vec<ToolCallInfo>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut calls = Vec::new();
    let mut notes = Vec::new();

    for (source, text) in [("reasoning", reasoning), ("content", content)] {
        let Some(text) = text else { continue };
        if text.is_empty() {
            continue;
        }

        if let Some(found) = scavenge_tool_calls(text) {
            for v in &found {
                let (name, args) = extract_name_args(v);
                let sig = format!("{}::{}", name, args);
                if seen.insert(sig) && !name.is_empty() {
                    if calls.len() >= max_calls {
                        notes.push(format!("scavenge reached max {} calls", max_calls));
                        break;
                    }
                    calls.push(ToolCallInfo {
                        name: name.clone(),
                        arguments: args,
                    });
                    notes.push(format!("scavenged {} from {}", name, source));
                }
            }
        }
    }

    (calls, notes)
}

// ---- DSML parsing ----

/// Parse DSML invoke blocks from text using simple string operations.
/// Format: <|DSML|invoke name="TOOL_NAME">
///           <|DSML|parameter name="KEY" string="true">VALUE<|DSML|parameter>
///         <|DSML|invoke>
fn scavenge_dsml(text: &str) -> Option<Vec<(String, serde_json::Map<String, Value>)>> {
    let mut results = Vec::new();
    let mut pos = 0;

    while pos < text.len() {
        let Some(invoke_start) = text[pos..].find("<|DSML|invoke name=\"") else {
            break;
        };
        let name_begin = pos + invoke_start + 20;
        let Some(name_end) = text[name_begin..].find('"') else {
            break;
        };
        let name = text[name_begin..name_begin + name_end].to_string();

        let body_begin = name_begin + name_end + 2; // skip closing ">
        let Some(invoke_end) = text[body_begin..].find("</|DSML|invoke>") else {
            break;
        };
        let body = &text[body_begin..body_begin + invoke_end];

        let mut args = serde_json::Map::new();
        let mut bpos = 0;
        while bpos < body.len() {
            let Some(param_start) = body[bpos..].find("<|DSML|parameter name=\"") else {
                break;
            };
            let key_begin = bpos + param_start + 23;
            let Some(key_end) = body[key_begin..].find('"') else {
                break;
            };
            let key = body[key_begin..key_begin + key_end].to_string();

            let val_search_start = key_begin + key_end + 1;
            let (val_start, is_json) = if body[val_search_start..].starts_with(" string=\"true\"") {
                (val_search_start + 15, false)
            } else if body[val_search_start..].starts_with(" string=\"false\"") {
                (val_search_start + 16, true)
            } else {
                (val_search_start, false)
            };

            let Some(param_end) = body[val_start..].find("<|DSML|parameter>") else {
                break;
            };
            let raw = body[val_start..val_start + param_end].trim().to_string();

            if is_json {
                match serde_json::from_str::<Value>(&raw) {
                    Ok(v) => {
                        args.insert(key, v);
                    }
                    Err(_) => {
                        args.insert(key, Value::String(raw));
                    }
                }
            } else {
                args.insert(key, Value::String(raw));
            }

            bpos = val_start + param_end + 18;
        }

        results.push((name, args));
        pos = body_begin + invoke_end + 15;
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

// ---- Bare JSON scanning ----

fn scavenge_bare_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let slice = &text[start..];
    let mut depth = 0i32;
    let mut end = 0;
    let mut in_string = false;
    let mut escaped = false;

    for (i, ch) in slice.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '"' => in_string = !in_string,
            '\\' if in_string => escaped = true,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    if end == 0 {
        return None;
    }

    let json_str = &slice[..end];
    let v: Value = serde_json::from_str(json_str).ok()?;

    coerce_to_tool_call(&v)
}

/// Try supported JSON shapes for tool call representation. This keeps
/// container recovery compatible while current tool implementations enforce
/// the active argument schema.
fn coerce_to_tool_call(v: &Value) -> Option<Value> {
    // Shape 1: { "name": "...", "arguments": {...} }
    if let Some(name) = v.get("name").and_then(Value::as_str)
        && !name.is_empty()
    {
        let args = v
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        return Some(serde_json::json!({"name": name, "arguments": args}));
    }

    // Shape 2: OpenAI-style { "type": "function", "function": { "name": "...", "arguments": "..." } }
    if v.get("type").and_then(Value::as_str) == Some("function")
        && let Some(func) = v.get("function")
        && let Some(name) = func.get("name").and_then(Value::as_str)
        && !name.is_empty()
    {
        let args_raw = func
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let args: Value =
            serde_json::from_str(args_raw).unwrap_or(Value::Object(Default::default()));
        return Some(serde_json::json!({"name": name, "arguments": args}));
    }

    // Shape 3: { "tool_name": "...", "tool_args": {...} } (R1 free-form variant)
    if let Some(name) = v.get("tool_name").and_then(Value::as_str)
        && !name.is_empty()
    {
        let args = v
            .get("tool_args")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        return Some(serde_json::json!({"name": name, "arguments": args}));
    }

    None
}

fn extract_name_args(v: &Value) -> (String, String) {
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = v
        .get("arguments")
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_default();
    (name, args)
}

/// Repair truncated JSON (unclosed braces, unterminated strings, trailing commas).
pub fn repair_truncated_json(input: &str) -> TruncationResult {
    let mut notes = Vec::new();

    if input.trim().is_empty() {
        notes.push("empty input -> {}".to_string());
        return TruncationResult {
            repaired: "{}".to_string(),
            changed: input != "{}",
            notes,
            fallback: false,
        };
    }

    // Fast path: already valid.
    if serde_json::from_str::<Value>(input).is_ok() {
        return TruncationResult {
            repaired: input.to_string(),
            changed: false,
            notes,
            fallback: false,
        };
    }

    let mut stack: Vec<u8> = Vec::new(); // b'{', b'[', b'"'
    let mut escaped = false;
    let mut in_string = false;
    let mut last_significant = 0;

    for (i, byte) in input.bytes().enumerate() {
        if !byte.is_ascii_whitespace() {
            last_significant = i;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if byte == b'"' {
                in_string = false;
                stack.pop();
            }
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                stack.push(b'"');
            }
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                stack.pop();
            }
            _ => {}
        }
    }

    let mut s = input[..=last_significant].to_string();

    // Trim trailing commas before closing brackets (e.g. `,}` -> `}`, `,]` -> `]`).
    let comma_close_re = regex::Regex::new(r",(\s*[}\]])").unwrap();
    loop {
        let next = comma_close_re.replace(&s, "$1").to_string();
        if next == s {
            break;
        }
        s = next;
        notes.push("trimmed trailing comma".to_string());
    }

    if s.ends_with(',') {
        s.pop();
        notes.push("trimmed trailing comma".to_string());
    }

    // Fill dangling key with null
    let trimmed = s.trim_end().to_string();
    if trimmed.ends_with(':') {
        s.push_str(" null");
        notes.push("filled dangling key with null".to_string());
    }

    // Close unterminated string
    if in_string {
        s.push('"');
        stack.pop();
        notes.push("closed unterminated string".to_string());
    }

    // Close remaining open structures in reverse
    while let Some(top) = stack.pop() {
        match top {
            b'{' => s.push('}'),
            b'[' => s.push(']'),
            b'"' => s.push('"'),
            _ => {}
        }
    }

    match serde_json::from_str::<Value>(&s) {
        Ok(_) => {
            let changed = s != input;
            TruncationResult {
                repaired: s,
                changed,
                notes,
                fallback: false,
            }
        }
        Err(e) => {
            notes.push(format!("fallback to {{}}: {}", e));
            TruncationResult {
                repaired: "{}".to_string(),
                changed: true,
                notes,
                fallback: true,
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    // ---- Existing scavenge tests ----

    #[test]
    fn scavenge_xml_style() {
        let text = r#"I'll update it <tool_call>{"name":"Edit","arguments":{"path":"src/a.rs","patch":"@src/a.rs#0A3B\nreplace 2:\n+new()"}}</tool_call> now"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Edit");
        assert_eq!(
            result[0]["arguments"]["patch"],
            "@src/a.rs#0A3B\nreplace 2:\n+new()"
        );
    }

    #[test]
    fn scavenge_bracket_style() {
        let text = r#"Let me check [TOOL_CALL]{"name":"Read","arguments":{"path":"/tmp/x:10-20"}}[/TOOL_CALL]"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Read");
        assert_eq!(result[0]["arguments"]["path"], "/tmp/x:10-20");
    }

    #[test]
    fn scavenge_bare_json() {
        let text = r#"Here: {"name":"Read","arguments":{"path":"artifact://bash-0001:1-20"}}"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Read");
        assert_eq!(result[0]["arguments"]["path"], "artifact://bash-0001:1-20");
    }

    #[test]
    fn no_tool_call_returns_none() {
        let text = "No tool calls here.";
        assert!(scavenge_tool_calls(text).is_none());
    }

    #[test]
    fn escape_hatches_fallback() {
        let text = r#"some text and then {"name": "Read", "arguments": {"path": "skill://debugging"}} trailing"#;
        let result = scavenge_tool_calls(text);
        assert!(result.is_some());
    }

    // ---- New: DSML ----

    #[test]
    fn scavenge_dsml_half_width() {
        let text = r#"<|DSML|invoke name="Read">
<|DSML|parameter name="path" string="true">/tmp/x.txt<|DSML|parameter>
</|DSML|invoke>"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Read");
        assert_eq!(result[0]["arguments"]["path"], "/tmp/x.txt");
    }

    #[test]
    fn scavenge_dsml_full_width() {
        let text = r#"<|DSML|invoke name="Grep">
<|DSML|parameter name="pattern" string="true">foo<|DSML|parameter>
</|DSML|invoke>"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Grep");
    }

    #[test]
    fn scavenge_dsml_with_json_param() {
        let text = r#"<|DSML|invoke name="Edit">
<|DSML|parameter name="path" string="true">/tmp/x<|DSML|parameter>
<|DSML|parameter name="patch" string="true">@/tmp/x#0A3B
replace 1:
+new<|DSML|parameter>
</|DSML|invoke>"#;
        let result = scavenge_tool_calls(text).unwrap();
        assert_eq!(result[0]["name"], "Edit");
        assert_eq!(result[0]["arguments"]["path"], "/tmp/x");
        assert!(
            result[0]["arguments"]["patch"]
                .as_str()
                .unwrap()
                .contains("replace 1")
        );
    }

    // ---- New: 3-shape coerce ----

    #[test]
    fn coerce_openai_style() {
        let v: Value = serde_json::from_str(
            r#"{"type":"function","function":{"name":"Read","arguments":"{\"path\":\"/x\"}"}}"#,
        )
        .unwrap();
        let result = coerce_to_tool_call(&v);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r["name"], "Read");
        assert_eq!(r["arguments"]["path"], "/x");
    }

    #[test]
    fn coerce_tool_name_style() {
        let v: Value = serde_json::from_str(
            r#"{"tool_name":"Read","tool_args":{"path":"session://current"}}"#,
        )
        .unwrap();
        let result = coerce_to_tool_call(&v);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result["name"], "Read");
        assert_eq!(result["arguments"]["path"], "session://current");
    }

    #[test]
    fn coerce_standard_style() {
        let v: Value =
            serde_json::from_str(r#"{"name":"Glob","arguments":{"pattern":"*.rs"}}"#).unwrap();
        let result = coerce_to_tool_call(&v);
        assert!(result.is_some());
        assert_eq!(result.unwrap()["name"], "Glob");
    }

    // ---- New: Truncation repair ----

    #[test]
    fn truncation_short_circuit_valid() {
        let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"}}"#);
        assert!(!r.changed);
        assert!(!r.fallback);
    }

    #[test]
    fn truncation_closes_brace() {
        let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"}"#);
        assert!(r.changed);
        assert!(!r.fallback);
        assert!(r.repaired.ends_with('}'));
        let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
        assert_eq!(parsed["arguments"]["command"], "ls");
    }

    #[test]
    fn truncation_closes_string() {
        let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"#);
        assert!(r.changed);
        assert!(!r.fallback);
        let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
        assert_eq!(parsed["name"], "Bash");
    }

    #[test]
    fn truncation_fills_dangling_key() {
        let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"#);
        assert!(r.changed);
        let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
        assert_eq!(parsed["name"], "Bash");
    }

    #[test]
    fn truncation_trailing_comma() {
        let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls",}}"#);
        assert!(r.changed);
        assert!(!r.fallback);
        let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
        assert_eq!(parsed["arguments"]["command"], "ls");
    }

    #[test]
    fn truncation_empty_fallback() {
        let r = repair_truncated_json("garbage");
        assert!(r.fallback);
        assert_eq!(r.repaired, "{}");
    }

    #[test]
    fn truncation_empty_input() {
        let r = repair_truncated_json("");
        assert_eq!(r.repaired, "{}");
    }

    // ---- New: Combined scavenge ----

    #[test]
    fn scavenge_combined_dedup() {
        let text1 = r#"<tool_call>{"name":"Read","arguments":{"path":"/x:1-4"}}</tool_call>"#;
        let text2 = r#"{"name":"Read","arguments":{"path":"/x:1-4"}}"#;
        let (calls, _) = scavenge_combined(Some(text1), Some(text2), 10);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn scavenge_combined_respects_max() {
        let text = r#"<tool_call>{"name":"Read","arguments":{"path":"/a"}}</tool_call>
<tool_call>{"name":"Read","arguments":{"path":"/b"}}</tool_call>
<tool_call>{"name":"Read","arguments":{"path":"/c"}}</tool_call>"#;
        let (calls, notes) = scavenge_combined(Some(text), None, 2);
        assert_eq!(calls.len(), 2);
        assert!(notes.iter().any(|n| n.contains("reached max")));
    }
}
