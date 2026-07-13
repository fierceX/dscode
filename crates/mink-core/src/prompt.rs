use crate::capabilities::{ContextFileSnapshot, RuleSnapshot, SkillSnapshot};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Builder {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub skill_snapshot: Arc<SkillSnapshot>,
    pub context_file_snapshot: Arc<ContextFileSnapshot>,
    pub rule_snapshot: Arc<RuleSnapshot>,
    pub summary_file: PathBuf,
    pub plan_file: PathBuf,
    pub plan_draft_file: PathBuf,
    pub mission_file: Option<PathBuf>,
    pub mission_content: Option<String>,
}

impl Builder {
    pub fn build_system_prompt(&self) -> Result<String> {
        let mut sections = Vec::new();
        let locale_raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "en_US".to_string());
        // Strip encoding suffix (e.g., zh_CN.UTF-8 -> zh_CN)
        let locale = locale_raw
            .split('.')
            .next()
            .unwrap_or(&locale_raw)
            .to_string();
        let identity = if locale.starts_with("zh") {
            "你是 mink，一个在终端中运行的轻量级编码智能体。".to_string()
        } else {
            "You are mink, a lightweight coding agent that works in a terminal.".to_string()
        };
        sections.push(wrap_section("agent-identity", &identity, None));
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
        let platform = std::env::consts::OS;
        let environment = format!(
            "lang: {}\npwd: {}\nhome: {}\nplatform: {}\nshell: {}",
            locale,
            self.cwd.display(),
            self.home.display(),
            platform,
            shell
        );
        sections.push(wrap_section("environment", &environment, None));
        if let Some(rules) = self.build_rules_section()? {
            sections.push(wrap_section("rules", &rules, None));
        }
        // Execution codes: cause-effect-verify, verification gate, stop triggers
        sections.push(wrap_section(
            "execution-codes",
            "BEFORE every code change, answer silently:\n\
             1. What specific behavior will this change affect? (cause)\n\
             2. What observable result do I expect? (effect)\n\
             3. How will I verify the cause-effect link? (verify)\n\
             If you cannot answer all three, DO NOT make the change.\n\
             One change at a time — multiple changes confound causality.\n\
             Verify immediately after each change to confirm cause-effect.\n\
             \n\
             BEFORE claiming any work is complete, fixed, or passing:\n\
             1. IDENTIFY the verification command (test suite, build, lint, etc.)\n\
             2. RUN it fresh and capture the FULL output\n\
             3. READ the output — check exit code, count failures, read error lines\n\
             4. STATE the result WITH evidence: pass/fail, how many tests, exit code\n\
             If you skip any of these steps, you are NOT verifying — you are guessing.\n\
             Guessing is lying.\n\
             \n\
             STOP and re-analyze when ANY of these triggers fire:\n\
             - 3 consecutive tool calls returned Error:\n\
             - You're about to make multiple changes before testing\n\
             - A build or test failed but you skipped reading full output\n\
             - You're thinking \"should work now\" without fresh verification\n\
             - You skipped TodoWrite because \"the task is simple\"\n\
             Any trigger hit means: STOP. Analyze root cause before acting.",
            None,
        ));

