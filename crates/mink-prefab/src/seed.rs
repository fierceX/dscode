use crate::template::{PrefabTemplate, render_conversation};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// High-level seed request accepted by direct seeding APIs.
#[derive(Debug, Clone)]
pub struct PrefabSeed {
    pub template: PrefabTemplate,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub agents_md: Option<String>,
    pub skill_result_code: Option<String>,
    pub skill_result_document: Option<String>,
    pub skill_result_list: Option<String>,
    pub system_reminder_agents: Option<String>,
    pub skill_catalog_reminder: Option<String>,
    pub instruction_hint: Option<String>,
    pub full_system_prompt: Option<String>,
}

impl PrefabSeed {
    /// Use the bundled generic template.
    pub fn builtin() -> Result<Self> {
        Ok(Self {
            template: crate::load_builtin()?,
            session_id: None,
            title: None,
            agents_md: None,
            skill_result_code: None,
            skill_result_document: None,
            skill_result_list: None,
            system_reminder_agents: None,
            skill_catalog_reminder: None,
            instruction_hint: None,
            full_system_prompt: None,
        })
    }

    /// Load a bundled template by name (`default`, `router-flash-weak`, ...).
    pub fn named(name: &str) -> Result<Self> {
        Ok(Self {
            template: crate::load_named(name)?,
            session_id: None,
            title: None,
            agents_md: None,
            skill_result_code: None,
            skill_result_document: None,
            skill_result_list: None,
            system_reminder_agents: None,
            skill_catalog_reminder: None,
            instruction_hint: None,
            full_system_prompt: None,
        })
    }

