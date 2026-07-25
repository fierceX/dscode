use super::workflows::{PromptFact, ResolvedPromptWorkflows};
use crate::tools::surface::ModelToolSurface;
use anyhow::{Result, bail, ensure};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSectionOrigin {
    Core,
    Tool,
    Workflow,
    External,
    ExternalOverride,
    SessionState,
}

#[derive(Debug, Clone)]
pub struct PromptSection {
    pub id: String,
    pub tag: String,
    pub origin: PromptSectionOrigin,
    pub content: String,
    pub name: Option<String>,
    pub referenced_tools: BTreeSet<&'static str>,
    pub consumed_facts: BTreeSet<PromptFact>,
}

#[derive(Debug, Default)]
pub struct PromptDocument {
    sections: Vec<PromptSection>,
}

impl PromptDocument {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, section: PromptSection) -> Result<()> {
        ensure!(
            !self
                .sections
                .iter()
                .any(|existing| existing.id == section.id),
            "duplicate prompt section id '{}'",
            section.id
        );
        match section.origin {
            PromptSectionOrigin::Core => ensure!(
                section.referenced_tools.is_empty() && section.consumed_facts.is_empty(),
                "core prompt section '{}' cannot reference tools or workflow facts",
                section.id
            ),
            PromptSectionOrigin::Tool => ensure!(
                section.referenced_tools.len() == 1 && section.consumed_facts.is_empty(),
                "tool prompt section '{}' must reference exactly its active tool",
                section.id
            ),
            _ => {}
        }
        if section.content.trim().is_empty() {
            return Ok(());
        }
        self.sections.push(section);
        Ok(())
    }

    pub fn replace_core_with_external(&mut self, id: &str, content: String) -> Result<()> {
        let Some(section) = self.sections.iter_mut().find(|section| section.id == id) else {
            bail!("MISSION cannot replace missing core section '{id}'");
        };
        ensure!(
            section.origin == PromptSectionOrigin::Core,
            "MISSION section '{id}' is reserved by the runtime"
        );
        section.content = content;
        section.origin = PromptSectionOrigin::ExternalOverride;
        section.referenced_tools.clear();
        section.consumed_facts.clear();
        Ok(())
    }

    pub fn sections(&self) -> &[PromptSection] {
        &self.sections
    }

    pub fn has_section(&self, id: &str) -> bool {
        self.sections.iter().any(|section| section.id == id)
    }

    pub fn move_to_end(&mut self, id: &str) -> Result<()> {
        let Some(index) = self.sections.iter().position(|section| section.id == id) else {
            bail!("missing prompt section '{id}'");
        };
        let section = self.sections.remove(index);
        self.sections.push(section);
        Ok(())
    }

    pub fn generated_sections(&self) -> impl Iterator<Item = &PromptSection> {
        self.sections.iter().filter(|section| {
            matches!(
                section.origin,
                PromptSectionOrigin::Core
                    | PromptSectionOrigin::Tool
                    | PromptSectionOrigin::Workflow
            )
        })
    }

    pub fn validate_generated_references(
        &self,
        surface: &ModelToolSurface,
        workflows: &ResolvedPromptWorkflows,
    ) -> Result<()> {
        for section in self.generated_sections() {
            for name in &section.referenced_tools {
                ensure!(
                    surface.has(name),
                    "prompt section '{}' referenced inactive tool '{}'",
                    section.id,
                    name
                );
            }
            for fact in &section.consumed_facts {
                ensure!(
                    workflows.has_fact(*fact),
                    "prompt section '{}' consumed unresolved fact '{fact:?}'",
                    section.id
                );
            }
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        self.sections
            .iter()
            .map(render_section)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_section(section: &PromptSection) -> String {
    let tag = &section.tag;
    match &section.name {
        Some(name) => format!(
            "<{tag} name=\"{}\">\n{}\n</{tag}>",
            escape_attr(name),
            section.content
        ),
        None => format!("<{tag}>\n{}\n</{tag}>", section.content),
    }
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