        if crate::agent::signal_mode::SignalMode::from_env().enabled() {
            // Belief awareness — static protocol for runtime signal injection.
            let belief_awareness = "This agent has a belief tracking system that monitors tool execution quality.\n\
                 - Each tool call is evaluated for errors (compile failures, test failures, edit loops)\n\
                 - A \"belief score\" (0.0–1.0) reflects recent tool execution reliability\n\
                 - When belief drops below 0.70, a [System note] may be appended as a user message\n\
                 - When belief drops below 0.30, the task may be aborted\n\n\
                 SIGNAL_RECOVERY mode:\n\
                 - A user message that begins with [System note: is a runtime control signal, not a new user request and not ordinary conversation.\n\
                 - Treat it as higher priority than your current repair momentum. It means recent tool outcomes show your current approach is unreliable.\n\
                 - Enter SIGNAL_RECOVERY mode immediately after any [System note: ...] message.\n\n\
                 While in SIGNAL_RECOVERY mode, your next assistant turn MUST obey these constraints:\n\
                 1. The FIRST tool call after the signal MUST inspect current state with Read, Grep, or Glob. Use Bash only for a focused verification/state command such as build, test, or git status when shell semantics are required\n\
                 2. The FIRST tool call after the signal MUST NOT be Edit or Write, even if you think you already know the fix\n\
                 - Calling Edit or Write as the next tool action after [System note: ...] is a violation\n\
                 - Treating the signal as stale because an earlier Read already happened is a violation; each new signal restarts the first-tool rule\n\n\
                 Repeated signals:\n\
                 - The FIRST tool after each repeated signal must again inspect current state with Read, Grep, Glob, or a focused Bash verification/state command\n\
                 - Do not treat repeated signals as noise\n\
                 \n\
                 Common rationalizations (ALL are WRONG):\n\
                 | Rationalization | Reality |\n\
                 |--------|---------|\n\
                 | \"One-file change, don't need Grep\" | Without searching you WILL miss call sites. Use Grep. |\n\
                 | \"Just two lines, skip the tests\" | Two lines can break the build chain. Run the tests. |\n\
                 | \"Non-zero exit but it's just a warning\" | Non-zero IS failure. Read the full output. |\n\
                 | \"I'll change 3 files at once, compile together\" | You can't isolate what broke it. One change, verify, next. |\n\
                 | \"Task is simple, skip TodoWrite\" | Simple tasks have order too. Write the checklist. |";
            sections.push(wrap_section("belief-awareness", belief_awareness, None));
        }

