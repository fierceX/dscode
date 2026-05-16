use crate::compact_dp::{CompactionTier, compact_turn_keep};
use crate::config::Config;
use crate::llm::transport::build_openai_body;
use crate::prompt;
use crate::protocol::{Event, TextEvent, ErrorEvent, StopEvent, UsageEvent};
use crate::session::store::ConversationStore;
use crate::session::stats::StatsTracker;
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
        let compact_pct = std::env::var("CONTEXT_COMPACT_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(85);
        Self { store, summary_path, plan_path, plan_draft_path, cwd, home, skills, api_url, api_key: config.api_key.clone(), config: config.clone(), stats, client, compact_pct }
    }

    fn should_compact(&self, trigger: &str, context_tokens: usize) -> bool {
        if trigger == "plan_clear" || trigger == "plan_confirm" {
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
            keep_lines.max(1).min(5)
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
        let total_tokens: usize = all.iter()
            .map(|m| serde_json::to_string(m).unwrap_or_default().len() / 4)
            .sum();
        let savings_ratio = if total_tokens > 0 {
            (total_tokens - kept_tokens) as f64 / total_tokens as f64
        } else {
            0.0
        };
        if savings_ratio < 0.10 && !is_plan_trigger(trigger) {
            return Ok((false, format!("savings too small: {:.1}%", savings_ratio * 100.0)));
        }

        let dropped_count = total_lines - k;
        let dropped_lines = &all[..dropped_count];

        let summary = self.run_summary_call(dropped_lines).await?;
        tokio::fs::write(&self.summary_path, format!("{summary}\n")).await?;
        self.store.trim_keep_last(k).await?;

        Ok((true, format!("compacted_at_trigger={trigger}_tier={tier:?}_kept={k}")))
    }

    async fn run_summary_call(&self, dropped_lines: &[Value]) -> Result<String> {
        let summary_instruction = "[CONVERSATION HISTORY SUMMARY — earlier turns compacted for context efficiency]\n\nUpdate the existing summary snapshot using the messages above. Use exactly these fields:\nTask focus:\nLatest request:\nProgress:\nTool evidence:\nReflections:\n\nIMPORTANT: Do NOT use any tools. Do NOT think. Just output the summary directly as plain text.";
        let mut messages: Vec<Value> = dropped_lines.to_vec();
        messages.push(json!({"role":"user","content":summary_instruction}));

        let system_prompt = prompt::Builder {
            cwd: self.cwd.clone(),
            home: self.home.clone(),
            skills: self.skills.clone(),
            summary_file: self.summary_path.clone(),
            plan_file: self.plan_path.clone(),
            plan_draft_file: self.plan_draft_path.clone(),
        }
        .build_system_prompt()?;

        let body = build_openai_body(
            &self.config.model, &messages, &[], &system_prompt,
            self.config.max_tokens,
        )?;

        let resp = self.client.post(&self.api_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .body(body)
            .send().await?;

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
                Event::Error(ErrorEvent { message }) => { last_error = message; }
                Event::Stop(StopEvent { reason }) => { stop_reason = reason; }
                _ => {}
            }
            Ok(())
        };

        let reader: Box<dyn std::io::Read + Send> = Box::new(std::io::Cursor::new(resp_bytes.to_vec()));
        crate::sse::openai::parse(reader, &mut |evt| {
            parse_emit(evt)
        })?;

        if let Some(usage) = compact_usage {
            self.stats.record_compact(&usage).await;
        }

        if out.is_empty() {
            bail!(
                "failed to generate context summary: empty text response (stop_reason={}, error={})",
                if stop_reason.is_empty() { "none" } else { &stop_reason },
                if last_error.is_empty() { "none" } else { &last_error }
            );
        }
        Ok(out)
    }

    fn log_compact_event(&self, usage: &UsageEvent) {
        let events_path = self.summary_path.with_file_name("events.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&events_path) {
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
    trigger == "plan_clear" || trigger == "plan_confirm"
}
