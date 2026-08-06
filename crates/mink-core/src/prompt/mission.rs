use super::{PromptDocument, PromptSection, PromptSectionOrigin};
use anyhow::{Result, bail};
use std::collections::BTreeSet;

const CORE_OVERRIDE_ALLOWLIST: &[&str] = &[
    "system-conventions",
    "agent-identity",
    "environment",
    "execution-codes",
    "belief-awareness",
    "output-language",
];

pub(super) fn apply(
    document: &mut PromptDocument,
    content: &str,
    runtime_reserved: BTreeSet<&'static str>,
) -> Result<()> {
    for (heading, body) in level_one_sections(content) {
        if CORE_OVERRIDE_ALLOWLIST.contains(&heading.as_str()) {
            document.replace_core_with_external(&heading, body)?;
            continue;
        }
        if runtime_reserved.contains(heading.as_str()) || document.has_section(&heading) {
            bail!("MISSION section '{heading}' is reserved by the runtime");
        }
        document.push(PromptSection {
            id: format!("mission:{heading}"),
            tag: heading,
            origin: PromptSectionOrigin::External,
            content: body,
            name: None,
            referenced_tools: BTreeSet::new(),
            consumed_facts: BTreeSet::new(),
        })?;
    }
    Ok(())
}

fn level_one_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut heading: Option<String> = None;
    let mut body = Vec::new();
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("# ")
            && !value.trim().is_empty()
        {
            if let Some(previous) = heading.take() {
                let content = body.join("\n").trim().to_string();
                if !content.is_empty() {
                    sections.push((previous, content));
                }
            }
            heading = Some(value.trim().to_string());
            body.clear();
        } else if heading.is_some() {
            body.push(line);
        }
    }
    if let Some(previous) = heading {
        let content = body.join("\n").trim().to_string();
        if !content.is_empty() {
            sections.push((previous, content));
        }
    }
    sections
}