        // Tool usage — priority order and anchored edit protocol
        sections.push(wrap_section(
            "tool-usage",
            "TOOL PRIORITY (MUST follow this order — tools above Bash take precedence):\n\
             1. File/directory reads → Read (not cat/head/tail/less/more in Bash)\n\
             2. Content search → Grep (not grep/rg/ag/ack in Bash)\n\
             3. Path discovery → Glob (not find/ls/fd/tree in Bash)\n\
             4. File writes → Write (not shell redirection or heredoc)\n\
             5. Code edits → Edit with patch parameter (not sed/awk)\n\
             6. Skills → Read with `skill://<name>`; skill-owned reference files use `skill://<name>/<relative-path>` (first check skill-index section)\n\
             7. Build/test/git/package-manager → Bash ✓ (shell-only operations)\n\
             8. All other shell needs → Bash ✓ (last resort)\n\
             WARNING: Bash commands matching patterns 1-6 above are DETECTED and REJECTED at runtime.\n\
             \n\
             Read usage:\n\
             - Use Read for a single file or lightweight resource. Call Read multiple times for multiple files.\n\
             - Read accepts line ranges either way:\n\
               - Path selector: `src/lib.rs:40-80` (lines 40-80), `src/lib.rs:40+20` (40-59), `src/lib.rs:raw` (no snapshot header)\n\
               - Separate fields: {\"path\":\"src/lib.rs\",\"offset\":40,\"limit\":40}\n\
             - Use Glob and Grep for one pattern at a time.\n\
             - Grep supports a context parameter to identify the target range. Before editing, call Read on that range to get the @PATH#TAG snapshot header.\n\
             - Use multiple tool calls in one response when they are independent.\n\
             - For skills, use Read with `skill://<name>` (see skill-index for available skills). For files referenced by a skill, use `skill://<name>/<relative-path>` rather than guessing local paths.\n\
             \n\
             EDIT PROTOCOL (anchored patch):\n\
             - Edit only modifies existing files. Use Write to create or fully replace a file.\n\
             - Edit requires the patch parameter. Old string matching (old_string/new_string) is not supported.\n\
             - Every patch starts with @PATH#TAG copied from the latest non-raw Read of that file, or from the fresh @PATH#TAG returned by the previous successful Edit.\n\
             - Every @PATH#TAG is a snapshot of one file state. After any successful Edit or Write to that file, all older tags and line numbers for that file are dead.\n\
             - Before every Edit, you must be grounded in the current file state: use a fresh non-raw Read after the last successful Edit/Write, or use the fresh header and numbered lines returned by the last successful Edit for an immediate follow-up in that shown range.\n\
             - Grep can locate code but cannot authorize Edit. Use Read on the exact target range to get the @PATH#TAG header.\n\
             - Supported ops: replace N..M:, replace N:, delete N..M, delete N, insert before N:, insert after N:, insert head:, insert tail:.\n\
             - Numbers refer to ORIGINAL file lines from the snapshot and do not shift within one patch. Multiple changes in the same file from the same snapshot -> combine into one multi-hunk Edit.patch.\n\
             - Body rows appear only under replace/insert headers, are final content only, and each must be prefixed with '+'. To keep a line, leave it out of every range. Never include '-old' rows or bare context lines.\n\
             - Keep ranges tight: cover only lines whose content actually changes. To change lines 2 and 5 while preserving 3-4, use two hunks, not replace 2..5.\n\
             - Never use Edit for mechanical formatting, import sorting, whitespace cleanup, or reindent-only changes; run the project formatter once after semantic edits.\n\
             - Prefer insert before/after an anchored line. Use insert head/tail only after reading enough of the file to make the file boundary intentional.\n\
             - On snapshot mismatch, unknown tag, uncovered line, no-op, or any result you cannot fully account for: stop editing that file, re-read the suggested target range, then retry with the new header.\n\
             Canonical shape:\n\
             @src/foo.rs#0A3B\n\
             replace 41..41:\n\
             +    return new_value;\n\
             insert after 55:\n\
             +    println!(\"done\");\n\
             Critical: re-ground after every edit; keep ranges tight and in-bounds; body rows are only +FINAL_CONTENT.",
            None,
        ));
        sections.push(wrap_section(
            "sub-agent-guidance",
            "- **When to use**: delegating independent sub-tasks that do NOT need your current conversation context — e.g. investigating a separate file, running a focused search, testing a hypothesis in isolation.\n- **When NOT to use**: tasks that depend on your working context, conversation history, or intermediate state. The child agent starts with a blank slate.\n- **Prompt design**: write a complete, self-contained prompt. Include all file paths, function names, error messages, and constraints the child needs. Assume zero shared context.\n- **Result handling**: when all sub-agents complete, their results are batched and injected together: `[sub-agent <id>] <status> (in=<n>, out=<n>)\nThinking: ...\nText: ...`. All results arrive in a single message — you see them all at once.\n- **Parallelism**: multiple SubAgent calls in one turn run concurrently. Use this to parallelize independent investigations. **IMPORTANT**: ALL results arrive together, not one by one. Wait for the batch before acting. Do NOT re-launch a sub-agent that hasn't returned results yet.\n- **Failure**: if the child fails (status=failed), the result text may be partial or empty. Handle gracefully — do not retry automatically.\n- **Fork mode**: pass `fork=true` to inherit parent session context (conversation history, plan, skills). Use when the child needs your working context.",
            None,
        ));
        sections.push(wrap_section(
            "todo-guidance",
            "- Use TodoWrite proactively for complex multi-step implementation, debugging, refactoring, review, or multi-file tasks.\n- Do not use TodoWrite for trivial single-step, single-command, or purely informational requests.\n- After receiving a non-trivial task, create an initial checklist before or as you begin work.\n- When you use TodoWrite, write the full updated checklist for the current session, not a partial diff.\n- Keep the checklist short, concrete, and actionable.\n- Prefer exactly one in_progress item when work is actively underway.\n- Mark items completed immediately after finishing them, and remove stale items that no longer matter.",
            None,
        ));
        let plan_file_display = if self.plan_file.as_os_str().is_empty() {
            "<not set>".to_string()
        } else {
            self.plan_file.display().to_string()
        };
        let plan_draft_file_display = if self.plan_draft_file.as_os_str().is_empty() {
            "<not set>".to_string()
        } else {
            self.plan_draft_file.display().to_string()
        };
        let plan_lifecycle_guidance = format!(
            "- **PLANNING WORKFLOW** — For complex multi-step tasks (3+ steps OR multi-file OR user requests planning)\n\
             - **Why draft first?** Writing to PLAN_FILE immediately invalidates the system prompt cache. Use PLAN_DRAFT_FILE for all drafting iterations to avoid this cost.\n\
             - **Step-by-step**:\n\
               1. Write draft to PLAN_DRAFT_FILE using Edit (markdown: goal, analysis, steps, notes)\n\
               2. Ask user to confirm the plan before execution\n\
               3. **Draft revision loop**: while PLAN_DRAFT_FILE is non-empty and user has NOT said \"confirmed\"/\"ok\"/\"go ahead\" (or equivalent), ANY user reply (questions, suggestions, objections, or implicit change requests) MUST be treated as revision feedback. ALWAYS update PLAN_DRAFT_FILE to reflect the discussion, then ask for confirmation again. NEVER just answer without updating the draft.\n\
               4. If user explicitly cancels/abandons: use Bash to clear PLAN_DRAFT_FILE (e.g. `: > PLAN_DRAFT_FILE`). Do NOT use PlanClear.\n\
               5. When user confirms: call PlanConfirm tool — this moves draft → PLAN_FILE and triggers a context compaction (cache invalidation is already happening, so we reclaim space at the same time).\n\
               6. After PlanConfirm, create TodoWrite checklist based on plan\n\
               7. Execute tasks following todo checklist (update progress in TodoWrite)\n\
               8. When all tasks complete, use PlanClear tool to clear plan and compact context\n\
             - **Plan vs Todo separation**:\n\
               - PLAN_FILE: locked-in plan (only written via PlanConfirm)\n\
               - PLAN_DRAFT_FILE: working draft during planning (safe to edit freely)\n\
               - TodoWrite: execution checklist for real-time progress tracking\n\
               - Do NOT mix todo checkboxes into plan files\n\
             - **Files**:\n\
               - PLAN_DRAFT_FILE: {}\n\
               - PLAN_FILE: {}",
            plan_draft_file_display, plan_file_display
        );
        sections.push(wrap_section(
            "plan-lifecycle-guidance",
            &plan_lifecycle_guidance,
            None,
        ));
        if let Some(s) = self.build_instruction_files_section()? {
            sections.push(wrap_section("instruction-files", &s, None));
        }
        if let Some(s) = self.build_rule_index_section()? {
            sections.push(wrap_section("rule-index", &s, None));
        }
        if let Some(s) = self.build_skill_index_section()? {
            sections.push(wrap_section("skill-index", &s, None));
        }
        if let Some(s) = self.build_selected_skills_section()? {
            sections.push(wrap_section("selected-skills", &s, None));
        }
        if let Some(s) = read_optional_file(&self.plan_file)? {
            sections.push(wrap_section(
                "current-plan",
                &s,
                Some(&self.plan_file.display().to_string()),
            ));
        }
        if let Some(s) = read_optional_file(&self.summary_file)? {
            sections.push(wrap_section("context-snapshot", &s, None));
        }
        let output_language = if locale.starts_with("zh") {
            "再次强调：必须使用中文进行所有输出，包括你的思考过程（Chain of Thought/推理/thinking）！严禁在思考或回答中出现任何英文内容！".to_string()
        } else {
            format!(
                "MUST use \"{}\" for all output, including your Chain of Thought/reasoning/thinking! Never mix languages! Code, commands, and file content remain as-is.",
                locale
            )
        };
        sections.push(wrap_section("output-language", &output_language, None));

