use super::PromptBuildContext;
use crate::tools::semantic_capabilities::{
    ProviderTier, ResolvedToolCapabilities, ToolSemanticCapability,
};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PromptFact {
    ToolCapability(ToolSemanticCapability),
    SpecializedWithFallback(ToolSemanticCapability),
    Workflow(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub enum WorkflowRequirement {
    Fact(PromptFact),
    All(&'static [PromptFact]),
    Any(&'static [PromptFact]),
    AllWithAny {
        all: &'static [PromptFact],
        any: &'static [PromptFact],
    },
}

impl WorkflowRequirement {
    pub fn satisfied_by(&self, facts: &BTreeSet<PromptFact>) -> bool {
        match self {
            Self::Fact(fact) => facts.contains(fact),
            Self::All(required) => required.iter().all(|fact| facts.contains(fact)),
            Self::Any(required) => required.iter().any(|fact| facts.contains(fact)),
            Self::AllWithAny { all, any } => {
                all.iter().all(|fact| facts.contains(fact))
                    && any.iter().any(|fact| facts.contains(fact))
            }
        }
    }

    fn workflow_dependencies(&self) -> impl Iterator<Item = &'static str> {
        let facts: Vec<PromptFact> = match self {
            Self::Fact(fact) => vec![*fact],
            Self::All(facts) | Self::Any(facts) => facts.to_vec(),
            Self::AllWithAny { all, any } => all.iter().chain(*any).copied().collect(),
        };
        facts.into_iter().filter_map(|fact| match fact {
            PromptFact::Workflow(id) => Some(id),
            _ => None,
        })
    }
}

#[derive(Debug)]
pub struct PromptWorkflowSpec {
    pub id: &'static str,
    pub tag: &'static str,
    pub requires: WorkflowRequirement,
    pub exclusive_group: Option<&'static str>,
    pub priority: u16,
    pub render: fn(&PromptBuildContext, &ResolvedToolCapabilities) -> Result<RenderedPromptPack>,
}

pub struct RenderedPromptPack {
    pub content: String,
    pub referenced_tools: BTreeSet<&'static str>,
    pub consumed_facts: BTreeSet<PromptFact>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPromptWorkflows {
    ordered: Vec<&'static PromptWorkflowSpec>,
    facts: BTreeSet<PromptFact>,
    fingerprint: String,
}

pub struct PromptWorkflowResolver {
    specs: &'static [PromptWorkflowSpec],
}

use PromptFact::{SpecializedWithFallback, ToolCapability};
use ToolSemanticCapability::*;

const SEARCH_FACTS: &[PromptFact] = &[ToolCapability(ContentSearch), ToolCapability(PathRead)];
const EDIT_FACTS: &[PromptFact] = &[
    ToolCapability(EditableSnapshotRead),
    ToolCapability(AnchoredEdit),
];
const ROUTING_FACTS: &[PromptFact] = &[
    SpecializedWithFallback(PathRead),
    SpecializedWithFallback(ContentSearch),
    SpecializedWithFallback(PathDiscovery),
];
const MUTATION_ROUTING_ALL: &[PromptFact] = &[ToolCapability(ShellExec)];
const MUTATION_ROUTING_ANY: &[PromptFact] = &[
    ToolCapability(FileCreate),
    ToolCapability(FileOverwrite),
    ToolCapability(AnchoredEdit),
];
const PYTHON_FACTS: &[PromptFact] = &[
    ToolCapability(HostPythonExec),
    ToolCapability(SandboxedPythonExec),
];
const PLAN_ALL: &[PromptFact] = &[
    ToolCapability(PlanDraft),
    ToolCapability(PlanConfirm),
    ToolCapability(PlanClear),
];

static WORKFLOWS: &[PromptWorkflowSpec] = &[
    PromptWorkflowSpec {
        id: "search-then-inspect",
        tag: "search-then-inspect",
        requires: WorkflowRequirement::All(SEARCH_FACTS),
        exclusive_group: None,
        priority: 100,
        render: render_search_then_inspect,
    },
    PromptWorkflowSpec {
        id: "anchored-edit",
        tag: "anchored-edit",
        requires: WorkflowRequirement::All(EDIT_FACTS),
        exclusive_group: None,
        priority: 100,
        render: render_anchored_edit,
    },
    PromptWorkflowSpec {
        id: "specialized-provider-routing",
        tag: "specialized-provider-routing",
        requires: WorkflowRequirement::Any(ROUTING_FACTS),
        exclusive_group: None,
        priority: 100,
        render: render_specialized_routing,
    },
    PromptWorkflowSpec {
        id: "specialized-mutation-routing",
        tag: "specialized-mutation-routing",
        requires: WorkflowRequirement::AllWithAny {
            all: MUTATION_ROUTING_ALL,
            any: MUTATION_ROUTING_ANY,
        },
        exclusive_group: None,
        priority: 100,
        render: render_specialized_mutation_routing,
    },
    PromptWorkflowSpec {
        id: "python-execution-routing",
        tag: "python-execution-routing",
        requires: WorkflowRequirement::Any(PYTHON_FACTS),
        exclusive_group: None,
        priority: 100,
        render: render_python_routing,
    },
    PromptWorkflowSpec {
        id: "plan-lifecycle",
        tag: "plan-lifecycle",
        requires: WorkflowRequirement::All(PLAN_ALL),
        exclusive_group: None,
        priority: 100,
        render: render_plan_lifecycle,
    },
];

static BUILTIN: LazyLock<PromptWorkflowResolver> =
    LazyLock::new(|| PromptWorkflowResolver { specs: WORKFLOWS });

impl PromptWorkflowResolver {
    pub fn builtin() -> &'static Self {
        &BUILTIN
    }

    pub fn resolve(&self, tools: &ResolvedToolCapabilities) -> Result<ResolvedPromptWorkflows> {
        self.validate()?;
        let mut facts = facts_from_capabilities(tools);
        let mut active = BTreeSet::new();
        let mut claimed_exclusive_groups = BTreeSet::new();
        loop {
            let eligible: Vec<(usize, &PromptWorkflowSpec)> = self
                .specs
                .iter()
                .enumerate()
                .filter(|(_, spec)| !active.contains(spec.id))
                .filter(|(_, spec)| {
                    spec.exclusive_group
                        .is_none_or(|group| !claimed_exclusive_groups.contains(group))
                })
                .filter(|(_, spec)| spec.requires.satisfied_by(&facts))
                .collect();
            if eligible.is_empty() {
                break;
            }
            let mut grouped: BTreeMap<Option<&str>, Vec<(usize, &PromptWorkflowSpec)>> =
                BTreeMap::new();
            for candidate in eligible {
                grouped
                    .entry(candidate.1.exclusive_group)
                    .or_default()
                    .push(candidate);
            }
            let mut winners = Vec::new();
            for (group, candidates) in grouped {
                if group.is_none() {
                    winners.extend(candidates);
                } else {
                    let winner = candidates
                        .into_iter()
                        .min_by_key(|(index, spec)| (std::cmp::Reverse(spec.priority), *index))
                        .expect("exclusive group was nonempty");
                    winners.push(winner);
                }
            }
            winners.sort_by_key(|(index, _)| *index);
            let mut changed = false;
            for (_, spec) in winners {
                if active.insert(spec.id) {
                    if let Some(group) = spec.exclusive_group {
                        claimed_exclusive_groups.insert(group);
                    }
                    facts.insert(PromptFact::Workflow(spec.id));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let ordered = self
            .specs
            .iter()
            .filter(|spec| active.contains(spec.id))
            .collect::<Vec<_>>();
        let fingerprint = workflow_fingerprint(&ordered, &facts);
        Ok(ResolvedPromptWorkflows {
            ordered,
            facts,
            fingerprint,
        })
    }

    fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        let mut tags = BTreeSet::new();
        for spec in self.specs {
            ensure!(ids.insert(spec.id), "duplicate workflow id '{}'", spec.id);
            ensure!(
                tags.insert(spec.tag),
                "duplicate workflow tag '{}'",
                spec.tag
            );
            match spec.requires {
                WorkflowRequirement::All(facts) | WorkflowRequirement::Any(facts) => {
                    ensure!(!facts.is_empty(), "workflow requirement cannot be empty");
                }
                WorkflowRequirement::AllWithAny { all, any } => {
                    ensure!(!all.is_empty() && !any.is_empty());
                }
                WorkflowRequirement::Fact(_) => {}
            }
        }
        for spec in self.specs {
            for dependency in spec.requires.workflow_dependencies() {
                ensure!(
                    ids.contains(dependency),
                    "unknown upstream workflow '{dependency}'"
                );
                ensure!(
                    dependency != spec.id,
                    "workflow '{}' depends on itself",
                    spec.id
                );
            }
        }
        let graph: BTreeMap<_, Vec<_>> = self
            .specs
            .iter()
            .map(|spec| {
                (
                    spec.id,
                    spec.requires.workflow_dependencies().collect::<Vec<_>>(),
                )
            })
            .collect();
        fn visit(
            node: &'static str,
            graph: &BTreeMap<&'static str, Vec<&'static str>>,
            visiting: &mut BTreeSet<&'static str>,
            visited: &mut BTreeSet<&'static str>,
        ) -> Result<()> {
            if visited.contains(node) {
                return Ok(());
            }
            ensure!(
                visiting.insert(node),
                "cycle in prompt workflow dependencies at '{node}'"
            );
            for dependency in graph.get(node).into_iter().flatten() {
                visit(dependency, graph, visiting, visited)?;
            }
            visiting.remove(node);
            visited.insert(node);
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for node in graph.keys().copied() {
            visit(node, &graph, &mut visiting, &mut visited)?;
        }
        Ok(())
    }
}

impl ResolvedPromptWorkflows {
    pub fn has(&self, workflow_id: &str) -> bool {
        self.ordered.iter().any(|spec| spec.id == workflow_id)
    }

    pub fn has_fact(&self, fact: PromptFact) -> bool {
        self.facts.contains(&fact)
    }

    pub fn ordered(&self) -> &[&'static PromptWorkflowSpec] {
        &self.ordered
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

fn facts_from_capabilities(tools: &ResolvedToolCapabilities) -> BTreeSet<PromptFact> {
    let mut facts = BTreeSet::new();
    for (capability, binding) in tools.iter() {
        facts.insert(ToolCapability(*capability));
        if binding.primary.tier == ProviderTier::Specialized
            && binding
                .alternatives
                .iter()
                .any(|provider| provider.tier == ProviderTier::Fallback)
        {
            facts.insert(SpecializedWithFallback(*capability));
        }
    }
    facts
}

fn render_search_then_inspect(
    _: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let search = tools.primary_provider(ContentSearch).unwrap();
    let read = tools.primary_provider(PathRead).unwrap();
    Ok(RenderedPromptPack {
        content: include_str!("../assets/prompts/workflows/search_then_inspect.md")
            .replace("{{SEARCH_PROVIDER}}", search.tool)
            .replace("{{READ_PROVIDER}}", read.tool)
            .trim()
            .into(),
        referenced_tools: [search.tool, read.tool].into_iter().collect(),
        consumed_facts: SEARCH_FACTS.iter().copied().collect(),
    })
}

fn render_anchored_edit(
    _: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let read = tools.primary_provider(EditableSnapshotRead).unwrap();
    let edit = tools.primary_provider(AnchoredEdit).unwrap();
    let mut content = include_str!("../assets/prompts/workflows/anchored_edit.md")
        .replace("{{SNAPSHOT_PROVIDER}}", read.tool)
        .replace("{{EDIT_PROVIDER}}", edit.tool);
    let mut referenced_tools: BTreeSet<_> = [read.tool, edit.tool].into_iter().collect();
    let mut consumed_facts: BTreeSet<_> = EDIT_FACTS.iter().copied().collect();
    if let Some(write) = tools.primary_provider(FileOverwrite) {
        content.push_str(&format!(
            "\nA successful {} overwrite invalidates older snapshot headers for that file.",
            write.tool
        ));
        referenced_tools.insert(write.tool);
        consumed_facts.insert(ToolCapability(FileOverwrite));
    }
    Ok(RenderedPromptPack {
        content: content.trim().into(),
        referenced_tools,
        consumed_facts,
    })
}

fn render_specialized_routing(
    _: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let mut lines = Vec::new();
    let mut referenced_tools = BTreeSet::new();
    let mut consumed_facts = BTreeSet::new();
    for (capability, label) in [
        (PathRead, "path reading"),
        (ContentSearch, "content search"),
        (PathDiscovery, "path discovery"),
    ] {
        let Some(binding) = tools.binding(capability) else {
            continue;
        };
        let fallbacks: Vec<_> = binding
            .alternatives
            .iter()
            .filter(|provider| provider.tier == ProviderTier::Fallback)
            .collect();
        if binding.primary.tier != ProviderTier::Specialized || fallbacks.is_empty() {
            continue;
        }
        referenced_tools.insert(binding.primary.tool);
        let names = fallbacks
            .iter()
            .map(|provider| {
                referenced_tools.insert(provider.tool);
                provider.tool
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "- For {label}, prefer {}. Use {names} only when shell semantics are genuinely required.",
            binding.primary.tool
        ));
        consumed_facts.insert(SpecializedWithFallback(capability));
    }
    ensure!(
        !lines.is_empty(),
        "routing workflow activated without a routable binding"
    );
    Ok(RenderedPromptPack {
        content: format!(
            "{}\n{}",
            include_str!("../assets/prompts/workflows/specialized_provider_routing.md").trim(),
            lines.join("\n")
        ),
        referenced_tools,
        consumed_facts,
    })
}

fn render_specialized_mutation_routing(
    _: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let shell = tools.primary_provider(ShellExec).unwrap();
    let mut lines = Vec::new();
    let mut referenced_tools = BTreeSet::from([shell.tool]);
    let mut consumed_facts = BTreeSet::from([ToolCapability(ShellExec)]);

    if let Some(create) = tools.primary_provider(FileCreate)
        && create.tool != shell.tool
    {
        lines.push(format!(
            "- For file creation, use {} rather than {} redirection or heredocs.",
            create.tool, shell.tool
        ));
        referenced_tools.insert(create.tool);
        consumed_facts.insert(ToolCapability(FileCreate));
    }
    if let Some(overwrite) = tools.primary_provider(FileOverwrite)
        && overwrite.tool != shell.tool
    {
        lines.push(format!(
            "- For full-file replacement, use {} rather than {} redirection or heredocs.",
            overwrite.tool, shell.tool
        ));
        referenced_tools.insert(overwrite.tool);
        consumed_facts.insert(ToolCapability(FileOverwrite));
    }
    if let Some(edit) = tools.primary_provider(AnchoredEdit)
        && edit.tool != shell.tool
    {
        lines.push(format!(
            "- For changes to existing file content, use {} with its anchored protocol rather than mutation commands through {} such as sed or awk.",
            edit.tool, shell.tool
        ));
        referenced_tools.insert(edit.tool);
        consumed_facts.insert(ToolCapability(AnchoredEdit));
    }
    ensure!(
        !lines.is_empty(),
        "mutation routing workflow activated without distinct specialized providers"
    );
    Ok(RenderedPromptPack {
        content: format!(
            "{}\n{}",
            include_str!("../assets/prompts/workflows/specialized_mutation_routing.md").trim(),
            lines.join("\n")
        ),
        referenced_tools,
        consumed_facts,
    })
}

fn render_python_routing(
    _: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let mut providers = Vec::new();
    let mut referenced_tools = BTreeSet::new();
    let mut consumed_facts = BTreeSet::new();
    for (capability, label) in [
        (HostPythonExec, "host execution"),
        (SandboxedPythonExec, "sandboxed execution"),
    ] {
        if let Some(provider) = tools.primary_provider(capability) {
            providers.push(format!("- {} provides {label}.", provider.tool));
            referenced_tools.insert(provider.tool);
            consumed_facts.insert(ToolCapability(capability));
        }
    }
    Ok(RenderedPromptPack {
        content: format!(
            "{}\n{}",
            include_str!("../assets/prompts/workflows/python_execution_routing.md").trim(),
            providers.join("\n")
        ),
        referenced_tools,
        consumed_facts,
    })
}

fn render_plan_lifecycle(
    _context: &PromptBuildContext,
    tools: &ResolvedToolCapabilities,
) -> Result<RenderedPromptPack> {
    let draft = tools.primary_provider(PlanDraft).unwrap();
    let confirm = tools.primary_provider(PlanConfirm).unwrap();
    let clear = tools.primary_provider(PlanClear).unwrap();
    let mut referenced_tools: BTreeSet<_> =
        [draft.tool, confirm.tool, clear.tool].into_iter().collect();
    let mut consumed_facts: BTreeSet<_> = [
        ToolCapability(PlanDraft),
        ToolCapability(PlanConfirm),
        ToolCapability(PlanClear),
    ]
    .into_iter()
    .collect();
    let todo = tools.primary_provider(TodoState).map(|provider| {
        referenced_tools.insert(provider.tool);
        consumed_facts.insert(ToolCapability(TodoState));
        format!(
            "\nAfter confirmation, use {} to track execution progress.",
            provider.tool
        )
    });
    let content = include_str!("../assets/prompts/workflows/plan_lifecycle.md")
        .replace("{{DRAFT_PROVIDER}}", draft.tool)
        .replace("{{CONFIRM_PROVIDER}}", confirm.tool)
        .replace("{{CLEAR_PROVIDER}}", clear.tool);
    Ok(RenderedPromptPack {
        content: format!("{}{}", content.trim(), todo.unwrap_or_default()),
        referenced_tools,
        consumed_facts,
    })
}

fn workflow_fingerprint(ordered: &[&PromptWorkflowSpec], facts: &BTreeSet<PromptFact>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mink-prompt-workflows-v1\0");
    for spec in ordered {
        hasher.update(spec.id.as_bytes());
        hasher.update(b"\0");
    }
    for fact in facts {
        hasher.update(format!("{fact:?}\0").as_bytes());
    }
    crate::util::hex_lower(hasher.finalize())
}

pub(super) fn workflow_section_ids() -> impl Iterator<Item = &'static str> {
    WORKFLOWS.iter().map(|spec| spec.id)
}
