use super::{PromptDocument, PromptSection, PromptSectionOrigin};
use crate::tools::surface::ModelToolSurface;
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

struct ToolPromptSpec {
    tool: &'static str,
    id: &'static str,
    tag: &'static str,
    content: &'static str,
}

static TOOL_PROMPTS: &[ToolPromptSpec] = &[
    ToolPromptSpec {
        tool: "SubAgent",
        id: "sub-agent-guidance",
        tag: "sub-agent-guidance",
        content: include_str!("../assets/prompts/tools/sub_agent.md"),
    },
    ToolPromptSpec {
        tool: "TodoWrite",
        id: "todo-guidance",
        tag: "todo-guidance",
        content: include_str!("../assets/prompts/tools/todo_write.md"),
    },
];

pub(super) fn append_tool_sections(
    document: &mut PromptDocument,
    surface: &ModelToolSurface,
) -> Result<()> {
    for spec in TOOL_PROMPTS {
        if !surface.has(spec.tool) {
            continue;
        }
        let referenced_tools = [spec.tool].into_iter().collect();
        ensure!(surface.has(spec.tool));
        document.push(PromptSection {
            id: spec.id.into(),
            tag: spec.tag.into(),
            origin: PromptSectionOrigin::Tool,
            content: spec.content.trim().into(),
            name: None,
            referenced_tools,
            consumed_facts: BTreeSet::new(),
        })?;
    }
    Ok(())
}

pub(super) fn reserved_section_ids() -> BTreeSet<&'static str> {
    TOOL_PROMPTS
        .iter()
        .map(|spec| spec.id)
        .chain(super::workflows::workflow_section_ids())
        .chain([
            "runtime-capabilities",
            "rules",
            "instruction-files",
            "rule-index",
            "skill-index",
            "selected-skills",
            "current-plan",
        ])
        .collect()
}