        // ═══ Mission override: load sections from MISSION.md (inline or file) ══════
        let mission_raw: Option<String> = if let Some(ref inline) = self.mission_content {
            Some(inline.clone())
        } else if let Some(ref mission_path) = self.mission_file {
            Some(fs::read_to_string(mission_path).map_err(|e| {
                anyhow::anyhow!(
                    "failed to read mission file {}: {e}",
                    mission_path.display()
                )
            })?)
        } else {
            None
        };

        if let Some(ref mission_content) = mission_raw {
            // Collect all level-1 headings from the mission file
            let mission_headings = extract_all_headings(mission_content);

            // Collect existing section tags for quick lookup
            let existing_tags: Vec<String> = sections
                .iter()
                .filter_map(|s| extract_tag_name(s))
                .collect();

            for heading in &mission_headings {
                if let Some(new_content) = extract_section(mission_content, heading) {
                    let new_wrapped = wrap_section(heading, &new_content, None);
                    // Backward compatibility: map old section tag names to merged sections
                    let matched_tag = match heading.as_str() {
                        "verification-gate" | "causal-reasoning" | "stop-triggers" => {
                            Some("execution-codes")
                        }
                        "using-your-tools" | "anchored-edit-protocol" => Some("tool-usage"),
                        "rationalization-table" => Some("belief-awareness"),
                        _ => None,
                    };
                    let effective_heading = matched_tag.unwrap_or(heading);
                    if let Some(pos) = existing_tags.iter().position(|t| t == effective_heading) {
                        // Replace existing section
                        sections[pos] = new_wrapped;
                    } else {
                        // Append new section
                        sections.push(new_wrapped);
                    }
                }
            }
        }

