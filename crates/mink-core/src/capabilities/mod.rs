pub mod skills;
pub mod source;

pub use skills::{SkillSnapshot, build_default_skill_snapshot};
pub use source::{CapabilityExposure, SourceLevel, SourceMeta};

#[derive(Debug, Clone, Default)]
pub struct CapabilitySnapshot {
    pub skills: SkillSnapshot,
    pub warnings: Vec<CapabilityWarning>,
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
        Ok(Self {
            warnings: skills.warnings.clone(),
            skills,
        })
    }
}
