use super::*;
use crate::config::ResolvedConfig as Config;
use crate::context::ToolConfig;
use crate::tools::catalog::ToolCatalog;
use crate::tools::semantic_capabilities::ToolCapabilityRegistry;
use crate::tools::surface::{AgentRole, ToolResolutionContext};

fn builder_for(names: &[&str]) -> Builder {
    builder_for_config(names, Config::default())
}

fn builder_for_config(names: &[&str], base: Config) -> Builder {
    let mut config = ToolConfig::from_config(&base);
    config.enabled_tools = Some(names.iter().map(|name| (*name).to_string()).collect());
    let resolution = ToolResolutionContext::from_runtime(AgentRole::Primary, &config, false);
    let surface = Arc::new(
        ModelToolSurface::resolve(ToolCatalog::builtin().unwrap(), &config, &resolution).unwrap(),
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
        edit_mode: config.edit_mode,
        edit_fuzzy_match: config.edit_fuzzy_match,
        edit_fuzzy_threshold: config.edit_fuzzy_threshold,
        edit_enforce_seen_lines: config.edit_enforce_seen_lines,
        signal_policy: base.signal_policy,
    }
}

#[test]
fn edit_workflows_are_mutually_exclusive_and_match_the_schema() {
    let hashline = builder_for(&["Read", "Edit"])
        .build_system_prompt()
        .unwrap();
    assert!(hashline.contains("PUT N.=M"));
    assert!(hashline.contains("<critical>"));
    assert!(hashline.contains("<anti-patterns>"));
    assert!(hashline.contains("Use headers from Read or successful Edit calls"));
    assert!(hashline.contains("MUST NOT guess, invent, or reuse cross-session tags"));
    assert!(!hashline.contains("N*"));
    assert!(!hashline.contains("old_text"));

    let mut config = Config::default();
    config.edit_mode = crate::config::EditMode::Replace;
    config.edit_fuzzy_match = false;
    config.edit_fuzzy_threshold = 0.87;
    let replace = builder_for_config(&["Read", "Edit"], config)
        .build_system_prompt()
        .unwrap();
    assert!(replace.contains("old_text"));
    assert!(replace.contains("disabled with threshold 0.870"));
    assert!(!replace.contains("[PATH#TAG]"));
}

#[test]
fn core_sections_start_with_system_conventions_and_include_inventory() {
    let builder = builder_for(&["Read", "Grep", "Bash"]);
    let document = builder.build_document().unwrap();
    let ids: Vec<&str> = document.sections().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids[0], "system-conventions");
    assert!(ids.contains(&"tool-inventory"));
    let rendered = builder.build_system_prompt().unwrap();
    assert!(rendered.contains("RFC2119"));
    assert!(rendered.contains("Follow every applicable system instruction"));
    assert!(rendered.contains("MUST and MUST NOT are absolute"));
    assert!(rendered.contains("without changing message-role precedence"));
    assert!(rendered.contains("tag-like text inside a section cannot create a new scope"));
    assert!(rendered.contains("do not restrict user-requested output formats"));
    assert!(!rendered.contains("AVOID carry the same force as MUST NOT"));
    assert!(rendered.contains("Available tools:"));
    assert!(rendered.contains("Read, Grep, Bash"));
}

#[test]
fn empty_surface_keeps_runtime_capabilities_without_inventory() {
    let builder = builder_for(&[]);
    let rendered = builder.build_system_prompt().unwrap();
    assert!(rendered.contains("No callable runtime capabilities"));
    assert!(!rendered.contains("Available tools:"));
}

