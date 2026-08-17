//! Shared local-command help text and skill listing for REPL and TUI.
//!
//! Both terminal modes implement the same slash-command vocabulary and skill
//! report; keeping the copy here prevents the two surfaces drifting apart.

use anyhow::Result;

pub(crate) const COMMON_COMMAND_HELP: &[&str] = &[
    "Commands:",
    "  /flash          Switch to flash alias",
    "  /pro            Switch to pro alias",
    "  /model NAME     Switch to a model name or alias",
    "  /compact        Force context compaction",
    "  /skills         List available skills",
    "  /help           Show this help",
];

pub(crate) const REPL_EXIT_HELP: &[&str] = &[
    "  Ctrl+C          Interrupt current task",
    "  Ctrl+C again    Exit REPL",
    "  exit / quit     Exit REPL",
];

#[cfg(feature = "tui")]
pub(crate) const TUI_EXTRA_HELP: &[&str] = &[
    "  /plan           Open current plan detail",
    "  /todos          Open current todo detail",
    "  /artifact ID    Open a bounded artifact preview",
    "  /sub-agent ID   Open sub-agent detail",
    "  Ctrl+C          Interrupt current task",
    "  Ctrl+C again    Exit TUI",
    "  Esc             Exit TUI",
    "  /exit  /quit    Exit TUI",
];

pub(crate) const SKILL_LOAD_HINT: &str = "Use --skill NAME or Read skill://NAME to load.";

/// Load the model-discoverable skill list using the CLI home resolution.
pub(crate) fn discoverable_skill_lines() -> Result<Vec<String>> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = crate::config::default_home();
    let snapshot = crate::capabilities::CapabilitySnapshot::load_default(&cwd, &home, &[])?;
    Ok(snapshot
        .skills
        .discoverable
        .iter()
        .map(|skill| {
            format!(
                "  {} [{}] - {}",
                skill.skill.name,
                skill.source_label(),
                skill.skill.description
            )
        })
        .collect())
}

pub(crate) fn print_skills() {
    println!("SKILLS");
    println!("{}", "-".repeat(60));
    match discoverable_skill_lines() {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
        Err(error) => println!("Error loading skills: {error}"),
    }
    println!("{SKILL_LOAD_HINT}");
}
