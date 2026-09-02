//! Prefab-aware helpers so the router can be composed with Mink's trajectory
//! mode without treating seeded warm-up messages as real user tasks.

use crate::core::message_text;
use serde_json::Value;

/// Known Prefab warm-up user texts. These are seeded by `mink-prefab` and must
/// not influence task classification or round counting.
pub fn is_prefab_warmup_message(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("Read the workspace-root AGENTS.md")
        || t.contains("加载完整使用说明并准备就绪")
        || t == "Ready."
        || t == "Instructions loaded."
}

/// Extract real user message texts, optionally skipping Prefab warm-up.
pub fn extract_real_user_messages(messages: &[Value], prefab_aware: bool) -> Vec<String> {
    messages
        .iter()
        .filter(|msg| msg.get("role").and_then(Value::as_str) == Some("user"))
        .filter(|msg| !is_internal_message(msg))
        .filter_map(|msg| {
            let text = message_text(msg.get("content")?).trim().to_string();
            if text.is_empty() {
                return None;
            }
            if is_tool_result_message(msg) {
                return None;
            }
            if prefab_aware && is_prefab_warmup_message(&text) {
                return None;
            }
            Some(text)
        })
        .collect()
}

/// First real user message text, or `None`.
pub fn first_real_user_message(messages: &[Value], prefab_aware: bool) -> Option<String> {
    extract_real_user_messages(messages, prefab_aware)
        .into_iter()
        .next()
}

/// Number of real user rounds (excluding Prefab warm-up and tool results).
pub fn count_real_user_rounds(messages: &[Value], prefab_aware: bool) -> usize {
    extract_real_user_messages(messages, prefab_aware).len()
}

/// True when the system prompt already carries the Flash weak persona.
pub fn has_flash_persona(system_prompt: &str) -> bool {
    system_prompt.contains("Before acting, decide the task type (build or fix)")
        || system_prompt.contains("Reasoning-mode persona")
}

/// True when the conversation already contains an assistant tool_use.
pub fn has_tool_use(messages: &[Value]) -> bool {
    messages.iter().any(|msg| {
        msg.get("role").and_then(Value::as_str) == Some("assistant")
            && msg
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                })
    })
}

/// Index of the first real user message, if any.
pub fn first_real_user_index(messages: &[Value], prefab_aware: bool) -> Option<usize> {
    messages.iter().position(|msg| {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            return false;
        }
        if is_internal_message(msg) {
            return false;
        }
        if is_tool_result_message(msg) {
            return false;
        }
        let text = message_text(msg.get("content").unwrap_or(&Value::Null));
        let text = text.trim();
        !text.is_empty() && (!prefab_aware || !is_prefab_warmup_message(text))
    })
}

/// True when an assistant `tool_use` exists after the first real user message.
/// Prefab warm-up tool calls before the first real task are ignored.
pub fn has_tool_use_after_real_user(messages: &[Value], prefab_aware: bool) -> bool {
    let start = first_real_user_index(messages, prefab_aware).unwrap_or(0);
    messages.iter().skip(start).any(|msg| {
        msg.get("role").and_then(Value::as_str) == Some("assistant")
            && msg
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                })
    })
}

/// True when the last message is a real user text message (not a tool result
/// and not a Prefab warm-up message).
pub fn last_is_real_user(messages: &[Value], prefab_aware: bool) -> bool {
    let Some(last) = messages.last() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if is_internal_message(last) {
        return false;
    }
    if is_tool_result_message(last) {
        return false;
    }
    let text = message_text(last.get("content").unwrap_or(&Value::Null));
    let text = text.trim();
    !text.is_empty() && (!prefab_aware || !is_prefab_warmup_message(text))
}

/// Filter Mink tool schemas down to a core set. Supports both
/// `{"name": ...}` and OpenAI `{"function": {"name": ...}}` shapes.
pub fn filter_core_tools(tools: &[Value], core: &[&str]) -> Vec<Value> {
    let core = core
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    tools
        .iter()
        .filter(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| {
                    tool.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            core.contains(name)
        })
        .cloned()
        .collect()
}

fn is_tool_result_message(msg: &Value) -> bool {
    msg.get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

fn is_internal_message(msg: &Value) -> bool {
    msg.get("internal").and_then(Value::as_bool) == Some(true) || msg.get("_mink").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text})
    }

    fn tool_result(text: &str) -> Value {
        json!({"role":"user","content":[{"type":"tool_result","content":text}]})
    }

    fn assistant_tool_use() -> Value {
        json!({"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]})
    }

    #[test]
    fn prefab_warmup_is_skipped() {
        let messages = vec![
            user(
                "Read the workspace-root AGENTS.md completely before any future maintenance work. Use bash with exactly this command: cat ./AGENTS.md.",
            ),
            user("加载完整使用说明并准备就绪。"),
            user("Ready."),
            user("修复这个 bug"),
        ];
        let real = extract_real_user_messages(&messages, true);
        assert_eq!(real, vec!["修复这个 bug"]);
        assert_eq!(count_real_user_rounds(&messages, true), 1);
    }

    #[test]
    fn runtime_checkpoints_are_not_real_user_rounds() {
        let messages = vec![
            json!({"role":"user","internal":true,"content":"<compacted-summary>old</compacted-summary>"}),
            user("修复这个 bug"),
        ];
        assert_eq!(
            extract_real_user_messages(&messages, false),
            vec!["修复这个 bug"]
        );
        assert_eq!(first_real_user_index(&messages, false), Some(1));
        assert!(!last_is_real_user(&messages[..1], false));
    }

    #[test]
    fn tool_results_are_not_user_rounds() {
        let messages = vec![user("修复这个 bug"), tool_result("ok")];
        assert_eq!(
            extract_real_user_messages(&messages, false),
            vec!["修复这个 bug"]
        );
    }

    #[test]
    fn persona_dedup_detection() {
        assert!(has_flash_persona(
            "You are a helpful assistant.\nBefore acting, decide the task type (build or fix)"
        ));
        assert!(has_flash_persona("# Reasoning-mode persona (mink-router)"));
        assert!(!has_flash_persona(
            "You are a helpful software engineer assistant."
        ));
    }

    #[test]
    fn tool_use_detection() {
        let messages = vec![assistant_tool_use()];
        assert!(has_tool_use(&messages));
        assert!(!has_tool_use(&[user("hi")]));
    }

    #[test]
    fn last_real_user_detection() {
        assert!(last_is_real_user(&[user("修复这个 bug")], false));
        assert!(!last_is_real_user(&[tool_result("ok")], false));
        assert!(!last_is_real_user(&[user("Ready.")], true));
    }

    #[test]
    fn filter_tools_supports_both_shapes() {
        let tools = vec![
            json!({"name":"Read"}),
            json!({"type":"function","function":{"name":"Bash"}}),
            json!({"name":"Edit"}),
        ];
        let filtered = filter_core_tools(&tools, &["Bash", "Read"]);
        assert_eq!(filtered.len(), 2);
    }
}
