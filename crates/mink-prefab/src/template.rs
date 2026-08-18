use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Metadata stored beside a prefab template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub dsh_preset: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tool_surface: Vec<String>,
    #[serde(default)]
    pub placeholders: Vec<String>,
}

/// A loaded prefab template: Mink-native conversation lines plus metadata.
#[derive(Debug, Clone)]
pub struct PrefabTemplate {
    pub meta: TemplateMeta,
    pub conversation: Vec<Value>,
}

/// Load the bundled generic template.
pub fn load_builtin() -> Result<PrefabTemplate> {
    crate::builtin::default_template()
}

/// Load a template from a directory containing `meta.json` and
/// `conversation.jsonl`.
pub fn load_path(dir: &Path) -> Result<PrefabTemplate> {
    let meta_path = dir.join("meta.json");
    let conversation_path = dir.join("conversation.jsonl");
    let meta_text = fs::read_to_string(&meta_path)
        .with_context(|| format!("failed to read {}", meta_path.display()))?;
    let meta: TemplateMeta = serde_json::from_str(&meta_text)
        .with_context(|| format!("invalid template meta {}", meta_path.display()))?;
    let conversation = parse_conversation_jsonl(&conversation_path)?;
    validate(&meta, &conversation)?;
    Ok(PrefabTemplate { meta, conversation })
}

/// Parse Mink `conversation.jsonl` lines.
pub fn parse_conversation_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_conversation_str(&text)
        .with_context(|| format!("invalid conversation.jsonl at {}", path.display()))
}

/// Parse Mink `conversation.jsonl` content from a string.
pub fn parse_conversation_str(text: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("invalid conversation.jsonl line {}", idx + 1))?;
        out.push(value);
    }
    Ok(out)
}

/// Validate a template before seeding.
pub fn validate(meta: &TemplateMeta, conversation: &[Value]) -> Result<()> {
    ensure!(
        !meta.name.trim().is_empty(),
        "template name must not be empty"
    );
    ensure!(!conversation.is_empty(), "template conversation is empty");
    ensure!(
        conversation.iter().any(is_assistant_message),
        "template must contain at least one assistant message"
    );
    ensure!(
        conversation.iter().any(has_tool_use),
        "template must contain at least one tool_use in assistant history"
    );
    for (idx, msg) in conversation.iter().enumerate() {
        ensure!(
            msg.get("role").and_then(Value::as_str).is_some(),
            "conversation[{}] is missing a string role",
            idx
        );
    }
    Ok(())
}

pub(crate) fn is_assistant_message(value: &Value) -> bool {
    value.get("role").and_then(Value::as_str) == Some("assistant")
}

pub(crate) fn has_tool_use(value: &Value) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        })
}

/// Parse a template directory into `(meta, conversation)`.
pub fn parse_template_dir(dir: &Path) -> Result<(TemplateMeta, Vec<Value>)> {
    let template = load_path(dir)?;
    Ok((template.meta, template.conversation))
}

/// Render conversation lines with the given replacements.
///
/// Replacements are literal string substitutions applied to every string
/// value in the JSON tree.
pub fn render_conversation(conversation: &[Value], replacements: &[(&str, String)]) -> Vec<Value> {
    conversation
        .iter()
        .map(|value| render_value(value, replacements))
        .collect()
}

fn render_value(value: &Value, replacements: &[(&str, String)]) -> Value {
    match value {
        Value::String(s) => {
            let mut out = s.clone();
            for (from, to) in replacements {
                if out.contains(from) {
                    out = out.replace(from, to);
                }
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_value(item, replacements))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), render_value(v, replacements)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_template_is_valid() {
        let template = load_builtin().unwrap();
        validate(&template.meta, &template.conversation).unwrap();
        assert!(template.conversation.len() >= 4);
    }

    #[test]
    fn pro_alias_loads_default_template() {
        let template = crate::builtin::named_template("pro").unwrap();
        validate(&template.meta, &template.conversation).unwrap();
        assert_eq!(template.meta.name, "pro");
        assert!(
            template
                .meta
                .system_prompt
                .contains("You are a helpful software engineer assistant.")
        );
    }

    #[test]
    fn router_flash_weak_template_is_valid_and_small_surface() {
        let template = crate::builtin::named_template("router-flash-weak").unwrap();
        validate(&template.meta, &template.conversation).unwrap();
        assert_eq!(template.meta.name, "flash");
        assert_eq!(
            template.meta.tool_surface,
            vec!["Bash".to_string(), "Read".to_string()]
        );
        assert!(template.meta.system_prompt.contains("Before acting"));
    }

    #[test]
    fn builtin_template_aligns_with_dsh_prefab_trajectory() {
        let template = load_builtin().unwrap();
        let roles: Vec<&str> = template
            .conversation
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles[0], "user");
        assert_eq!(roles[1], "assistant");
        assert_eq!(roles[2], "user");
        assert_eq!(roles[6], "assistant");

        let first_user = template.conversation[0]["content"].as_str().unwrap();
        assert!(first_user.starts_with("Read the workspace-root AGENTS.md completely"));

        let load_user = template.conversation[3]["content"].as_str().unwrap();
        assert!(load_user.contains("加载完整使用说明"));

        let tool_names: Vec<&str> = template
            .conversation
            .iter()
            .filter(|m| is_assistant_message(m))
            .flat_map(|m| m["content"].as_array().unwrap())
            .filter(|b| b["type"] == "tool_use")
            .map(|b| b["name"].as_str().unwrap())
            .collect();
        assert_eq!(tool_names[0], "Bash");
        assert_eq!(tool_names[1], "load_full_instructions");

        let last = template.conversation.last().unwrap();
        assert!(
            last["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["type"] == "text" && b["text"] == "Ready.")
        );
    }
}
