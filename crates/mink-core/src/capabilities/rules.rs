use crate::capabilities::source::{CapabilityExposure, SourceLevel, SourceMeta};
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

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

pub trait RuleProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn load_rules(&self, ctx: &LoadContext<'_>) -> Result<Vec<LoadedRule>>;
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
    cwd: &Path,
    home: &Path,
    session_id: &str,
    resource_session_id: &str,
) -> Result<RuleSnapshot> {
    let providers = default_rule_providers();
    RuleSnapshot::load(
        &providers,
        &LoadContext {
            cwd,
            home,
            session_id,
            resource_session_id,
        },
    )
}

pub fn default_rule_providers() -> Vec<Arc<dyn RuleProvider>> {
    vec![Arc::new(BuiltInRuleProvider)]
}

impl RuleSnapshot {
    pub fn load(providers: &[Arc<dyn RuleProvider>], ctx: &LoadContext<'_>) -> Result<Self> {
        let mut order: Vec<usize> = (0..providers.len()).collect();
        order.sort_by(|a, b| {
            providers[*b]
                .priority()
                .cmp(&providers[*a].priority())
                .then_with(|| a.cmp(b))
        });

        let mut all = Vec::new();
        let mut by_name = BTreeMap::new();
        let mut warnings = Vec::new();
        for idx in order {
            let provider = &providers[idx];
            let mut loaded = provider.load_rules(ctx).map_err(|e| {
                anyhow!(
                    "Error: rule provider {} failed to load rules: {e}",
                    provider.id()
                )
            })?;
            loaded.sort_by(|a, b| a.rule.name.cmp(&b.rule.name));
            for rule in loaded {
                if !is_valid_rule_name(&rule.rule.name) {
                    bail!("Error: invalid rule name: {}", rule.rule.name);
                }
                if by_name.contains_key(&rule.rule.name) {
                    continue;
                }
                if matches!(rule.exposure, CapabilityExposure::HostOnly) {
                    warnings.push(crate::capabilities::CapabilityWarning {
                        provider_id: provider.id().to_string(),
                        message: format!("host-only rule '{}' is hidden", rule.rule.name),
                    });
                }
                by_name.insert(rule.rule.name.clone(), rule.clone());
                all.push(rule);
            }
        }

        let discoverable = all
            .iter()
            .filter(|rule| matches!(rule.exposure, CapabilityExposure::ModelDiscoverable))
            .cloned()
            .collect::<Vec<_>>();
        let always_apply = all
            .iter()
            .filter(|rule| {
                rule.rule.always_apply && !matches!(rule.exposure, CapabilityExposure::HostOnly)
            })
            .cloned()
            .collect::<Vec<_>>();
        let dependency_fingerprint = compute_dependency_fingerprint(&discoverable, &always_apply);
        Ok(Self {
            all,
            discoverable,
            always_apply,
            by_name,
            warnings,
            dependency_fingerprint,
        })
    }
}

struct BuiltInRuleProvider;

impl RuleProvider for BuiltInRuleProvider {
    fn id(&self) -> &'static str {
        "built-in-rules"
    }

    fn display_name(&self) -> &'static str {
        "built-in rules"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn load_rules(&self, _ctx: &LoadContext<'_>) -> Result<Vec<LoadedRule>> {
        let content = "- Be concise and concrete. No pleasantries, no explanations unless asked. Raw results only.\n- Prefer safe, exact edits.\n- Report failures clearly.";
        Ok(vec![LoadedRule {
            revision: crate::util::sha256_hex(content),
            rule: RuleCapability {
                name: "default-agent-rules".to_string(),
                description: "Default response and edit discipline".to_string(),
                content: content.to_string(),
                always_apply: true,
            },
            source: SourceMeta {
                provider_id: self.id().to_string(),
                provider_name: self.display_name().to_string(),
                level: SourceLevel::BuiltIn,
                source_path: None,
                display_label: Some("built-in".to_string()),
            },
            exposure: CapabilityExposure::ModelDiscoverable,
        }])
    }
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

fn is_valid_rule_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
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
