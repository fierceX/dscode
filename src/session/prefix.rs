use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// ImmutablePrefix holds content that cannot change during a session.
/// This is the foundation for prefix-cache alignment: the system prompt
/// and tool definitions must produce byte-identical tokens across all
/// LLM requests within a session to maintain prefix-cache hits.
pub struct ImmutablePrefix {
    system_prompt: String,
    tools_json: Vec<Value>,
    fingerprint: String,
}

impl ImmutablePrefix {
    pub fn new(system_prompt: String, tools_json: Vec<Value>) -> Self {
        let mut hasher = DefaultHasher::new();
        system_prompt.hash(&mut hasher);
        let tools_str = serde_json::to_string(&tools_json).unwrap_or_default();
        tools_str.hash(&mut hasher);
        let fingerprint = format!("{:x}", hasher.finish());

        Self {
            system_prompt,
            tools_json,
            fingerprint,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn tools_json(&self) -> &[Value] {
        &self.tools_json
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn verify_fingerprint(&self) -> bool {
        let recomputed = Self::compute_fingerprint(&self.system_prompt, &self.tools_json);
        recomputed == self.fingerprint
    }

    fn compute_fingerprint(system_prompt: &str, tools_json: &[Value]) -> String {
        let mut hasher = DefaultHasher::new();
        system_prompt.hash(&mut hasher);
        let tools_str = serde_json::to_string(tools_json).unwrap_or_default();
        tools_str.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_is_deterministic() {
        let p1 = ImmutablePrefix::new("you are an agent".into(), vec![json!({"name":"Bash"})]);
        let p2 = ImmutablePrefix::new("you are an agent".into(), vec![json!({"name":"Bash"})]);
        assert_eq!(p1.fingerprint(), p2.fingerprint());
    }

    #[test]
    fn fingerprint_changes_on_system_prompt_change() {
        let p1 = ImmutablePrefix::new("you are an agent".into(), vec![json!({"name":"Bash"})]);
        let p2 = ImmutablePrefix::new(
            "you are a different agent".into(),
            vec![json!({"name":"Bash"})],
        );
        assert_ne!(p1.fingerprint(), p2.fingerprint());
    }

    #[test]
    fn fingerprint_changes_on_tools_change() {
        let p1 = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})]);
        let p2 = ImmutablePrefix::new(
            "agent".into(),
            vec![json!({"name":"Bash"}), json!({"name":"Read"})],
        );
        assert_ne!(p1.fingerprint(), p2.fingerprint());
    }

    #[test]
    fn verify_fingerprint_succeeds() {
        let p = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})]);
        assert!(p.verify_fingerprint());
    }
}
