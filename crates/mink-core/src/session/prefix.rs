use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::capabilities::fingerprint::hex_lower;

/// ImmutablePrefix holds content that cannot change during a session.
/// This is the foundation for prefix-cache alignment: the system prompt
/// and tool definitions must produce byte-identical tokens across all
/// LLM requests within a session to maintain prefix-cache hits.
pub struct ImmutablePrefix {
    system_prompt: String,
    tools_json: Vec<Value>,
    dependency_fingerprint: String,
    fingerprint: String,
}

impl ImmutablePrefix {
    pub fn new(
        system_prompt: String,
        tools_json: Vec<Value>,
        dependency_fingerprint: String,
    ) -> Self {
        let fingerprint =
            Self::compute_fingerprint(&system_prompt, &tools_json, &dependency_fingerprint);

        Self {
            system_prompt,
            tools_json,
            dependency_fingerprint,
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

    pub fn dependency_fingerprint(&self) -> &str {
        &self.dependency_fingerprint
    }

    #[cfg(test)]
    pub(crate) fn new_with_fingerprint(
        system_prompt: String,
        tools_json: Vec<Value>,
        dependency_fingerprint: String,
        fingerprint: String,
    ) -> Self {
        Self {
            system_prompt,
            tools_json,
            dependency_fingerprint,
            fingerprint,
        }
    }

    pub fn verify_fingerprint(&self) -> bool {
        let recomputed = Self::compute_fingerprint(
            &self.system_prompt,
            &self.tools_json,
            &self.dependency_fingerprint,
        );
        recomputed == self.fingerprint
    }

    fn compute_fingerprint(
        system_prompt: &str,
        tools_json: &[Value],
        dependency_fingerprint: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(system_prompt.as_bytes());
        hasher.update(b"\0");
        let tools_str = serde_json::to_string(tools_json).unwrap_or_default();
        hasher.update(tools_str.as_bytes());
        hasher.update(b"\0");
        hasher.update(dependency_fingerprint.as_bytes());
        hex_lower(hasher.finalize())
    }
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;