    /// Load a template from a directory containing `meta.json` and
    /// `conversation.jsonl`.
    pub fn from_path(path: &Path) -> Result<Self> {
        Ok(Self {
            template: crate::load_path(path)?,
            session_id: None,
            title: None,
            agents_md: None,
            skill_result_code: None,
            skill_result_document: None,
            skill_result_list: None,
            system_reminder_agents: None,
            skill_catalog_reminder: None,
            instruction_hint: None,
            full_system_prompt: None,
        })
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_agents_md(mut self, agents_md: impl Into<String>) -> Self {
        self.agents_md = Some(agents_md.into());
        self
    }

    pub fn with_skill_result_code(mut self, skill_result_code: impl Into<String>) -> Self {
        self.skill_result_code = Some(skill_result_code.into());
        self
    }

    pub fn with_skill_result_document(mut self, skill_result_document: impl Into<String>) -> Self {
        self.skill_result_document = Some(skill_result_document.into());
        self
    }

    pub fn with_skill_result_list(mut self, skill_result_list: impl Into<String>) -> Self {
        self.skill_result_list = Some(skill_result_list.into());
        self
    }

    pub fn with_system_reminder_agents(mut self, value: impl Into<String>) -> Self {
        self.system_reminder_agents = Some(value.into());
        self
    }

    pub fn with_skill_catalog_reminder(mut self, value: impl Into<String>) -> Self {
        self.skill_catalog_reminder = Some(value.into());
        self
    }

    pub fn with_instruction_hint(mut self, value: impl Into<String>) -> Self {
        self.instruction_hint = Some(value.into());
        self
    }

    pub fn with_full_system_prompt(mut self, value: impl Into<String>) -> Self {
        self.full_system_prompt = Some(value.into());
        self
    }
}

/// Options for seeding a Mink session directory.
#[derive(Debug, Clone)]
pub struct PrefabSeedOptions {
    /// Concrete session id used in `session.json`.
    pub session_id: String,
    /// Optional session title.
    pub title: Option<String>,
    /// Target working directory; replaces `{{CWD}}` in the template.
    pub cwd: PathBuf,
    /// Target workspace AGENTS.md content; replaces `{{AGENTS_MD}}`.
    pub agents_md: Option<String>,
    /// Optional live-rendered skill_search result for query `code`.
    pub skill_result_code: Option<String>,
    /// Optional live-rendered skill_search result for query `document`.
    pub skill_result_document: Option<String>,
    /// Optional live-rendered `skill://list` result for flash-style templates.
    pub skill_result_list: Option<String>,
    /// Optional full `<system-reminder>` user message with AGENTS.md content.
    pub system_reminder_agents: Option<String>,
    /// Optional skill catalog `<system-reminder>` user message.
    pub skill_catalog_reminder: Option<String>,
    /// Optional instruction hint user message.
    pub instruction_hint: Option<String>,
    /// Optional full Mink system prompt injected as virtual tool result.
    pub full_system_prompt: Option<String>,
}

/// Seed a new Mink session directory with the given prefab template.
///
/// This refuses to overwrite an existing session that already has metadata or
/// conversation content.
pub fn seed_session(
    session_dir: &Path,
    template: &PrefabTemplate,
    options: &PrefabSeedOptions,
) -> Result<()> {
    if session_dir.exists() && session_has_data(session_dir)? {
        bail!(
            "refusing to seed existing session directory {}",
            session_dir.display()
        );
    }
    fs::create_dir_all(session_dir)
        .with_context(|| format!("failed to create session dir {}", session_dir.display()))?;

    let replacements = build_replacements(options);
    let conversation = render_conversation(&template.conversation, &replacements);
    let conversation_text = conversation
        .iter()
        .map(|value| {
            serde_json::to_string(value)
                .map(|line| line + "\n")
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?
        .concat();

    let metadata = build_metadata(options);
    let metadata_text = serde_json::to_string_pretty(&metadata)?;

    fs::write(session_dir.join("session.json"), metadata_text).with_context(|| {
        format!(
            "failed to write {}",
            session_dir.join("session.json").display()
        )
    })?;
    fs::write(session_dir.join("conversation.jsonl"), conversation_text).with_context(|| {
        format!(
            "failed to write {}",
            session_dir.join("conversation.jsonl").display()
        )
    })?;
    // Write replayable events so REPL/TUI show the prefilled trajectory as a
    // normal session history. The agent loop will append real events later.
    let events = build_events_from_conversation(&conversation, &template.meta.model);
    let events_text = events
        .iter()
        .map(|value| {
            serde_json::to_string(value)
                .map(|line| line + "\n")
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?
        .concat();
    fs::write(session_dir.join("events.jsonl"), events_text).with_context(|| {
        format!(
            "failed to write {}",
            session_dir.join("events.jsonl").display()
        )
    })?;

    Ok(())
}

fn session_has_data(session_dir: &Path) -> Result<bool> {
    if session_dir.join("session.json").exists() {
        return Ok(true);
    }
    let conversation_path = session_dir.join("conversation.jsonl");
    if conversation_path.exists() {
        let text = fs::read_to_string(&conversation_path)
            .with_context(|| format!("failed to read {}", conversation_path.display()))?;
        if text.lines().any(|line| !line.trim().is_empty()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Restructure an already-initialized Mink session for Prefab.
///
/// Unlike `seed_session`, this does not refuse an existing `session.json`.
/// It only writes template conversation/events when the conversation is empty
/// (a fresh session). Prefix snapshots are stored as standard `prefix_snapshot`
/// events in `events.jsonl` by the runtime, not as extra Prefab files.
pub fn restructure_session(
    session_dir: &Path,
    template: &PrefabTemplate,
    options: &PrefabSeedOptions,
) -> Result<()> {
    fs::create_dir_all(session_dir)
        .with_context(|| format!("failed to create session dir {}", session_dir.display()))?;

    let replacements = build_replacements(options);
    let conversation = render_conversation(&template.conversation, &replacements);

    if !conversation_has_data(session_dir)? {
        let conversation_text = conversation
            .iter()
            .map(|value| {
                serde_json::to_string(value)
                    .map(|line| line + "\n")
                    .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()?
            .concat();
        fs::write(session_dir.join("conversation.jsonl"), conversation_text).with_context(
            || {
                format!(
                    "failed to write {}",
                    session_dir.join("conversation.jsonl").display()
                )
            },
        )?;

        let events = build_events_from_conversation(&conversation, &template.meta.model);
        let events_text = events
            .iter()
            .map(|value| {
                serde_json::to_string(value)
                    .map(|line| line + "\n")
                    .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()?
            .concat();
        let events_path = session_dir.join("events.jsonl");
        let mut events_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
            .with_context(|| format!("failed to open {}", events_path.display()))?;
        events_file
            .write_all(events_text.as_bytes())
            .with_context(|| format!("failed to write {}", events_path.display()))?;
    }

    Ok(())
}

fn conversation_has_data(session_dir: &Path) -> Result<bool> {
    let conversation_path = session_dir.join("conversation.jsonl");
    if !conversation_path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(&conversation_path)
        .with_context(|| format!("failed to read {}", conversation_path.display()))?;
    Ok(text.lines().any(|line| !line.trim().is_empty()))
}

/// Convert Mink conversation lines into the `events.jsonl` replay vocabulary
/// used by REPL/TUI. This lets a seeded prefab session show its warm-up
/// trajectory immediately after entering the UI.
fn build_events_from_conversation(
    conversation: &[serde_json::Value],
    model: &str,
) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    let mut tool_names = std::collections::HashMap::<String, String>::new();
    let mut tool_call_count = 0u32;

    events.push(json!({
        "type":"runtime_turn_started",
        "turn_id":"prefab-turn-1",
    }));
    events.push(json!({
        "type":"turn_start",
        "model": model,
        "model_alias": null,
        "belief": 0.75,
        "forced_model": null,
    }));

    for msg in conversation {
        let role = msg
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let content = msg
            .get("content")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        if role == "user" {
            if let Some(text) = content.as_str() {
                events.push(json!({"type":"user_input","version":null,"content":text}));
            } else if let Some(blocks) = content.as_array() {
                for block in blocks {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                    {
                        let id = block
                            .get("tool_use_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = tool_names.get(&id).cloned().unwrap_or_default();
                        events.push(json!({
                            "type":"tool_result",
                            "version":null,
                            "tool_use_id": id,
                            "name": name,
                            "content": block.get("content").and_then(serde_json::Value::as_str).unwrap_or(""),
                        }));
                    }
                }
            }
        } else if role == "assistant"
            && let Some(blocks) = content.as_array()
        {
            for block in blocks {
                match block.get("type").and_then(serde_json::Value::as_str) {
                        Some("thinking") => events.push(json!({
                            "type":"thinking",
                            "version":null,
                            "content": block.get("thinking").and_then(serde_json::Value::as_str).unwrap_or(""),
                        })),
                        Some("text") => events.push(json!({
                            "type":"text",
                            "version":null,
                            "content": block.get("text").and_then(serde_json::Value::as_str).unwrap_or(""),
                        })),
                        Some("tool_use") => {
                            let id = block.get("id").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
                            let name = block.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
                            let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            tool_names.insert(id.clone(), name.clone());
                            tool_call_count += 1;
                            events.push(json!({
                                "type":"tool_call",
                                "version":null,
                                "name": name,
                                "id": id,
                                "input": input,
                            }));
                        }
                        _ => {}
                    }
            }
        }
    }

    events.push(json!({
        "type":"turn_tracking",
        "version":null,
        "decision":"Stop",
        "tool_call_count": tool_call_count,
        "tool_error_count": 0,
        "belief": 0.75,
        "model": model,
    }));
    events.push(json!({
        "type":"turn_final",
        "billing_turn_id":"prefab-turn-1",
        "status":"ok",
        "tool_call_count": tool_call_count,
        "tool_error_count": 0,
        "elapsed_ms": 0,
        "error": null,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "total_tokens": 0,
            "cost_usd": 0.0,
            "currency": "USD",
        },
    }));

    events
}

fn build_replacements(options: &PrefabSeedOptions) -> Vec<(&'static str, String)> {
    let cwd = options.cwd.display().to_string();
    let agents_md = options
        .agents_md
        .clone()
        .unwrap_or_else(|| {
            "No AGENTS.md instruction file is present; continue without additional file-based instructions.".to_string()
        });
    let skill_result_code = options.skill_result_code.clone().unwrap_or_else(|| {
        "No skills match \"code\". Use skill_search with other keywords.".to_string()
    });
    let skill_result_document = options.skill_result_document.clone().unwrap_or_else(|| {
        "No skills match \"document\". Use skill_search with other keywords.".to_string()
    });
    let skill_result_list = options.skill_result_list.clone().unwrap_or_else(|| {
        "# Skills\n- debugging: Use when encountering any bug, test failure, or unexpected behavior, before proposing fixes\n".to_string()
    });
    let system_reminder_agents = options
        .system_reminder_agents
        .clone()
        .unwrap_or_else(|| {
            format!(
                "<system-reminder>\nThe following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.\n\nInstructions from: workspace AGENTS.md\n\n{agents_md}\n</system-reminder>"
            )
        });
    let skill_catalog_reminder = options
        .skill_catalog_reminder
        .clone()
        .unwrap_or_else(|| {
            format!(
                "<system-reminder>\nA skill is a reusable set of task-specific instructions. The following skills are available in this session:\n\n<available_skills>\n{skill_result_list}</available_skills>\n</system-reminder>"
            )
        });
    let instruction_hint = options.instruction_hint.clone().unwrap_or_else(|| {
        format!(
            "Workspace instruction files exist: AGENTS.md (project root: {cwd}). Do NOT assume their content. When a task touches this workspace, read the relevant instruction files first and follow them."
        )
    });
    let full_system_prompt = options
        .full_system_prompt
        .clone()
        .unwrap_or_else(|| "Full instructions loaded.".to_string());
    vec![
        ("{{CWD}}", cwd),
        ("{{AGENTS_MD}}", agents_md),
        ("{{SYSTEM_REMINDER_AGENTS}}", system_reminder_agents),
        ("{{SKILL_CATALOG_REMINDER}}", skill_catalog_reminder),
        ("{{INSTRUCTION_HINT}}", instruction_hint),
        ("{{FULL_SYSTEM_PROMPT}}", full_system_prompt),
        ("{{SKILL_RESULT_CODE}}", skill_result_code),
        ("{{SKILL_RESULT_DOCUMENT}}", skill_result_document),
        ("{{SKILL_RESULT_LIST}}", skill_result_list),
    ]
}

fn build_metadata(options: &PrefabSeedOptions) -> serde_json::Value {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
    let mut metadata = json!({
        "id": options.session_id,
        "created_at": now,
        "updated_at": now,
        "cwd": options.cwd.display().to_string(),
    });
    if let Some(title) = &options.title {
        metadata["title"] = json!(title);
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::load_builtin;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn seed_writes_mink_compatible_session() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mink-prefab-test-{unique}"));
        let template = load_builtin().unwrap();
        let options = PrefabSeedOptions {
            session_id: "prefab-test".to_string(),
            title: Some("Prefab Test".to_string()),
            cwd: dir.clone(),
            agents_md: Some("# Test AGENTS\n".to_string()),
            skill_result_code: None,
            skill_result_document: None,
            skill_result_list: None,
            system_reminder_agents: None,
            skill_catalog_reminder: None,
            instruction_hint: None,
            full_system_prompt: Some("Full instructions loaded.".to_string()),
        };
        seed_session(&dir, &template, &options).unwrap();

        let session_json = fs::read_to_string(dir.join("session.json")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&session_json).unwrap();
        assert_eq!(meta["id"], "prefab-test");
        assert_eq!(meta["cwd"], dir.display().to_string());

        let conv = fs::read_to_string(dir.join("conversation.jsonl")).unwrap();
        assert!(conv.contains("# Test AGENTS"));
        assert!(conv.contains("Instructions loaded."));
        assert!(conv.contains("Ready."));
        let events_text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(events_text.contains("\"type\":\"user_input\""));
        assert!(events_text.contains("\"type\":\"tool_call\""));
        assert!(events_text.contains("\"type\":\"tool_result\""));
        // Direct seeding must keep the same session layout as a normal session.
        assert!(!dir.join("prefab-prefix.json").exists());
        assert!(!dir.join("prefab-phases.json").exists());

        // Refuse to overwrite.
        let err = seed_session(&dir, &template, &options).unwrap_err();
        assert!(err.to_string().contains("refusing to seed"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
