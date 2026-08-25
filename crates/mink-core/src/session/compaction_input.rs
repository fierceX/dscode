use serde_json::Value;

const MAX_TOOL_ARGS_CHARS: usize = 1_000;
const MAX_TOOL_RESULT_CHARS: usize = 2_000;

pub fn reduce_for_summary(messages: &[Value]) -> String {
    let mut out = String::from("<conversation>\n");
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match message.get("content") {
            Some(Value::String(text)) => push_text(&mut out, role, text),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            push_text(&mut out, role, text);
                        }
                        Some("tool_use") => push_tool_use(&mut out, block),
                        Some("tool_result") => push_tool_result(&mut out, block),
                        // Summary requests never carry pixels: an attachment
                        // degrades to a text marker using its persisted
                        // metadata (v7 §10).
                        Some("tool_attachment") => {
                            let url = block.get("url").and_then(Value::as_str).unwrap_or("?");
                            let format = block.get("format").and_then(Value::as_str).unwrap_or("?");
                            let width = block.get("width").and_then(Value::as_u64).unwrap_or(0);
                            let height = block.get("height").and_then(Value::as_u64).unwrap_or(0);
                            out.push_str(&format!(
                                "[image {format} {width}x{height}: {url}]\n"
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out.push_str("</conversation>");
    out
}

fn push_text(out: &mut String, role: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    out.push('[');
    out.push_str(role);
    out.push_str("]\n");
    out.push_str(text);
    out.push('\n');
}

fn push_tool_use(out: &mut String, block: &Value) {
    let id = block.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let input = block.get("input").unwrap_or(&Value::Null);
    let detail = concise_tool_input(name, input);
    out.push_str(&format!("[tool {name} id={id}] {detail}\n"));
}

fn push_tool_result(out: &mut String, block: &Value) {
    let id = block
        .get("tool_use_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let content = block.get("content").and_then(Value::as_str).unwrap_or("");
    out.push_str(&format!(
        "[tool_result id={id}] {}\n",
        reduce_tool_result(content.trim())
    ));
}

fn concise_tool_input(name: &str, input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    let concise = match name {
        "Read" | "Write" | "Edit" => field("path").map(|path| format!("path={path}")),
        "Bash" => field("command").map(|command| format!("command={command}")),
        "Python" | "PythonSandbox" => field("script")
            .or_else(|| field("script_file"))
            .map(|script| format!("script={script}")),
        "Grep" => {
            let pattern = field("pattern").unwrap_or("");
            let path = field("path").unwrap_or("");
            Some(format!("pattern={pattern} path={path}"))
        }
        "Glob" => field("pattern").map(|pattern| format!("pattern={pattern}")),
        _ => None,
    };
    truncate_middle(
        &concise.unwrap_or_else(|| {
            truncate_middle(
                &serde_json::to_string(input).unwrap_or_else(|_| "null".to_string()),
                MAX_TOOL_ARGS_CHARS,
            )
        }),
        MAX_TOOL_ARGS_CHARS,
    )
}

fn reduce_tool_result(content: &str) -> String {
    if content.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return content.to_string();
    }
    let evidence = content
        .lines()
        .filter(|line| is_evidence_line(line))
        .take(16)
        .collect::<Vec<_>>()
        .join("\n");
    if evidence.is_empty() {
        return truncate_middle(content, MAX_TOOL_RESULT_CHARS);
    }
    let evidence = truncate_middle(&evidence, MAX_TOOL_RESULT_CHARS / 2);
    let remaining = MAX_TOOL_RESULT_CHARS.saturating_sub(evidence.chars().count() + 64);
    let edge = truncate_middle(content, remaining);
    format!("{edge}\n... extracted evidence ...\n{evidence}")
}

fn is_evidence_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("traceback")
        || lower.contains("exit code")
        || lower.contains("blocked by")
        || lower.contains("was not executed")
        || lower.contains("artifact://")
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let marker = "\n... tool content reduced ...\n";
    let marker_chars = marker.chars().count();
    let available = max_chars.saturating_sub(marker_chars);
    let head_chars = available * 2 / 3;
    let tail_chars = available - head_chars;
    let head = value.chars().take(head_chars).collect::<String>();
    let mut tail = value.chars().rev().take(tail_chars).collect::<Vec<_>>();
    tail.reverse();
    format!("{head}{marker}{}", tail.into_iter().collect::<String>())
}

#[cfg(test)]
#[path = "compaction_input_tests.rs"]
mod tests;
