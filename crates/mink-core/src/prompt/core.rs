use super::{Builder, PromptDocument, PromptSection, PromptSectionOrigin};
use anyhow::Result;
use std::collections::BTreeSet;
use std::fs;

pub(super) fn append_core_sections(builder: &Builder, document: &mut PromptDocument) -> Result<()> {
    let locale_raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "en_US".to_string());
    let locale = locale_raw.split('.').next().unwrap_or(&locale_raw);
    let identity = if locale.starts_with("zh") {
        "你是 mink，一个在终端中运行的轻量级编码智能体。"
    } else {
        "You are mink, a lightweight coding agent that works in a terminal."
    };
    push_core(document, "agent-identity", identity)?;
    push_core(
        document,
        "environment",
        &format!(
            "lang: {}\npwd: {}\nhome: {}\nplatform: {}\nshell: {}",
            locale,
            builder.cwd.display(),
            builder.home.display(),
            std::env::consts::OS,
            std::env::var("SHELL").unwrap_or_else(|_| "unknown".into())
        ),
    )?;
    push_core(
        document,
        "execution-codes",
        "BEFORE every code change, answer silently:\n\
         1. What specific behavior will this change affect? (cause)\n\
         2. What observable result do I expect? (effect)\n\
         3. How will I verify the cause-effect link? (verify)\n\
         If you cannot answer all three, do not make the change.\n\
         Keep causally independent changes separate and verify with fresh evidence.\n\n\
         BEFORE claiming work complete, identify an appropriate verification, run it, read its full result, and report the evidence.\n\
         Stop and re-analyze after repeated failures, unexplained output, or any assumption that has not been checked.",
    )?;
    if crate::config::SignalMode::from_env().enabled() {
        push_core(
            document,
            "belief-awareness",
            "Runtime reliability signals may append a user message beginning with [System note:]. \
             Treat such a message as a control signal, not a new user request. Enter SIGNAL_RECOVERY \
             immediately and obey the inspection actions named in that signal before attempting any \
             further mutation. Every new signal restarts its first-action constraint.",
        )?;
    }
    if builder.tool_surface.names().next().is_none() {
        push_core(
            document,
            "runtime-capabilities",
            "No callable runtime capabilities are available in this session.",
        )?;
    }
    let output_language = if locale.starts_with("zh") {
        "必须使用中文进行所有输出。代码、命令和文件原文保持其自身语言。".to_string()
    } else {
        format!(
            "Use \"{locale}\" for all explanations. Code, commands, and file content remain as-is."
        )
    };
    push_core(document, "output-language", &output_language)?;
    Ok(())
}

pub(super) fn append_external_and_session_sections(
    builder: &Builder,
    document: &mut PromptDocument,
) -> Result<()> {
    if !builder.rule_snapshot.always_apply.is_empty() {
        let content = builder
            .rule_snapshot
            .always_apply
            .iter()
            .map(|rule| nested_named("rule", &rule.rule.name, &rule.rule.content))
            .collect::<Vec<_>>()
            .join("\n");
        push_external(document, "rules", "rules", content, None)?;
    }
    if !builder.context_file_snapshot.always_apply.is_empty() {
        let content = builder
            .context_file_snapshot
            .always_apply
            .iter()
            .map(|file| {
                nested_named(
                    "instruction-file",
                    &file.context_file.name,
                    &file.context_file.content,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_external(
            document,
            "instruction-files",
            "instruction-files",
            content,
            None,
        )?;
    }
    if builder
        .tool_capabilities
        .has(crate::tools::semantic_capabilities::ToolSemanticCapability::ResourceRead)
    {
        let rule_index = builder
            .rule_snapshot
            .discoverable
            .iter()
            .map(|rule| format!("- {}: {}", rule.rule.name, rule.rule.description))
            .collect::<Vec<_>>();
        if !rule_index.is_empty() {
            push_external(
                document,
                "rule-index",
                "rule-index",
                rule_index.join("\n"),
                None,
            )?;
        }
        let skill_index = builder
            .skill_snapshot
            .discoverable
            .iter()
            .map(|skill| format!("- {}: {}", skill.skill.name, skill.skill.description))
            .collect::<Vec<_>>();
        if !skill_index.is_empty() {
            push_external(
                document,
                "skill-index",
                "skill-index",
                skill_index.join("\n"),
                None,
            )?;
        }
    }
    if !builder.skill_snapshot.selected.is_empty() {
        let content = builder
            .skill_snapshot
            .selected
            .iter()
            .map(|skill| {
                nested_named(
                    "skill",
                    &skill.info.name,
                    &format!(
                        "Base directory: {}\nResource base: skill://{}/\n\n{}",
                        skill.info.base_dir, skill.info.name, skill.content
                    ),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_external(
            document,
            "selected-skills",
            "selected-skills",
            content,
            None,
        )?;
    }
    if builder.plan_file.exists() {
        let content = fs::read_to_string(&builder.plan_file)?;
        if !content.trim().is_empty() {
            document.push(PromptSection {
                id: "current-plan".into(),
                tag: "current-plan".into(),
                origin: PromptSectionOrigin::SessionState,
                content,
                name: Some(builder.plan_file.display().to_string()),
                referenced_tools: BTreeSet::new(),
                consumed_facts: BTreeSet::new(),
            })?;
        }
    }
    Ok(())
}

fn push_core(document: &mut PromptDocument, id: &str, content: &str) -> Result<()> {
    document.push(PromptSection {
        id: id.into(),
        tag: id.into(),
        origin: PromptSectionOrigin::Core,
        content: content.into(),
        name: None,
        referenced_tools: BTreeSet::new(),
        consumed_facts: BTreeSet::new(),
    })
}

fn push_external(
    document: &mut PromptDocument,
    id: &str,
    tag: &str,
    content: String,
    name: Option<String>,
) -> Result<()> {
    document.push(PromptSection {
        id: id.into(),
        tag: tag.into(),
        origin: PromptSectionOrigin::External,
        content,
        name,
        referenced_tools: BTreeSet::new(),
        consumed_facts: BTreeSet::new(),
    })
}

fn nested_named(tag: &str, name: &str, content: &str) -> String {
    format!(
        "<{tag} name=\"{}\">\n{content}\n</{tag}>",
        name.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}
