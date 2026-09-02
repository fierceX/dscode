use crate::protocol::ToolCallEvent;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, RwLock};

/// ConversationStore provides async JSONL conversation persistence.
pub struct ConversationStore {
    path: PathBuf,
    /// Parsed active suffix. Complete history remains authoritative on disk.
    cache: RwLock<Option<CachedLines>>,
    write_lock: Mutex<()>,
}

#[derive(Debug, Clone)]
struct CachedLines {
    start: usize,
    lines: Vec<Value>,
}

impl ConversationStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: RwLock::new(None),
            write_lock: Mutex::new(()),
        }
    }

    pub async fn ensure(&self) -> Result<()> {
        if !self.path.exists() {
            if let Some(parent) = self.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::File::create(&self.path).await?;
        }
        Ok(())
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub async fn add_user(&self, content: &str) -> Result<()> {
        self.append_line(&json!({"role":"user","content":content}))
            .await
    }

    /// Append an engine-injected user-role message (todo reminders, signal
    /// recovery guidance, ...). The `internal` flag lets compaction and other
    /// consumers distinguish runtime injections from real user constraints
    /// without maintaining a string-prefix denylist.
    pub async fn add_runtime_user(&self, content: &str) -> Result<()> {
        self.append_line(&json!({"role":"user","content":content,"internal":true}))
            .await
    }
    pub async fn add_assistant(
        &self,
        text: &str,
        thinking: &str,
        calls: &[ToolCallEvent],
    ) -> Result<()> {
        let mut content = Vec::new();
        content.push(json!({"type":"thinking","thinking":thinking}));
        content.push(json!({"type":"text","text":text}));
        for c in calls {
            content.push(json!({"type":"tool_use","id":c.id,"name":c.name,"input":c.input_json}));
        }
        self.append_line(&json!({"role":"assistant","content":content}))
            .await
    }

    pub async fn add_tool_results(
        &self,
        results: &[crate::tools::runner::ToolExecution],
    ) -> Result<()> {
        self.append_line(&Self::tool_results_message(results)).await
    }

    pub(crate) fn tool_results_message(results: &[crate::tools::runner::ToolExecution]) -> Value {
        let content: Vec<Value> = results
            .iter()
            .flat_map(|r| {
                let conv = if r.conv_content.is_empty() {
                    &r.content
                } else {
                    &r.conv_content
                };
                let mut blocks =
                    vec![json!({"type":"tool_result","tool_use_id":r.tool_use_id,"content":conv})];
                if let Some(metadata) = &r.state_metadata {
                    blocks[0]["_mink"] = metadata.clone();
                }
                // Image capture: append an interleaved label + attachment
                // block so the model can associate each image with its call
                // (v7 §8.1). The block carries budget metadata but never a
                // path.
                if let Some(image) = &r.image_attachment {
                    if !image.name.is_empty() {
                        blocks.push(json!({
                            "type": "text",
                            "text": format!("Image for {}: {}", r.tool_use_id, image.name),
                        }));
                    }
                    blocks.push(json!({
                        "type": "tool_attachment",
                        "tool_use_id": r.tool_use_id,
                        "url": format!("image://{}", image.image_id),
                        "format": image.format,
                        "width": image.width,
                        "height": image.height,
                        "bytes": image.bytes,
                    }));
                }
                blocks
            })
            .collect();
        json!({"role":"user","content":content})
    }

    pub(crate) async fn append_runtime_message(&self, message: Value) -> Result<()> {
        self.append_line(&message).await
    }

    /// Append and fsync an engine-owned state transition before its external
    /// transaction journal is cleared. This is intentionally reserved for
    /// cross-file commits; ordinary high-frequency conversation appends keep
    /// the existing flush-only behavior.
    pub(crate) async fn append_runtime_message_durable(&self, message: Value) -> Result<()> {
        self.append_line_with_sync(&message, true).await
    }

    pub async fn lines(&self) -> Result<Vec<Value>> {
        self.lines_from(0).await
    }

    pub async fn lines_from(&self, start: usize) -> Result<Vec<Value>> {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && start >= cache.start
            {
                let offset = start - cache.start;
                if offset <= cache.lines.len() {
                    return Ok(cache.lines[offset..].to_vec());
                }
            }
        }

        let _guard = self.write_lock.lock().await;
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && start >= cache.start
            {
                let offset = start - cache.start;
                if offset <= cache.lines.len() {
                    return Ok(cache.lines[offset..].to_vec());
                }
            }
        }

        let lines = self.read_lines_from_disk(start).await?;
        let mut cache = self.cache.write().await;
        if cache.is_none() || cache.as_ref().is_some_and(|cached| start >= cached.start) {
            *cache = Some(CachedLines {
                start,
                lines: lines.clone(),
            });
        }
        Ok(lines)
    }

    pub async fn prune_cache_before(&self, start: usize) {
        let mut cache = self.cache.write().await;
        let Some(cache) = cache.as_mut() else {
            return;
        };
        if start <= cache.start {
            return;
        }
        let remove = (start - cache.start).min(cache.lines.len());
        cache.lines.drain(..remove);
        cache.start = start;
    }

    pub async fn last_assistant_message(&self) -> Result<Option<Value>> {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && let Some(message) = cache.lines.iter().rev().find(|message| {
                    message.get("role").and_then(Value::as_str) == Some("assistant")
                })
            {
                return Ok(Some(message.clone()));
            }
        }
        let _guard = self.write_lock.lock().await;
        self.read_last_assistant_from_disk().await
    }

    pub async fn lines_lossy_with_warnings<F>(&self, mut warn: F) -> Result<Vec<Value>>
    where
        F: FnMut(String),
    {
        let data = tokio::fs::read_to_string(&self.path).await?;
        Ok(crate::session::jsonl::parse_lossy_lines(
            &self.path, &data, &mut warn,
        ))
    }

    async fn read_lines_from_disk(&self, start: usize) -> Result<Vec<Value>> {
        let file = tokio::fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut physical_line = 0usize;
        let mut message_index = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line).await?;
            if bytes == 0 {
                break;
            }
            physical_line += 1;
            if line.trim().is_empty() {
                continue;
            }
            let terminated = line.ends_with('\n');
            let value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) if !terminated => break,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "invalid JSONL in {} at line {}: {}",
                        self.path.display(),
                        physical_line,
                        e
                    ));
                }
            };
            if message_index >= start {
                lines.push(value);
            }
            message_index += 1;
        }
        if start > message_index {
            return Err(anyhow::anyhow!(
                "conversation start index {} exceeds history length {}",
                start,
                message_index
            ));
        }
        Ok(lines)
    }

    async fn read_last_assistant_from_disk(&self) -> Result<Option<Value>> {
        let file = tokio::fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(file);
        let mut latest = None;
        let mut physical_line = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line).await?;
            if bytes == 0 {
                break;
            }
            physical_line += 1;
            if line.trim().is_empty() {
                continue;
            }
            let terminated = line.ends_with('\n');
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) if !terminated => break,
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "invalid JSONL in {} at line {}: {}",
                        self.path.display(),
                        physical_line,
                        error
                    ));
                }
            };
            if value.get("role").and_then(Value::as_str) == Some("assistant") {
                latest = Some(value);
            }
        }
        Ok(latest)
    }

    async fn append_line(&self, value: &Value) -> Result<()> {
        self.append_line_with_sync(value, false).await
    }

    async fn append_line_with_sync(&self, value: &Value, sync: bool) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || crate::session::jsonl::append_line(&path, &line, sync))
            .await??;
        // Append to cache instead of invalidating
        let mut cache = self.cache.write().await;
        if let Some(cache) = cache.as_mut() {
            cache.lines.push(value.clone());
        }
        Ok(())
    }

    /// Append synthetic tool_result blocks for every assistant tool_use id
    /// that has no matching tool_result anywhere in the file. Called once at
    /// session init (single-writer startup, before any turn task exists), so
    /// no lock is held across the scan; the append itself takes the write
    /// lock internally. Restores message pairing after a turn was interrupted
    /// or aborted between persisting tool_calls and their results.
    pub async fn repair_dangling_tool_uses(&self) -> Result<()> {
        let lines = self.lines_lossy_with_warnings(|_| {}).await?;
        let mut pending: Vec<String> = Vec::new();
        let mut resolved: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in &lines {
            let Some(content) = line.get("content").and_then(serde_json::Value::as_array) else {
                continue;
            };
            for block in content {
                match block.get("type").and_then(serde_json::Value::as_str) {
                    Some("tool_use") => {
                        if let Some(id) = block.get("id").and_then(serde_json::Value::as_str) {
                            let id = id.to_string();
                            if !resolved.contains(&id) && !pending.contains(&id) {
                                pending.push(id);
                            }
                        }
                    }
                    Some("tool_result") => {
                        if let Some(id) =
                            block.get("tool_use_id").and_then(serde_json::Value::as_str)
                        {
                            resolved.insert(id.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        let dangling: Vec<String> = pending
            .into_iter()
            .filter(|id| !resolved.contains(id))
            .collect();
        if dangling.is_empty() {
            return Ok(());
        }
        let content: Vec<serde_json::Value> = dangling
            .iter()
            .map(|id| {
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": "[mink] tool execution did not complete before the session was interrupted; this synthetic result was appended on load to restore message pairing.",
                })
            })
            .collect();
        self.append_line(&serde_json::json!({"role": "user", "content": content}))
            .await
    }
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Extract fields from a JSON event's "input" field and build a tool call summary.
/// Shared by TUI replay and REPL replay to avoid duplication.
pub fn build_tool_summary_from_json(name: &str, evt: &serde_json::Value) -> String {
    let input = evt.get("input").cloned().unwrap_or(serde_json::Value::Null);
    let mut fields = std::collections::BTreeMap::new();
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            match v {
                serde_json::Value::String(s) => {
                    fields.insert(k.clone(), s.clone());
                }
                _ => {
                    fields.insert(k.clone(), v.to_string());
                }
            }
        }
    }
    build_tool_call_summary(name, &fields)
}

pub fn build_tool_call_summary(
    name: &str,
    fields: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut label = String::new();
    match name {
        "Read" | "Write" => label = fields.get("path").cloned().unwrap_or_default(),
        "Edit" => {
            if let Some(input) = fields.get("input") {
                label = match crate::tools::hashline::parse(input) {
                    Ok(patch) => {
                        let operations = patch
                            .sections
                            .iter()
                            .map(|section| section.operations.len())
                            .sum::<usize>();
                        let paths = patch
                            .sections
                            .iter()
                            .map(|section| section.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "{} sections, {operations} ops: {paths}",
                            patch.sections.len()
                        )
                    }
                    Err(_) => "invalid hashline input".into(),
                };
            } else if let Some(path) = fields.get("path") {
                let edits = fields
                    .get("edits")
                    .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
                    .map_or(0, |values| values.len());
                label = format!("{path}, {edits} edits");
            }
        }
        "Glob" | "Grep" => label = fields.get("pattern").cloned().unwrap_or_default(),
        "Bash" => {
            label = fields
                .get("command")
                .cloned()
                .unwrap_or_default()
                .replace('\n', " ");
            if label.chars().count() > 80 {
                let truncated: String = label.chars().take(77).collect();
                label = format!("{truncated}...");
            }
        }
        "TodoRead" => label = "state".into(),
        "TodoWrite" => {
            let changes = ["add", "update", "remove"]
                .iter()
                .filter_map(|field| fields.get(*field))
                .filter_map(|value| serde_json::from_str::<Vec<Value>>(value).ok())
                .map(|values| values.len())
                .sum::<usize>();
            let revision = fields
                .get("base_revision")
                .map(String::as_str)
                .unwrap_or("?");
            label = format!("{changes} changes @r{revision}");
        }
        "TodoAdvance" => {
            let transitions = ["complete", "activate", "pause", "reopen"]
                .iter()
                .filter_map(|field| fields.get(*field))
                .filter_map(|value| serde_json::from_str::<Vec<Value>>(value).ok())
                .map(|values| values.len())
                .sum::<usize>();
            let revision = fields
                .get("base_revision")
                .map(String::as_str)
                .unwrap_or("?");
            label = format!("{transitions} transitions @r{revision}");
        }
        "Skill" => label = fields.get("name").cloned().unwrap_or_default(),
        "SubAgent" => label = fields.get("description").cloned().unwrap_or_default(),
        _ => {}
    }
    if label.is_empty() {
        name.to_string()
    } else {
        format!("{name}({label})")
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
