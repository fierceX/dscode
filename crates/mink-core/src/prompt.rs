mod core;
mod document;
mod mission;
mod modules;
pub mod workflows;

pub use document::{PromptDocument, PromptSection, PromptSectionOrigin};
pub use workflows::{RenderedPromptPack, ResolvedPromptWorkflows};

use crate::capabilities::{ContextFileSnapshot, RuleSnapshot, SkillSnapshot};
use crate::tools::semantic_capabilities::ResolvedToolCapabilities;
use crate::tools::surface::ModelToolSurface;
use anyhow::{Result, ensure};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub struct Builder {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub skill_snapshot: Arc<SkillSnapshot>,
    pub context_file_snapshot: Arc<ContextFileSnapshot>,
    pub rule_snapshot: Arc<RuleSnapshot>,
    pub mission_file: Option<PathBuf>,
    pub mission_content: Option<String>,
    pub tool_surface: Arc<ModelToolSurface>,
    pub tool_capabilities: Arc<ResolvedToolCapabilities>,
    pub edit_mode: crate::config::EditMode,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: f64,
    pub edit_enforce_seen_lines: bool,
    pub signal_policy: crate::config::SignalPolicy,
}

pub struct PromptBuildContext {
    pub edit_mode: crate::config::EditMode,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: f64,
    pub edit_enforce_seen_lines: bool,
}

impl Builder {
    pub fn build_system_prompt(&self) -> Result<String> {
        Ok(self.build_document()?.render())
    }

    pub fn build_document(&self) -> Result<PromptDocument> {
        let mut document = PromptDocument::new();
        core::append_core_sections(self, &mut document)?;

        if let Some(content) = self.mission_content()? {
            mission::apply(&mut document, &content, modules::reserved_section_ids())?;
        }

        modules::append_tool_sections(&mut document, &self.tool_surface)?;
        let workflows =
            workflows::PromptWorkflowResolver::builtin().resolve(&self.tool_capabilities)?;
        let build_context = PromptBuildContext {
            edit_mode: self.edit_mode,
            edit_fuzzy_match: self.edit_fuzzy_match,
            edit_fuzzy_threshold: self.edit_fuzzy_threshold,
            edit_enforce_seen_lines: self.edit_enforce_seen_lines,
        };
        for spec in workflows.ordered() {
            let rendered = (spec.render)(&build_context, &self.tool_capabilities)?;
            validate_pack(spec, &rendered, &workflows, &self.tool_surface)?;
            document.push(PromptSection {
                id: spec.id.to_string(),
                tag: spec.tag.to_string(),
                origin: PromptSectionOrigin::Workflow,
                content: rendered.content,
                name: None,
                referenced_tools: rendered.referenced_tools,
                consumed_facts: rendered.consumed_facts,
            })?;
        }

        core::append_external_sections(self, &mut document)?;
        document.move_to_end("output-language")?;
        document.validate_generated_references(&self.tool_surface, &workflows)?;
        Ok(document)
    }

    fn mission_content(&self) -> Result<Option<String>> {
        if let Some(content) = &self.mission_content {
            return Ok((!content.trim().is_empty()).then(|| content.clone()));
        }
        let Some(path) = &self.mission_file else {
            return Ok(None);
        };
        let content = fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!("failed to read mission file {}: {error}", path.display())
        })?;
        Ok((!content.trim().is_empty()).then_some(content))
    }
}

fn validate_pack(
    spec: &workflows::PromptWorkflowSpec,
    rendered: &RenderedPromptPack,
    workflows: &ResolvedPromptWorkflows,
    surface: &ModelToolSurface,
) -> Result<()> {
    for name in &rendered.referenced_tools {
        ensure!(
            surface.has(name),
            "workflow '{}' referenced inactive tool '{}'",
            spec.id,
            name
        );
    }
    for fact in &rendered.consumed_facts {
        ensure!(
            workflows.has_fact(*fact),
            "workflow '{}' consumed unresolved fact '{fact:?}'",
            spec.id
        );
    }
    ensure!(
        spec.requires.satisfied_by(&rendered.consumed_facts),
        "workflow '{}' render contract lacks a satisfying requirement witness",
        spec.id
    );
    Ok(())
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