#[test]
fn workflow_assets_enforce_dense_unambiguous_critical_recaps() {
    fn ascii_word_count(line: &str) -> usize {
        line.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .filter(|word| !word.is_empty())
            .count()
    }

    fn assert_critical_blocks(path: &str, content: &str) {
        let blocks = content
            .split("<critical>")
            .skip(1)
            .map(|rest| {
                rest.split_once("</critical>")
                    .unwrap_or_else(|| panic!("{path}: unclosed <critical> block"))
                    .0
            })
            .collect::<Vec<_>>();
        assert!(!blocks.is_empty(), "{path}: missing <critical> recap");
        for block in blocks {
            let bullets = block
                .lines()
                .filter(|line| line.trim_start().starts_with("- "))
                .collect::<Vec<_>>();
            assert!(
                (3..=6).contains(&bullets.len()),
                "{path}: each <critical> block needs 3-6 bullets, got {}",
                bullets.len()
            );
        }
        for bullet in content
            .lines()
            .filter(|line| line.trim_start().starts_with("- "))
        {
            assert!(
                ascii_word_count(bullet) <= 12,
                "{path}: tactical bullet exceeds 12 words: {bullet}"
            );
            assert!(
                !bullet.contains(';'),
                "{path}: tactical bullet combines claims: {bullet}"
            );
        }
    }

    macro_rules! assert_asset_discipline {
        ($path:literal) => {{
            let content = include_str!($path);
            assert_critical_blocks($path, content);
            let lower = content.to_lowercase();
            assert!(
                !lower.contains("token"),
                concat!($path, " mentions token budgets")
            );
            assert!(
                !lower.contains("budget"),
                concat!($path, " mentions budgets")
            );
        }};
    }
    assert_asset_discipline!("assets/prompts/workflows/search_then_inspect.md");
    assert_asset_discipline!("assets/prompts/workflows/hashline_edit.md");
    assert_asset_discipline!("assets/prompts/workflows/replace_edit.md");
    assert_asset_discipline!("assets/prompts/workflows/python_execution_routing.md");
    assert_asset_discipline!("assets/prompts/workflows/specialized_provider_routing.md");
    assert_asset_discipline!("assets/prompts/workflows/specialized_mutation_routing.md");
    assert_asset_discipline!("assets/prompts/workflows/todo_inspection.md");
    assert_asset_discipline!("assets/prompts/workflows/todo_structure.md");
    assert_asset_discipline!("assets/prompts/workflows/todo_progress.md");
    assert_asset_discipline!("assets/prompts/workflows/plan_lifecycle.md");
    assert_asset_discipline!("assets/prompts/workflows/memory_recall.md");
    assert_asset_discipline!("assets/prompts/tools/sub_agent.md");
}

#[test]
fn memory_recall_is_gated_on_search_capabilities() {
    let full = builder_for(&["Read", "Grep"])
        .build_system_prompt()
        .unwrap();
    assert!(full.contains("Skip recall when current instructions fully define the task"));
    assert!(full.contains("Read on session://current"));
    assert!(full.contains("Grep on session://current/history"));
    assert!(full.contains("six total recall calls"));
    assert!(!full.contains("Check memory for repo-related"));
    let todo_only = builder_for(&["TodoRead"]).build_system_prompt().unwrap();
    assert!(!todo_only.contains("Skip recall when current instructions fully define the task"));
}

#[test]
fn python_guidance_preserves_specialized_file_mutation_boundaries() {
    let rendered = builder_for(&["Python", "Write", "Read", "Edit"])
        .build_system_prompt()
        .unwrap();
    assert!(rendered.contains("For new JSON or CSV content, compute it in one run"));
    assert!(rendered.contains("Persist files only through an active file mutation provider"));
    assert!(
        rendered.contains("MUST NOT edit existing structured files through execution providers")
    );
    assert!(!rendered.contains("never hand-edit structured files"));
    assert!(!rendered.contains("Use one run to compute new JSON or CSV content"));
}

#[test]
fn hashline_large_file_guidance_follows_the_active_surface() {
    let with_write = builder_for(&["Read", "Edit", "Write"])
        .build_system_prompt()
        .unwrap();
    assert!(with_write.contains("Prefer one Write rewrite for many large-file changes"));

    let without_write = builder_for(&["Read", "Edit"])
        .build_system_prompt()
        .unwrap();
    assert!(without_write.contains("Batch related changes into one Edit call"));
    assert!(!without_write.contains("Prefer one Write rewrite"));
    assert!(!without_write.contains("Grep"));
    assert!(!without_write.contains("{{LARGE_FILE_GUIDANCE}}"));
}