        Ok(sections.join("\n"))
    }

    fn build_instruction_files_section(&self) -> Result<Option<String>> {
        let mut out = Vec::new();
        for file in &self.context_file_snapshot.always_apply {
            out.push(wrap_section(
                "instruction-file",
                &file.context_file.content,
                Some(&file.context_file.name),
            ));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out.join("\n")))
        }
    }

    fn build_rules_section(&self) -> Result<Option<String>> {
        let mut sections = Vec::new();
        for rule in &self.rule_snapshot.always_apply {
            sections.push(wrap_section(
                "rule",
                &rule.rule.content,
                Some(&rule.rule.name),
            ));
        }
        if sections.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sections.join("\n")))
        }
    }

    fn build_rule_index_section(&self) -> Result<Option<String>> {
        let lines: Vec<String> = self
            .rule_snapshot
            .discoverable
            .iter()
            .map(|rule| {
                if rule.rule.description.is_empty() {
                    format!("- {}", rule.rule.name)
                } else {
                    format!("- {}: {}", rule.rule.name, rule.rule.description)
                }
            })
            .collect();
        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lines.join("\n")))
        }
    }

    fn build_skill_index_section(&self) -> Result<Option<String>> {
        let lines: Vec<String> = self
            .skill_snapshot
            .discoverable
            .iter()
            .map(|skill| {
                if skill.skill.description.is_empty() {
                    format!("- {}", skill.skill.name)
                } else {
                    format!("- {}: {}", skill.skill.name, skill.skill.description)
                }
            })
            .collect();
        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lines.join("\n")))
        }
    }

    fn build_selected_skills_section(&self) -> Result<Option<String>> {
        if self.skill_snapshot.selected.is_empty() {
            return Ok(None);
        }
        let mut sections = Vec::new();
        for resolved in &self.skill_snapshot.selected {
            let full = format!(
                "Base directory: {}\nFor files referenced by this skill, use Read with `skill://{}/<relative-path>`.\n\n{}",
                resolved.info.base_dir, resolved.info.name, resolved.content
            );
            sections.push(wrap_section("skill", &full, Some(&resolved.info.name)));
        }
        Ok(Some(sections.join("\n")))
    }
}

fn wrap_section(tag: &str, content: &str, name: Option<&str>) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    match name {
        Some(n) => format!("<{tag} name=\"{}\">\n{}\n</{tag}>", escape_attr(n), content),
        None => format!("<{tag}>\n{}\n</{tag}>", content),
    }
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Extract the tag name from a wrapped section string like ``<tag>...</tag>``.
fn extract_tag_name(wrapped: &str) -> Option<String> {
    let s = wrapped.trim();
    if !s.starts_with('<') {
        return None;
    }
    let closing = s.find('>')?;
    let tag = &s[1..closing];
    // Skip if it has attributes (name="...")
    if tag.contains(' ') {
        return None;
    }
    Some(tag.to_string())
}

