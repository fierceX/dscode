mod core;
mod document;
mod mission;
mod modules;
pub mod workflows;

pub use document::{PromptDocument, PromptSection, PromptSectionOrigin};
pub use workflows::{PromptFact, RenderedPromptPack, ResolvedPromptWorkflows, WorkflowRequirement};

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
}

pub struct PromptBuildContext;

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
        let build_context = PromptBuildContext;
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
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::ToolConfig;
    use crate::tools::catalog::ToolCatalog;
    use crate::tools::semantic_capabilities::ToolCapabilityRegistry;
    use crate::tools::surface::{AgentRole, ToolResolutionContext};

    fn builder_for(names: &[&str]) -> Builder {
        let mut config = ToolConfig::from_config(&Config::default());
        config.enabled_tools = Some(names.iter().map(|name| (*name).to_string()).collect());
        let resolution = ToolResolutionContext::from_runtime(AgentRole::Primary, &config, false);
        let surface = Arc::new(
            ModelToolSurface::resolve(ToolCatalog::builtin().unwrap(), &config, &resolution)
                .unwrap(),
        );
        let capabilities = Arc::new(
            ToolCapabilityRegistry::builtin()
                .resolve(&surface, &resolution)
                .unwrap(),
        );
        Builder {
            cwd: PathBuf::from("/tmp"),
            home: PathBuf::from("/home/user"),
            skill_snapshot: Arc::new(SkillSnapshot::default()),
            context_file_snapshot: Arc::new(ContextFileSnapshot::default()),
            rule_snapshot: Arc::new(RuleSnapshot::default()),
            mission_file: None,
            mission_content: None,
            tool_surface: surface,
            tool_capabilities: capabilities,
        }
    }

    #[test]
    fn empty_surface_has_no_generated_tool_guidance() {
        let document = builder_for(&[]).build_document().unwrap();
        assert!(
            document
                .sections()
                .iter()
                .find(|section| matches!(
                    section.origin,
                    PromptSectionOrigin::Tool | PromptSectionOrigin::Workflow
                ))
                .is_none()
        );
    }

    #[test]
    fn tool_and_workflow_sections_are_loaded_on_demand() {
        let document = builder_for(&["Read", "Grep", "Edit", "Bash", "TodoWrite", "SubAgent"])
            .build_document()
            .unwrap();
        let ids: Vec<_> = document
            .sections()
            .iter()
            .map(|section| section.id.as_str())
            .collect();
        assert!(ids.contains(&"todo-guidance"));
        assert!(ids.contains(&"sub-agent-guidance"));
        assert!(ids.contains(&"search-then-inspect"));
        assert!(ids.contains(&"anchored-edit"));
        assert!(ids.contains(&"specialized-provider-routing"));
        assert!(ids.contains(&"specialized-mutation-routing"));
    }

    #[test]
    fn selected_skill_resource_hint_requires_resource_read_capability() {
        fn selected_skill_builder(names: &[&str]) -> Builder {
            let mut builder = builder_for(names);
            builder.skill_snapshot = Arc::new(SkillSnapshot {
                selected: vec![crate::capabilities::ResolvedSkill {
                    info: crate::capabilities::SkillInfo {
                        name: "review".into(),
                        description: "review code".into(),
                        source: crate::capabilities::SkillSource::FileSystem,
                        base_dir: "/skills/review".into(),
                    },
                    content: "Review the target carefully.".into(),
                }],
                ..SkillSnapshot::default()
            });
            builder
        }

        let without_resource_read = selected_skill_builder(&[]).build_document().unwrap();
        let selected = without_resource_read
            .sections()
            .iter()
            .find(|section| section.id == "selected-skills")
            .unwrap();
        assert!(selected.content.contains("Review the target carefully."));
        assert!(selected.content.contains("Base directory: /skills/review"));
        assert!(!selected.content.contains("skill://review/"));

        let with_resource_read = selected_skill_builder(&["Read"]).build_document().unwrap();
        let selected = with_resource_read
            .sections()
            .iter()
            .find(|section| section.id == "selected-skills")
            .unwrap();
        assert!(selected.content.contains("Review the target carefully."));
        assert!(selected.content.contains("Resource base: skill://review/"));
    }

    #[test]
    fn mutation_routing_is_loaded_only_for_shell_and_specialized_mutation_tools() {
        let bash_write = builder_for(&["Bash", "Write"]).build_document().unwrap();
        let section = bash_write
            .sections()
            .iter()
            .find(|section| section.id == "specialized-mutation-routing")
            .unwrap();
        assert!(section.content.contains("Write"));
        assert!(section.content.contains("redirection or heredocs"));
        assert!(!section.content.contains("Edit"));

        let bash_edit = builder_for(&["Bash", "Read", "Edit"])
            .build_document()
            .unwrap();
        let section = bash_edit
            .sections()
            .iter()
            .find(|section| section.id == "specialized-mutation-routing")
            .unwrap();
        assert!(section.content.contains("Edit"));
        assert!(section.content.contains("sed or awk"));
        assert!(!section.content.contains("Write"));

        for names in [&["Bash"][..], &["Write"][..], &["Read", "Edit"][..]] {
            let document = builder_for(names).build_document().unwrap();
            assert!(
                document
                    .sections()
                    .iter()
                    .all(|section| section.id != "specialized-mutation-routing")
            );
        }
    }

    #[test]
    fn plan_lifecycle_restores_revision_and_cancellation_constraints() {
        let document = builder_for(&["PlanDraft", "PlanConfirm", "PlanClear"])
            .build_document()
            .unwrap();
        let section = document
            .sections()
            .iter()
            .find(|section| section.id == "plan-lifecycle")
            .unwrap();
        assert!(section.content.contains("every reply about the plan"));
        assert!(section.content.contains("revised draft before responding"));
        assert!(section.content.contains("PlanDraft"));
        assert!(section.content.contains("empty content"));
        assert!(section.content.contains("Only explicit confirmation"));
    }

    #[test]
    fn mission_cannot_replace_runtime_owned_sections() {
        let mut builder = builder_for(&["TodoWrite"]);
        builder.mission_content = Some("# todo-guidance\nreplace".into());
        assert!(
            builder
                .build_document()
                .unwrap_err()
                .to_string()
                .contains("reserved")
        );
    }

    #[test]
    fn mission_core_override_changes_provenance() {
        let mut builder = builder_for(&[]);
        builder.mission_content = Some("# execution-codes\ncustom".into());
        let document = builder.build_document().unwrap();
        let section = document
            .sections()
            .iter()
            .find(|section| section.id == "execution-codes")
            .unwrap();
        assert_eq!(section.origin, PromptSectionOrigin::ExternalOverride);
        assert_eq!(section.content, "custom");
    }

    #[test]
    fn generated_sections_declare_every_embedded_tool_name() {
        fn contains_canonical_name(content: &str, name: &str) -> bool {
            content.match_indices(name).any(|(start, _)| {
                let before = content[..start].chars().next_back();
                let end = start + name.len();
                let after = content[end..].chars().next();
                before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
                    && after.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
            })
        }
        let names: Vec<_> = ToolCatalog::builtin()
            .unwrap()
            .iter_compiled()
            .map(|(_, metadata)| metadata.name)
            .collect();
        let document = builder_for(&names).build_document().unwrap();
        for section in document.generated_sections() {
            for name in &names {
                if contains_canonical_name(&section.content, name) {
                    assert!(
                        section.referenced_tools.contains(name),
                        "generated section '{}' embeds undeclared tool '{}'",
                        section.id,
                        name
                    );
                }
            }
        }
    }

    #[test]
    fn every_valid_surface_preserves_combination_invariants() {
        let catalog = ToolCatalog::builtin().unwrap();
        let names: Vec<_> = catalog
            .iter_compiled()
            .map(|(_, metadata)| metadata.name)
            .collect();
        for role in [AgentRole::Primary, AgentRole::SubAgent] {
            for vfs in [false, true] {
                for mask in 0usize..(1usize << names.len()) {
                    let mut config = ToolConfig::from_config(&Config::default());
                    config.enabled_tools = Some(
                        names
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| mask & (1usize << index) != 0)
                            .map(|(_, name)| (*name).to_string())
                            .collect(),
                    );
                    let resolution = ToolResolutionContext::from_runtime(role, &config, vfs);
                    let Ok(surface) = ModelToolSurface::resolve(catalog, &config, &resolution)
                    else {
                        continue;
                    };
                    let capabilities = ToolCapabilityRegistry::builtin()
                        .resolve(&surface, &resolution)
                        .unwrap();
                    for (_, binding) in capabilities.iter() {
                        assert!(surface.has(binding.primary.tool));
                        assert!(
                            binding
                                .alternatives
                                .iter()
                                .all(|provider| surface.has(provider.tool))
                        );
                        if binding.primary.tier
                            == crate::tools::semantic_capabilities::ProviderTier::Fallback
                        {
                            assert!(
                                binding.alternatives.iter().all(|provider| {
                                    provider.tier
                                        == crate::tools::semantic_capabilities::ProviderTier::Fallback
                                }),
                                "fallback cannot outrank a specialized provider"
                            );
                        }
                    }
                    if role == AgentRole::SubAgent {
                        assert!(!surface.has("SubAgent"));
                        assert!(!capabilities.has(
                            crate::tools::semantic_capabilities::ToolSemanticCapability::Delegation
                        ));
                    }
                    if vfs {
                        assert!(!surface.has("Edit"));
                        assert!(!capabilities.has(
                            crate::tools::semantic_capabilities::ToolSemanticCapability::AnchoredEdit
                        ));
                        assert!(!capabilities.has(
                            crate::tools::semantic_capabilities::ToolSemanticCapability::EditableSnapshotRead
                        ));
                    }
                    let first = workflows::PromptWorkflowResolver::builtin()
                        .resolve(&capabilities)
                        .unwrap();
                    let second = workflows::PromptWorkflowResolver::builtin()
                        .resolve(&capabilities)
                        .unwrap();
                    assert_eq!(first.fingerprint(), second.fingerprint());
                    assert_eq!(
                        first
                            .ordered()
                            .iter()
                            .map(|spec| spec.id)
                            .collect::<Vec<_>>(),
                        second
                            .ordered()
                            .iter()
                            .map(|spec| spec.id)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}
