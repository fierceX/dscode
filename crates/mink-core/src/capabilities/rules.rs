use crate::capabilities::source::{CapabilityExposure, SourceLevel, SourceMeta};
 use anyhow::Result;
use std::collections::BTreeMap;
 use std::path::Path;

pub struct LoadContext<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub session_id: &'a str,
    pub resource_session_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct RuleCapability {
    pub name: String,
    pub description: String,
    pub content: String,
    pub always_apply: bool,
}

#[derive(Debug, Clone)]
pub struct LoadedRule {
    pub rule: RuleCapability,
    pub source: SourceMeta,
    pub exposure: CapabilityExposure,
    pub revision: String,
}


#[derive(Debug, Clone, Default)]
pub struct RuleSnapshot {
    pub all: Vec<LoadedRule>,
    pub discoverable: Vec<LoadedRule>,
    pub always_apply: Vec<LoadedRule>,
    pub by_name: BTreeMap<String, LoadedRule>,
    pub warnings: Vec<crate::capabilities::CapabilityWarning>,
    pub dependency_fingerprint: String,
}

 pub fn build_default_rule_snapshot(
     _cwd: &Path,
     _home: &Path,
     _session_id: &str,
     _resource_session_id: &str,
 ) -> Result<RuleSnapshot> {
     let content = "- Be concise and concrete. No pleasantries, no explanations unless asked. Raw results only.\n- Prefer safe, exact edits.\n- Report failures clearly.";
     let rule = LoadedRule {
         revision: crate::util::sha256_hex(content),
         rule: RuleCapability {
             name: "default-agent-rules".to_string(),
             description: "Default response and edit discipline".to_string(),
             content: content.to_string(),
             always_apply: true,
         },
         source: SourceMeta {
             provider_id: "built-in-rules".to_string(),
             provider_name: "built-in rules".to_string(),
             level: SourceLevel::BuiltIn,
             source_path: None,
             display_label: Some("built-in".to_string()),
         },
         exposure: CapabilityExposure::ModelDiscoverable,
     };
     let discoverable = vec![rule.clone()];
     let always_apply = vec![rule.clone()];
     let dependency_fingerprint = compute_dependency_fingerprint(&discoverable, &always_apply);
     let mut by_name = BTreeMap::new();
     by_name.insert(rule.rule.name.clone(), rule);
     Ok(RuleSnapshot {
         all: discoverable.clone(),
         discoverable,
         always_apply,
         by_name,
         warnings: Vec::new(),
         dependency_fingerprint,
     })
 }

fn compute_dependency_fingerprint(
    discoverable: &[LoadedRule],
    always_apply: &[LoadedRule],
) -> String {
    let mut input = String::new();
    for rule in discoverable {
        input.push_str("rule:index\0");
        input.push_str(&rule.rule.name);
        input.push('\0');
        input.push_str(&rule.rule.description);
        input.push('\0');
        input.push_str(&rule.source.provider_id);
        input.push('\0');
        input.push_str(&rule.revision);
        input.push('\0');
    }
    for rule in always_apply {
        input.push_str("rule:always\0");
        input.push_str(&rule.rule.name);
        input.push('\0');
        input.push_str(&rule.revision);
        input.push('\0');
    }
    crate::util::sha256_hex(&input)
 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_snapshot_loads_builtin_rules() {
        let cwd = std::path::PathBuf::from("/tmp/project");
        let home = std::path::PathBuf::from("/tmp/home");
        let snapshot = build_default_rule_snapshot(&cwd, &home, "session", "session").unwrap();

        assert!(snapshot.by_name.contains_key("default-agent-rules"));
        assert_eq!(snapshot.always_apply.len(), 1);
        assert_eq!(snapshot.discoverable.len(), 1);
    }
}