/// Extract all level-1 headings (``# name``) from a MISSION.md file.
fn extract_all_headings(content: &str) -> Vec<String> {
    let mut headings = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Match lines starting with "# " but not "## "
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            let name = trimmed.trim_start_matches("# ").trim().to_string();
            if !name.is_empty() {
                headings.push(name);
            }
        }
    }
    headings
}

/// Extract content under a markdown level-1 heading from a MISSION.md file.
///
/// Looks for ``# <heading>`` at the start of a line and returns everything
/// between that heading and the next level-1 heading (or EOF).
fn extract_section(content: &str, heading: &str) -> Option<String> {
    let target = format!("# {}", heading);
    let lines: Vec<&str> = content.lines().collect();
    let mut start_idx = None;

    for (i, line) in lines.iter().enumerate() {
        if line.trim() == target.as_str() {
            start_idx = Some(i + 1);
            break;
        }
    }

    let start = start_idx?;
    let mut end = lines.len();
    for (i, line) in lines[start..].iter().enumerate() {
        if line.trim().starts_with("# ") && !line.trim().starts_with("## ") {
            end = start + i;
            break;
        }
    }

    let section_lines: Vec<&str> = lines[start..end]
        .iter()
        .copied()
        .skip_while(|l| l.trim().is_empty())
        .collect();
    if section_lines.is_empty() {
        return None;
    }
    let result = section_lines.join("\n");
    if result.trim().is_empty() {
        None
    } else {
        Some(result)
    }
}