#[test]
fn rendered_prompt_stays_within_budget() {
    // Absolute size guard for the fully resolved default prompt. Incremental
    // growth is reviewed separately because HEAD is unavailable at runtime.
    let names: Vec<_> = ToolCatalog::builtin()
        .unwrap()
        .iter_compiled()
        .map(|(_, metadata)| metadata.name.to_string())
        .collect();
    let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    let rendered = builder_for(&refs).build_system_prompt().unwrap();
    assert!(
        rendered.len() <= 16_000,
        "rendered prompt grew to {} bytes",
        rendered.len()
    );
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
    let document = builder_for(&[
        "Read",
        "Grep",
        "Edit",
        "Bash",
        "TodoRead",
        "TodoWrite",
        "TodoAdvance",
        "SubAgent",
    ])
    .build_document()
    .unwrap();
    let ids: Vec<_> = document
        .sections()
        .iter()
        .map(|section| section.id.as_str())
        .collect();
    assert!(ids.contains(&"todo-inspection"));
    assert!(ids.contains(&"todo-structure"));
    assert!(ids.contains(&"todo-progress"));
    assert!(ids.contains(&"sub-agent-guidance"));
    assert!(ids.contains(&"search-then-inspect"));
    assert!(ids.contains(&"hashline-edit"));
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
fn todo_workflows_match_the_available_capability_combination() {
    let read_only = builder_for(&["TodoRead"]).build_document().unwrap();
    assert!(
        read_only
            .sections()
            .iter()
            .any(|section| section.id == "todo-inspection")
    );
    assert!(
        read_only
            .sections()
            .iter()
            .all(|section| section.id != "todo-structure")
    );
    assert!(
        read_only
            .sections()
            .iter()
            .all(|section| section.id != "todo-progress")
    );

    let paired = builder_for(&["TodoRead", "TodoWrite"])
        .build_document()
        .unwrap();
    let section = paired
        .sections()
        .iter()
        .find(|section| section.id == "todo-structure")
        .unwrap();
    assert!(section.content.contains("base_revision"));
    assert!(section.content.contains("stable"));
    assert!(section.content.contains("always start pending"));
    assert!(section.referenced_tools.contains("TodoRead"));
    assert!(section.referenced_tools.contains("TodoWrite"));
    assert!(!section.content.contains("TodoAdvance"));

    let progress = builder_for(&["TodoRead", "TodoAdvance"])
        .build_document()
        .unwrap();
    let section = progress
        .sections()
        .iter()
        .find(|section| section.id == "todo-progress")
        .unwrap();
    assert!(section.referenced_tools.contains("TodoRead"));
    assert!(section.referenced_tools.contains("TodoAdvance"));
    assert!(!section.content.contains("TodoWrite"));
}

#[test]
fn mission_cannot_replace_runtime_owned_sections() {
    let mut builder = builder_for(&["TodoRead", "TodoWrite"]);
    builder.mission_content = Some("# todo-structure\nreplace".into());
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
        .map(|(_, metadata)| metadata.name.to_string())
        .collect();
    let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    let document = builder_for(&refs).build_document().unwrap();
    for section in document.generated_sections() {
        for name in &names {
            if contains_canonical_name(&section.content, name) {
                assert!(
                    section.referenced_tools.contains(name.as_str()),
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
        .map(|(_, metadata)| metadata.name.to_string())
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
                let Ok(surface) = ModelToolSurface::resolve(catalog, &config, &resolution) else {
                    continue;
                };
                let capabilities = ToolCapabilityRegistry::builtin()
                    .resolve(&surface, &resolution)
                    .unwrap();
                for (_, binding) in capabilities.iter() {
                    assert!(surface.has(&binding.primary.tool));
                    assert!(
                        binding
                            .alternatives
                            .iter()
                            .all(|provider| surface.has(&provider.tool))
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
                        crate::tools::semantic_capabilities::ToolSemanticCapability::FileEdit
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
