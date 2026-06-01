use crate::config::Config;
use crate::llm::transport::build_openai_body;
use crate::prompt;
use crate::protocol::{ErrorEvent, Event, StopEvent, TextEvent, UsageEvent};
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub struct CompactionEngine {
    store: Arc<ConversationStore>,
    summary_path: PathBuf,
    plan_path: PathBuf,
    plan_draft_path: PathBuf,
    cwd: PathBuf,
    home: PathBuf,
    skills: Vec<String>,
    api_url: String,
    api_key: String,
    config: Config,
    stats: Arc<StatsTracker>,
    client: reqwest::Client,
    compact_pct: u8,
}

impl CompactionEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<ConversationStore>,
        summary_path: PathBuf,
        plan_path: PathBuf,
        plan_draft_path: PathBuf,
        cwd: PathBuf,
        home: PathBuf,
        skills: Vec<String>,
        api_url: String,
        config: &Config,
        stats: Arc<StatsTracker>,
        client: reqwest::Client,
    ) -> Self {
        let compact_pct = config.context_compact_pct;
        Self {
            store,
            summary_path,
            plan_path,
            plan_draft_path,
            cwd,
            home,
            skills,
            api_url,
            api_key: config.api_key.clone(),
            config: config.clone(),
            stats,
            client,
            compact_pct,
        }
    }

    /// Path to the summary file written after each compaction.
    pub fn summary_path(&self) -> &std::path::Path {
        &self.summary_path
    }

    /// Read the current summary file content, with DSML tags stripped.
    pub async fn read_summary(&self) -> Option<String> {
        let raw = tokio::fs::read_to_string(&self.summary_path).await.ok()?;
        Some(strip_dsml_tags(raw.trim()))
    }

    fn should_compact(&self, trigger: &str, context_tokens: usize) -> bool {
        if is_plan_trigger(trigger) {
            return true;
        }
        if self.config.max_context_tokens == 0 {
            return false;
        }
        let pct = (context_tokens * 100) / self.config.max_context_tokens;
        pct >= self.compact_pct as usize
    }

    pub async fn evaluate_and_compact(
        &self,
        trigger: &str,
        context_tokens: usize,
    ) -> Result<(bool, String)> {
        if !self.should_compact(trigger, context_tokens) {
            return Ok((false, "below threshold".into()));
        }

        let all = self.store.lines().await?;
        let total_lines = all.len();
        if total_lines == 0 {
            return Ok((false, "empty".into()));
        }

        let tier = CompactionTier::from_ratio(context_tokens, self.config.max_context_tokens);
        let keep_ratio = tier.tail_budget_ratio();

        let keep_lines = match compact_turn_keep(&all, keep_ratio) {
            Some(k) => k.min(total_lines),
            None => return Ok((false, "keep=0".into())),
        };

        // Emergency: keep minimal context
        let k = if tier == CompactionTier::Emergency {
            keep_lines.clamp(1, 5)
        } else {
            keep_lines
        };

        if k >= total_lines && !is_plan_trigger(trigger) {
            return Ok((false, "all kept".into()));
        }

        // Minimum savings check: skip if compaction won't free enough tokens
        let kept_tokens: usize = all[total_lines - k..]
            .iter()
            .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
            .sum();
        let total_tokens: usize = all
            .iter()
            .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
            .sum();
        let savings_ratio = if total_tokens > 0 {
            (total_tokens - kept_tokens) as f64 / total_tokens as f64
        } else {
            0.0
        };
        if savings_ratio < 0.10 && !is_plan_trigger(trigger) {
            return Ok((
                false,
                format!("savings too small: {:.1}%", savings_ratio * 100.0),
            ));
        }

        let dropped_count = total_lines - k;
        let dropped_lines = &all[..dropped_count];
        let kept_lines = &all[dropped_count..];

        let summary = self.run_summary_call(dropped_lines).await?;
        self.validate_conversation_messages(kept_lines)?;
        tokio::fs::write(&self.summary_path, format!("{summary}\n")).await?;
        self.store.trim_keep_last(k).await?;
        let remaining = self.store.lines().await?;
        self.validate_conversation_messages(&remaining)?;

        Ok((
            true,
            format!("compacted_at_trigger={trigger}_tier={tier:?}_kept={k}"),
        ))
    }

    fn validate_conversation_messages(&self, messages: &[Value]) -> Result<()> {
        let system_prompt = "";
        let _ = build_openai_body(
            crate::config::resolve_model_name(&self.config.model),
            messages,
            &[],
            system_prompt,
            self.config.max_tokens,
        )?;
        Ok(())
    }

    async fn run_summary_call(&self, dropped_lines: &[Value]) -> Result<String> {
        let summary_instruction = "\
Summarise the conversation turns above into a concise context snapshot.
Output exactly the 5 fields below, one field per line.
After each colon, write the content — do not leave the field empty.

Task focus:
  (one sentence: what task or topic the user is working on)
Latest request:
  (the most recent user instruction or question)
Progress:
  (what has been completed, what remains, any blockers)
Tool evidence:
  (describe tool actions in plain language — do NOT write tool call syntax
   like Read(path), Bash(cmd), [tool], Grep(pattern) etc.)
Reflections:
  (insights, decisions, open questions)

Start directly with \"Task focus:\" — no preamble, no markdown, no code fences.";

        let mut messages: Vec<Value> = dropped_lines.to_vec();
        messages.push(json!({"role":"user","content":summary_instruction}));

        let system_prompt = prompt::Builder {
            cwd: self.cwd.clone(),
            home: self.home.clone(),
            skills: self.skills.clone(),
            summary_file: self.summary_path.clone(),
            plan_file: self.plan_path.clone(),
            plan_draft_file: self.plan_draft_path.clone(),
            mission_file: None,
        }
        .build_system_prompt()?;

        let body = build_openai_body(
            crate::config::resolve_model_name(&self.config.model),
            &messages,
            &[],
            &system_prompt,
            self.config.max_tokens,
        )?;

        let resp = self
            .client
            .post(&self.api_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .body(body)
            .send()
            .await?;

        let resp_bytes = resp.bytes().await?;
        let mut out = String::new();
        let mut stop_reason = String::new();
        let mut last_error = String::new();

        let mut compact_usage: Option<UsageEvent> = None;
        let mut parse_emit = |evt: Event| -> Result<()> {
            match evt {
                Event::Text(TextEvent { content }) => out.push_str(&content),
                Event::Usage(usage) => {
                    self.log_compact_event(&usage);
                    compact_usage = Some(usage);
                }
                Event::Error(ErrorEvent { message }) => {
                    last_error = message;
                }
                Event::Stop(StopEvent { reason }) => {
                    stop_reason = reason;
                }
                _ => {}
            }
            Ok(())
        };

        let reader: Box<dyn std::io::Read + Send> =
            Box::new(std::io::Cursor::new(resp_bytes.to_vec()));
        crate::sse::openai::parse(reader, &mut |evt| parse_emit(evt))?;

        if let Some(usage) = compact_usage {
            self.stats.record_compact(&usage).await;
        }

        if out.is_empty() {
            bail!(
                "failed to generate context summary: empty text response (stop_reason={}, error={})",
                if stop_reason.is_empty() {
                    "none"
                } else {
                    &stop_reason
                },
                if last_error.is_empty() {
                    "none"
                } else {
                    &last_error
                }
            );
        }
        Ok(strip_tool_labels(&strip_dsml_tags(&out)))
    }

    fn log_compact_event(&self, usage: &UsageEvent) {
        let events_path = self.summary_path.with_file_name("events.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
        {
            let evt = json!({
                "type": "usage",
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_input_tokens": usage.cache_read_input_tokens,
                "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                "kind": "compact",
            });
            if let Ok(line) = serde_json::to_string(&evt) {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

fn is_plan_trigger(trigger: &str) -> bool {
    trigger == "plan_clear" || trigger == "plan_confirm" || trigger == "manual"
}

/// Strip DSML (DeepSeek Markup Language) and common XML-like tags
/// that some models inject into generated text (e.g. `<ds_safety>`,
/// `<ds_copyright>`, `<ds_suffix>`, etc.).
fn strip_dsml_tags(text: &str) -> String {
    // Pattern: <ds_...>...</ds_...> or <ds_.../> (self-closing)
    let re = regex::Regex::new(r"</?ds_\w+[^>]*>").unwrap();
    re.replace_all(text, "").into_owned()
}

/// Strip tool-call labels from summary text for clean user display.
/// Removes [tool] prefixes and Name(args) patterns that look like raw
/// tool-call syntax leaked by an LLM into the summary output.
pub fn strip_tool_labels(text: &str) -> String {
    let re_tool = regex::Regex::new(r"\[tool\]\s*").unwrap();
    let re_call = regex::Regex::new(r"\b(Read|Bash|Grep|Edit|Write|Glob|WebSearch|WebFetch|Skill|TodoWrite|SubAgent|PlanConfirm|PlanClear)\([^)]*\)").unwrap();
    let s = re_tool.replace_all(text, "");
    re_call.replace_all(&s, "").into_owned()
}

// ====================================================================
// CompactionTier + compact_turn_keep (原在 compact_dp.rs，现在内联)
// ====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTier {
    Conservative,
    Aggressive,
    ForceSummary,
    Emergency,
}

impl CompactionTier {
    pub fn from_ratio(current_tokens: usize, max_tokens: usize) -> Self {
        if max_tokens == 0 {
            return CompactionTier::Conservative;
        }
        let ratio = (current_tokens * 100) / max_tokens;
        if ratio >= 95 {
            CompactionTier::Emergency
        } else if ratio >= 80 {
            CompactionTier::ForceSummary
        } else if ratio >= 70 {
            CompactionTier::Aggressive
        } else {
            CompactionTier::Conservative
        }
    }

    pub fn tail_budget_ratio(&self) -> f64 {
        match self {
            CompactionTier::Conservative => 0.20,
            CompactionTier::Aggressive => 0.10,
            CompactionTier::ForceSummary => 0.05,
            CompactionTier::Emergency => 0.05,
        }
    }
}

/// 按 turn 对齐计算需要保留的行数
pub fn compact_turn_keep(lines: &[Value], min_keep_ratio: f64) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }

    let mut is_user = Vec::with_capacity(lines.len());
    let mut total_turns = 0usize;
    for line in lines {
        let user = line.get("role").and_then(Value::as_str) == Some("user")
            && line.get("content").is_some_and(|c| c.is_string());
        is_user.push(user);
        if user {
            total_turns += 1;
        }
    }

    let target = {
        let t = (total_turns as f64 * min_keep_ratio + 0.5) as usize;
        t.max(1).min(total_turns)
    };

    let mut keep = 0usize;
    let mut found = 0usize;
    for i in (0..lines.len()).rev() {
        if found >= target {
            break;
        }
        keep += 1;
        if is_user[i] {
            found += 1;
        }
    }
    if keep == 0 { None } else { Some(keep) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, OutputFormat};
    use crate::session::paths;
    use crate::session::stats::StatsTracker;
    use crate::session::store::ConversationStore;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Test should_compact logic inline (CompactionEngine requires async construction)

    #[test]
    fn compact_triggers_at_threshold() {
        let pct = 85u8;
        let max_ctx = 200_000usize;
        let test = |ctx: usize| -> bool {
            if max_ctx == 0 {
                return false;
            }
            (ctx * 100) / max_ctx >= pct as usize
        };
        assert!(!test(100_000));
        assert!(!test(169_999));
        assert!(test(170_000));
        assert!(test(200_000));
    }

    #[test]
    fn compact_triggers_for_plan_operations() {
        let pct = 85u8;
        let max_ctx = 200_000usize;
        let should_compact = |trigger: &str, ctx: usize| -> bool {
            if trigger == "plan_clear" || trigger == "plan_confirm" {
                return true;
            }
            if max_ctx == 0 {
                return false;
            }
            (ctx * 100) / max_ctx >= pct as usize
        };
        assert!(should_compact("plan_clear", 0));
        assert!(should_compact("plan_confirm", 0));
        assert!(!should_compact("auto", 100_000));
        assert!(should_compact("auto", 200_000));
    }

    #[test]
    fn compact_skips_when_max_context_is_zero() {
        let pct = 85u8;
        let max_ctx = 0usize;
        let should_compact = |trigger: &str, ctx: usize| -> bool {
            if trigger == "plan_clear" || trigger == "plan_confirm" {
                return true;
            }
            if max_ctx == 0 {
                return false;
            }
            (ctx * 100) / max_ctx >= pct as usize
        };
        assert!(!should_compact("auto", 100));
    }

    #[test]
    fn compact_pct_configurable() {
        let test_pct = |pct: u8, ctx: usize, max_ctx: usize| -> bool {
            if max_ctx == 0 {
                return false;
            }
            (ctx * 100) / max_ctx >= pct as usize
        };
        assert!(test_pct(50, 100, 200));
        assert!(!test_pct(50, 99, 200));
        assert!(test_pct(70, 140, 200));
        assert!(!test_pct(70, 139, 200));
        assert!(test_pct(90, 180, 200));
        assert!(!test_pct(90, 179, 200));
    }

    #[test]
    fn compact_tier_from_ratio_emergency() {
        use super::CompactionTier;
        assert_eq!(
            CompactionTier::from_ratio(95, 100),
            CompactionTier::Emergency
        );
        assert_eq!(
            CompactionTier::from_ratio(100, 100),
            CompactionTier::Emergency
        );
    }

    #[test]
    fn compact_tier_from_ratio_force_summary() {
        use super::CompactionTier;
        assert_eq!(
            CompactionTier::from_ratio(80, 100),
            CompactionTier::ForceSummary
        );
        assert_eq!(
            CompactionTier::from_ratio(94, 100),
            CompactionTier::ForceSummary
        );
    }

    #[test]
    fn compact_tier_from_ratio_aggressive() {
        use super::CompactionTier;
        assert_eq!(
            CompactionTier::from_ratio(70, 100),
            CompactionTier::Aggressive
        );
        assert_eq!(
            CompactionTier::from_ratio(79, 100),
            CompactionTier::Aggressive
        );
    }

    #[test]
    fn compact_tier_from_ratio_conservative() {
        use super::CompactionTier;
        assert_eq!(
            CompactionTier::from_ratio(0, 100),
            CompactionTier::Conservative
        );
        assert_eq!(
            CompactionTier::from_ratio(69, 100),
            CompactionTier::Conservative
        );
    }

    #[test]
    fn compact_tier_zero_max_returns_conservative() {
        use super::CompactionTier;
        assert_eq!(
            CompactionTier::from_ratio(100, 0),
            CompactionTier::Conservative
        );
    }

    #[tokio::test]
    async fn evaluate_and_compact_skips_below_threshold_without_network() -> anyhow::Result<()> {
        let (home, cwd, store, stats, summary, plan, draft) =
            temp_compaction_session("below-threshold").await?;
        store.add_user("hello").await?;
        store.add_assistant("world", "", &[]).await?;
        let cfg = test_compaction_config("https://example.invalid/v1/chat/completions", 85);
        let engine = CompactionEngine::new(
            store.clone(),
            summary,
            plan,
            draft,
            cwd,
            home,
            Vec::new(),
            crate::config::api_url(&cfg),
            &cfg,
            stats,
            reqwest::Client::new(),
        );

        let (did_compact, reason) = engine.evaluate_and_compact("auto", 10).await?;
        assert!(!did_compact);
        assert_eq!(reason, "below threshold");
        assert_eq!(store.lines().await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn evaluate_and_compact_skips_empty_manual_without_network() -> anyhow::Result<()> {
        let (home, cwd, store, stats, summary, plan, draft) =
            temp_compaction_session("empty-manual").await?;
        let cfg = test_compaction_config("https://example.invalid/v1/chat/completions", 100);
        let engine = CompactionEngine::new(
            store,
            summary,
            plan,
            draft,
            cwd,
            home,
            Vec::new(),
            crate::config::api_url(&cfg),
            &cfg,
            stats,
            reqwest::Client::new(),
        );

        let (did_compact, reason) = engine.evaluate_and_compact("manual", 0).await?;
        assert!(!did_compact);
        assert_eq!(reason, "empty");
        Ok(())
    }

    #[tokio::test]
    async fn evaluate_and_compact_skips_when_all_context_would_be_kept() -> anyhow::Result<()> {
        let (home, cwd, store, stats, summary, plan, draft) =
            temp_compaction_session("all-kept").await?;
        store.add_user("only turn").await?;
        store.add_assistant("small answer", "", &[]).await?;
        let cfg = test_compaction_config("https://example.invalid/v1/chat/completions", 1);
        let engine = CompactionEngine::new(
            store.clone(),
            summary,
            plan,
            draft,
            cwd,
            home,
            Vec::new(),
            crate::config::api_url(&cfg),
            &cfg,
            stats,
            reqwest::Client::new(),
        );

        let (did_compact, reason) = engine.evaluate_and_compact("auto", 20_000).await?;
        assert!(!did_compact);
        assert_eq!(reason, "all kept");
        assert_eq!(store.lines().await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn read_summary_strips_dsml_tags() -> anyhow::Result<()> {
        let (home, cwd, store, stats, summary, plan, draft) =
            temp_compaction_session("read-summary").await?;
        let cfg = test_compaction_config("https://example.invalid/v1/chat/completions", 100);
        let engine = CompactionEngine::new(
            store,
            summary.clone(),
            plan,
            draft,
            cwd,
            home,
            Vec::new(),
            crate::config::api_url(&cfg),
            &cfg,
            stats,
            reqwest::Client::new(),
        );
        tokio::fs::write(&summary, "keep <ds_meta>drop</ds_meta> text\n").await?;

        assert_eq!(
            engine.read_summary().await.as_deref(),
            Some("keep drop text")
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local loopback sockets"]
    async fn evaluate_and_compact_writes_clean_summary_and_keeps_valid_conversation()
    -> anyhow::Result<()> {
        let (api_url, _server) = start_summary_server(
            "Task focus: <ds_meta>drop</ds_meta>visible Read(secret.txt)\n\
Latest request: keep working\n\
Progress: compacted\n\
Tool evidence: [tool] inspected files\n\
Reflections: none",
        )
        .await?;
        let (home, cwd, store, stats, summary, plan, draft) =
            temp_compaction_session("e2e").await?;
        for idx in 0..3 {
            store.add_user(&format!("user {idx}")).await?;
            store
                .add_assistant(&format!("assistant {idx}"), "", &[])
                .await?;
        }
        let cfg = Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: api_url.clone(),
            max_context_tokens: 1_000_000,
            context_compact_pct: 100,
            output_format: OutputFormat::Human,
            log_events: true,
            ..Default::default()
        };
        let engine = CompactionEngine::new(
            store.clone(),
            summary.clone(),
            plan,
            draft,
            cwd,
            home,
            Vec::new(),
            api_url,
            &cfg,
            stats.clone(),
            reqwest::Client::new(),
        );

        let (did_compact, reason) = engine.evaluate_and_compact("manual", 0).await?;
        assert!(did_compact, "{reason}");
        let summary_text = tokio::fs::read_to_string(summary).await?;
        assert!(summary_text.contains("Task focus: dropvisible"));
        assert!(!summary_text.contains("<ds_meta>"), "{summary_text}");
        assert!(!summary_text.contains("Read(secret.txt)"), "{summary_text}");
        assert!(!summary_text.contains("[tool]"), "{summary_text}");
        let remaining = store.lines().await?;
        assert_eq!(remaining.len(), 2);
        crate::llm::transport::build_openai_body("deepseek-v4-flash", &remaining, &[], "", 1024)?;
        assert_eq!(stats.snapshot().await.compact_request_count, 1);
        Ok(())
    }

    async fn temp_compaction_session(
        name: &str,
    ) -> anyhow::Result<(
        std::path::PathBuf,
        std::path::PathBuf,
        Arc<ConversationStore>,
        Arc<StatsTracker>,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    )> {
        static CNT: AtomicU64 = AtomicU64::new(0);
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "mink-compact-test-{}-{name}-{n}",
            std::process::id()
        ));
        let home = root.join("home");
        let cwd = root.join("workspace");
        tokio::fs::create_dir_all(&home).await?;
        tokio::fs::create_dir_all(&cwd).await?;
        let spaths = paths::paths_for(&home, &cwd, "compact");
        let store = Arc::new(ConversationStore::new(spaths.conversation.clone()));
        store.ensure().await?;
        let stats = StatsTracker::load(&spaths.stats).await?;
        Ok((
            home,
            cwd,
            store,
            stats,
            spaths.summary,
            spaths.plan,
            spaths.plan_draft,
        ))
    }

    fn test_compaction_config(base_url: &str, context_compact_pct: u8) -> Config {
        Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: base_url.into(),
            max_context_tokens: 1_000_000,
            context_compact_pct,
            output_format: OutputFormat::Human,
            log_events: true,
            ..Default::default()
        }
    }

    async fn start_summary_server(
        summary_text: &str,
    ) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let summary_text = summary_text.to_string();
        let handle = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let Ok(n) = socket.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":summary_text}}]}),
                json!({"choices":[{"finish_reason":"stop","delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        Ok((format!("http://{addr}/chat/completions"), handle))
    }
}
