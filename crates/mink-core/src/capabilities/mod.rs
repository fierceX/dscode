pub mod context_files;
pub mod rules;
pub mod skills;
pub mod source;

use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::util::hex_lower;

pub use context_files::{ContextFileSnapshot, build_default_context_file_snapshot};
pub use rules::{RuleSnapshot, build_default_rule_snapshot};
pub use skills::{
    LoadContext as SkillLoadContext, LoadedSkill, ResolvedSkill, RuntimeSkill, SkillCapability,
    SkillDiscoveryPolicy, SkillInfo, SkillProvider, SkillSnapshot, SkillSource,
    build_default_skill_snapshot, skill_providers_for_policy,
};
pub use source::{CapabilityExposure, SourceLevel, SourceMeta};

#[derive(Debug, Clone, Default)]
pub struct CapabilitySnapshot {
    pub skills: SkillSnapshot,
    pub context_files: ContextFileSnapshot,
    pub rules: RuleSnapshot,
    pub warnings: Vec<CapabilityWarning>,
    pub dependency_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityWarning {
    pub provider_id: String,
    pub message: String,
}

impl CapabilitySnapshot {
    pub fn load_default(
        cwd: &std::path::Path,
        home: &std::path::Path,
        session_id: &str,
        resource_session_id: &str,
        selected_skills: &[String],
    ) -> anyhow::Result<Self> {
        let skills = build_default_skill_snapshot(
            cwd,
            home,
            session_id,
            resource_session_id,
            selected_skills,
        )?;
        let context_files =
            build_default_context_file_snapshot(cwd, home, session_id, resource_session_id)?;
        let rules = build_default_rule_snapshot(cwd, home, session_id, resource_session_id)?;
        let warnings = collect_warnings(&skills, &context_files, &rules);
        let dependency_fingerprint = compute_dependency_fingerprint(&[
            &skills.dependency_fingerprint,
            &context_files.dependency_fingerprint,
            &rules.dependency_fingerprint,
        ]);
        Ok(Self {
            skills,
            context_files,
            rules,
            warnings,
            dependency_fingerprint,
        })
    }

    pub fn load_from_skill_providers(
        providers: &[Arc<dyn SkillProvider>],
        cwd: &std::path::Path,
        home: &std::path::Path,
        session_id: &str,
        resource_session_id: &str,
        selected_skills: &[String],
    ) -> anyhow::Result<Self> {
        let skills = SkillSnapshot::load(
            providers,
            &skills::LoadContext {
                cwd,
                home,
                session_id,
                resource_session_id,
            },
            selected_skills,
        )?;
        let context_files =
            build_default_context_file_snapshot(cwd, home, session_id, resource_session_id)?;
        let rules = build_default_rule_snapshot(cwd, home, session_id, resource_session_id)?;
        let warnings = collect_warnings(&skills, &context_files, &rules);
        let dependency_fingerprint = compute_dependency_fingerprint(&[
            &skills.dependency_fingerprint,
            &context_files.dependency_fingerprint,
            &rules.dependency_fingerprint,
        ]);
        Ok(Self {
            skills,
            context_files,
            rules,
            warnings,
            dependency_fingerprint,
        })
    }
}

fn collect_warnings(
    skills: &SkillSnapshot,
    context_files: &ContextFileSnapshot,
    rules: &RuleSnapshot,
) -> Vec<CapabilityWarning> {
    let mut warnings = Vec::new();
    warnings.extend(skills.warnings.clone());
    warnings.extend(context_files.warnings.clone());
    warnings.extend(rules.warnings.clone());
    warnings
}

fn compute_dependency_fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex_lower(hasher.finalize())
}