fn read_optional_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let s = fs::read_to_string(path)?;
    if s.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_builder() -> Builder {
        let cwd = PathBuf::from("/tmp");
        let home = PathBuf::from("/home/user");
        Builder {
            cwd: cwd.clone(),
            home: home.clone(),
            skill_snapshot: Arc::new(crate::capabilities::SkillSnapshot::default()),
            context_file_snapshot: Arc::new(crate::capabilities::ContextFileSnapshot::default()),
            rule_snapshot: Arc::new(
                crate::capabilities::build_default_rule_snapshot(&cwd, &home, "session", "session")
                    .unwrap(),
            ),
            summary_file: PathBuf::from("/tmp/summary.txt"),
            plan_file: PathBuf::from("/tmp/plan.md"),
            plan_draft_file: PathBuf::from("/tmp/plan.draft"),
            mission_file: None,
            mission_content: None,
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mink-prompt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    fn signal_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("signal env lock poisoned")
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.old {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn build_system_prompt_contains_agent_identity() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("mink"), "should contain project name");
        assert!(
            prompt.contains("<agent-identity>"),
            "should have identity section"
        );
    }

    #[test]
    fn build_system_prompt_contains_environment() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(
            prompt.contains("<environment>"),
            "should have environment section"
        );
        assert!(prompt.contains("/tmp"), "should contain cwd");
    }

    #[test]
    fn build_system_prompt_has_all_sections() {
        let _lock = signal_env_lock();
        let _guard = EnvGuard::set("MINK_SIGNAL_MODE", "full");
        let prompt = test_builder().build_system_prompt().unwrap();
        let sections = vec![
            "<agent-identity>",
            "<environment>",
            "<rules>",
            "<execution-codes>",
            "<belief-awareness>",
            "<tool-usage>",
            "<sub-agent-guidance>",
            "<todo-guidance>",
            "<plan-lifecycle-guidance>",
            "<output-language>",
        ];
        for section in &sections {
            assert!(prompt.contains(section), "missing section: {}", section);
        }
    }

    #[test]
    fn build_system_prompt_includes_execution_codes() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("execution-codes"));
        assert!(prompt.contains("IDENTIFY"));
        assert!(prompt.contains("Guessing is lying"));
        assert!(prompt.contains("3 consecutive tool calls"));
    }

    #[test]
    fn build_system_prompt_includes_tool_usage() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("tool-usage"));
        assert!(prompt.contains("TOOL PRIORITY"));
        assert!(prompt.contains("DETECTED and REJECTED at runtime"));
    }

    #[test]
    fn build_system_prompt_includes_signal_recovery_protocol() {
        let _lock = signal_env_lock();
        let _guard = EnvGuard::set("MINK_SIGNAL_MODE", "full");
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("SIGNAL_RECOVERY mode"));
        assert!(prompt.contains("The FIRST tool call after the signal MUST inspect current state"));
        assert!(prompt.contains(
            "Calling Edit or Write as the next tool action after [System note: ...] is a violation"
        ));
        assert!(prompt.contains("each new signal restarts the first-tool rule"));
        assert!(!prompt.contains("Make at most one minimal corrective edit"));
        assert!(!prompt.contains("Verify with the narrowest failing command first"));
    }

    #[test]
    fn build_system_prompt_omits_signal_protocol_when_disabled() {
        let _lock = signal_env_lock();
        let _guard = EnvGuard::set("MINK_SIGNAL_MODE", "off");
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(!prompt.contains("<belief-awareness>"));
        assert!(!prompt.contains("SIGNAL_RECOVERY mode"));
        assert!(!prompt.contains("belief score"));
    }

    #[test]
    fn build_system_prompt_includes_rationalization_table_in_belief_awareness() {
        let _lock = signal_env_lock();
        let _guard = EnvGuard::set("MINK_SIGNAL_MODE", "full");
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("Common rationalizations"));
        assert!(prompt.contains("Without searching you WILL miss"));
    }

    #[test]
    fn build_system_prompt_includes_plan_lifecycle() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("PLANNING WORKFLOW"));
        assert!(prompt.contains("PlanConfirm"));
        assert!(prompt.contains("PlanClear"));
    }

    #[test]
    fn build_system_prompt_includes_todo_guidance() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("TodoWrite"));
    }

    #[test]
    fn build_system_prompt_mentions_skill_subresources() {
        let prompt = test_builder().build_system_prompt().unwrap();
        assert!(prompt.contains("skill://<name>/<relative-path>"));
        assert!(prompt.contains("rather than guessing local paths"));
    }

    #[test]
    fn selected_model_addressable_skill_enters_prompt() {
        let root = temp_root("selected-hidden");
        let home = root.join("home");
        let cwd = root.join("workspace");
        let skill_dir = cwd.join("skills/hidden-review");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Hidden review\"\nhide: true\n---\n\nHidden body",
        )
        .unwrap();
        let snapshot = crate::capabilities::build_default_skill_snapshot(
            &cwd,
            &home,
            "session-1",
            "session-1",
            &["hidden-review".to_string()],
        )
        .unwrap();
        let mut builder = test_builder();
        builder.cwd = cwd;
        builder.home = home;
        builder.skill_snapshot = Arc::new(snapshot);

        let prompt = builder.build_system_prompt().unwrap();

        assert!(prompt.contains("<selected-skills>"));
        assert!(prompt.contains("<skill name=\"hidden-review\">"));
        assert!(prompt.contains("Hidden body"));
        assert!(
            prompt.contains(
                "For files referenced by this skill, use Read with `skill://hidden-review/<relative-path>`."
            ),
            "{prompt}"
        );
        assert!(!prompt.contains("- hidden-review: Hidden review"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_and_read_skill_share_same_snapshot() {
        let root = temp_root("shared-snapshot");
        let home = root.join("home");
        let cwd = root.join("workspace");
        let skill_dir = cwd.join(".claude/skills/debugging");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Snapshot debugging\"\n---\n\nSnapshot body",
        )
        .unwrap();
        let snapshot = Arc::new(
            crate::capabilities::CapabilitySnapshot::load_default(
                &cwd,
                &home,
                "session-1",
                "session-1",
                &[],
            )
            .unwrap(),
        );
        let mut builder = test_builder();
        builder.cwd = cwd.clone();
        builder.home = home.clone();
        builder.skill_snapshot = Arc::new(snapshot.skills.clone());
        let prompt = builder.build_system_prompt().unwrap();

        let session = home.join(".mink/projects/-workspace/session-1");
        fs::create_dir_all(session.join("artifacts")).unwrap();
        fs::write(session.join("conversation.jsonl"), "").unwrap();
        fs::write(session.join("stats.json"), "{}\n").unwrap();
        let artifacts = Arc::new(crate::session::artifacts::ArtifactManager::new(
            session.join("artifacts"),
        ));
        artifacts.ensure().unwrap();
        let ctx = crate::context::ToolContext {
            vfs_scope: crate::tools::vfs::VfsScope {
                resource_session_id: "session-1".into(),
                agent_session_id: "session-1".into(),
            },
            read_only_fs: None,
            cwd,
            home,
            store: Arc::new(crate::session::store::ConversationStore::new(
                session.join("conversation.jsonl"),
            )),
            artifacts,
            snapshots: Arc::new(std::sync::Mutex::new(
                crate::tools::snapshot::FileSnapshotStore::default(),
            )),
            tool_config: crate::context::ToolConfig::from_config(&crate::config::Config::default()),
            interrupt: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resource_router: Arc::new(crate::resources::ResourceRouter::with_builtin_handlers()),
            capability_snapshot: snapshot,
        };
        let read = crate::resources::skill::read_skill_resource("skill://debugging", &ctx).unwrap();

        assert!(prompt.contains("Snapshot debugging"));
        assert!(read.contains("Snapshot debugging"));
        assert!(read.contains("Snapshot body"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_files_and_rules_enter_prompt_from_snapshot() {
        let root = temp_root("context-rules");
        let home = root.join("home");
        let cwd = root.join("workspace");
        fs::create_dir_all(home.join(".mink")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(home.join(".mink/AGENTS.md"), "Global instruction").unwrap();
        fs::write(cwd.join("AGENTS.md"), "Project instruction").unwrap();
        let snapshot = crate::capabilities::CapabilitySnapshot::load_default(
            &cwd,
            &home,
            "session-1",
            "session-1",
            &[],
        )
        .unwrap();
        let mut builder = test_builder();
        builder.cwd = cwd;
        builder.home = home;
        builder.context_file_snapshot = Arc::new(snapshot.context_files.clone());
        builder.rule_snapshot = Arc::new(snapshot.rules.clone());

        let prompt = builder.build_system_prompt().unwrap();

        assert!(prompt.contains("<instruction-files>"));
        assert!(prompt.contains("Global instruction"));
        assert!(prompt.contains("Project instruction"));
        assert!(prompt.contains("<rules>"));
        assert!(prompt.contains("<rule name=\"default-agent-rules\">"));
        assert!(prompt.contains("<rule-index>"));
        assert!(prompt.contains("default-agent-rules"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_file_change_updates_capability_fingerprint() {
        let root = temp_root("context-fingerprint");
        let home = root.join("home");
        let cwd = root.join("workspace");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(cwd.join("AGENTS.md"), "Project instruction 1").unwrap();
        let first = crate::capabilities::CapabilitySnapshot::load_default(
            &cwd,
            &home,
            "session-1",
            "session-1",
            &[],
        )
        .unwrap();
        fs::write(cwd.join("AGENTS.md"), "Project instruction 2").unwrap();
        let second = crate::capabilities::CapabilitySnapshot::load_default(
            &cwd,
            &home,
            "session-1",
            "session-1",
            &[],
        )
        .unwrap();

        assert_ne!(first.dependency_fingerprint, second.dependency_fingerprint);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn wrap_section_with_name() {
        let result = wrap_section("test", "content", Some("section-name"));
        assert!(result.contains("<test name="));
        assert!(result.contains("content"));
        assert!(result.contains("</test>"));
    }

    #[test]
    fn wrap_section_without_name() {
        let result = wrap_section("test", "content", None);
        assert!(result.contains("<test>"));
        assert!(result.contains("content"));
        assert!(result.contains("</test>"));
    }

    #[test]
    fn wrap_section_empty_content_returns_empty() {
        let result = wrap_section("test", "", None);
        assert_eq!(result, "");
    }

    #[test]
    fn read_optional_file_nonexistent_returns_none() {
        let result = read_optional_file(Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_none());
    }
}
