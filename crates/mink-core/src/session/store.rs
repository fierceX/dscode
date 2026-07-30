use crate::protocol::ToolCallEvent;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};

/// Result of executing a single tool call, persisted in the conversation store.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_args: std::collections::BTreeMap<String, String>,
    pub content: String,
    pub conv_content: String,
    pub state_metadata: Option<Value>,
}

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

    pub async fn add_tool_results(&self, results: &[ToolResult]) -> Result<()> {
        let content: Vec<Value> = results
            .iter()
            .map(|r| {
                let conv = if r.conv_content.is_empty() {
                    &r.content
                } else {
                    &r.conv_content
                };
                let mut block =
                    json!({"type":"tool_result","tool_use_id":r.tool_use_id,"content":conv});
                if let Some(metadata) = &r.state_metadata {
                    block["_mink"] = metadata.clone();
                }
                block
            })
            .collect();
        self.append_line(&json!({"role":"user","content":content}))
            .await
    }

    pub(crate) async fn append_runtime_message(&self, message: Value) -> Result<()> {
        self.append_line(&message).await
    }

    pub async fn lines(&self) -> Result<Vec<Value>> {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && cache.start == 0
            {
                return Ok(cache.lines.clone());
            }
        }

        let _guard = self.write_lock.lock().await;
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref()
                && cache.start == 0
            {
                return Ok(cache.lines.clone());
            }
        }

        let lines = self.read_lines_from_disk(0).await?;
        let mut cache = self.cache.write().await;
        if cache.is_none() {
            *cache = Some(CachedLines {
                start: 0,
                lines: lines.clone(),
            });
        }
        Ok(lines)
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

    pub async fn lines_lossy(&self) -> Result<Vec<Value>> {
        self.lines_lossy_with_warnings(|_| {}).await
    }

    pub async fn lines_lossy_with_warnings<F>(&self, mut warn: F) -> Result<Vec<Value>>
    where
        F: FnMut(String),
    {
        let data = tokio::fs::read_to_string(&self.path).await?;
        let mut lines = Vec::new();
        for (idx, line) in data.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(value) => lines.push(value),
                Err(e) => warn(format!(
                    "invalid JSONL in {} at line {} skipped by lossy read: {}",
                    self.path.display(),
                    idx + 1,
                    e
                )),
            }
        }
        Ok(lines)
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
        let _guard = self.write_lock.lock().await;
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .append(true)
            .open(&self.path)
            .await?;
        repair_unterminated_tail(&mut file).await?;
        file.write_all(&line).await?;
        file.flush().await?;
        // Append to cache instead of invalidating
        let mut cache = self.cache.write().await;
        if let Some(cache) = cache.as_mut() {
            cache.lines.push(value.clone());
        }
        Ok(())
    }
}

async fn repair_unterminated_tail(file: &mut tokio::fs::File) -> Result<()> {
    const SCAN_CHUNK_BYTES: u64 = 8 * 1024;

    let len = file.metadata().await?.len();
    if len == 0 {
        return Ok(());
    }

    file.seek(std::io::SeekFrom::End(-1)).await?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last).await?;
    if last[0] == b'\n' {
        return Ok(());
    }

    let mut scan_end = len;
    let mut tail_start = 0;
    while scan_end > 0 {
        let scan_start = scan_end.saturating_sub(SCAN_CHUNK_BYTES);
        let chunk_len = usize::try_from(scan_end - scan_start)?;
        let mut chunk = vec![0u8; chunk_len];
        file.seek(std::io::SeekFrom::Start(scan_start)).await?;
        file.read_exact(&mut chunk).await?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            tail_start = scan_start + index as u64 + 1;
            break;
        }
        scan_end = scan_start;
    }

    let tail_len = usize::try_from(len - tail_start)?;
    let mut tail = vec![0u8; tail_len];
    file.seek(std::io::SeekFrom::Start(tail_start)).await?;
    file.read_exact(&mut tail).await?;
    if serde_json::from_slice::<Value>(&tail).is_ok() {
        file.write_all(b"\n").await?;
    } else {
        file.set_len(tail_start).await?;
    }
    Ok(())
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
        "Read" | "Write" | "Edit" => label = fields.get("path").cloned().unwrap_or_default(),
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
mod tests {
    use super::*;
    use tokio;

    fn temp_store() -> ConversationStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CNT: AtomicU64 = AtomicU64::new(0);
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("conv-test-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        ConversationStore::new(dir.join("conversation.jsonl"))
    }

    #[tokio::test]
    async fn add_user_and_read_back() {
        let store = temp_store();
        store.ensure().await.unwrap();
        store.add_user("hello").await.unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(lines[0]["content"], "hello");
    }

    #[tokio::test]
    async fn add_assistant_with_thinking_and_text() {
        let store = temp_store();
        store.ensure().await.unwrap();
        let calls = vec![];
        store
            .add_assistant("response", "thinking...", &calls)
            .await
            .unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["role"], "assistant");
        let content = lines[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "thinking...");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "response");
    }

    #[tokio::test]
    async fn add_tool_results_then_lines() {
        let store = temp_store();
        store.ensure().await.unwrap();
        let results = vec![ToolResult {
            tool_use_id: "id1".into(),
            tool_name: "TodoAdvance".into(),
            tool_args: Default::default(),
            content: "output".into(),
            conv_content: "".into(),
            state_metadata: Some(json!({
                "todo_revision": 3,
                "todo_state_kind": "progress",
            })),
        }];
        store.add_tool_results(&results).await.unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(
            lines[0]["content"][0]["_mink"]["todo_revision"].as_u64(),
            Some(3)
        );
    }

    #[tokio::test]
    async fn cache_appended_not_invalidated() {
        let store = temp_store();
        store.ensure().await.unwrap();
        store.add_user("a").await.unwrap();
        let _ = store.lines().await.unwrap(); // populate cache
        store.add_user("b").await.unwrap();
        // cache should be updated, not None
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 2);
    }

    #[tokio::test]
    async fn active_suffix_cache_stays_pruned_when_full_history_is_read() {
        let store = temp_store();
        store.ensure().await.unwrap();
        for value in ["a", "b", "c", "d"] {
            store.add_user(value).await.unwrap();
        }
        assert_eq!(store.lines_from(0).await.unwrap().len(), 4);

        store.prune_cache_before(2).await;
        let active = store.lines_from(2).await.unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0]["content"], "c");

        assert_eq!(store.lines().await.unwrap().len(), 4);
        let cache = store.cache.read().await;
        let cache = cache.as_ref().unwrap();
        assert_eq!(cache.start, 2);
        assert_eq!(cache.lines.len(), 2);
    }

    #[tokio::test]
    async fn active_suffix_cache_accepts_new_messages() {
        let store = temp_store();
        store.ensure().await.unwrap();
        store.add_user("old").await.unwrap();
        store.add_user("active").await.unwrap();
        let _ = store.lines_from(0).await.unwrap();
        store.prune_cache_before(1).await;

        store.add_user("new").await.unwrap();

        let active = store.lines_from(1).await.unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0]["content"], "active");
        assert_eq!(active[1]["content"], "new");
    }

    #[tokio::test]
    async fn lines_from_rejects_boundary_beyond_history() {
        let store = temp_store();
        store.ensure().await.unwrap();
        store.add_user("only").await.unwrap();

        let error = store.lines_from(2).await.unwrap_err().to_string();
        assert!(error.contains("exceeds history length 1"), "{error}");
    }

    #[tokio::test]
    async fn strict_lines_errors_on_bad_json_with_line_number() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(
            store.path(),
            "{\"role\":\"user\",\"content\":\"ok\"}\nnot-json\n",
        )
        .await
        .unwrap();
        let err = store.lines().await.unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    #[tokio::test]
    async fn lossy_lines_skips_bad_json() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(
            store.path(),
            "{\"role\":\"user\",\"content\":\"ok\"}\nnot-json\n{\"role\":\"user\",\"content\":\"ok2\"}\n",
        )
        .await
        .unwrap();
        let lines = store.lines_lossy().await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["content"], "ok");
        assert_eq!(lines[1]["content"], "ok2");
    }

    #[tokio::test]
    async fn lossy_lines_reports_bad_json_warnings() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(store.path(), "{\"role\":\"user\"}\nnot-json\n")
            .await
            .unwrap();
        let mut warnings = Vec::new();
        let lines = store
            .lines_lossy_with_warnings(|warning| warnings.push(warning))
            .await
            .unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("line 2"), "{}", warnings[0]);
    }

    #[tokio::test]
    async fn strict_lines_skips_partial_trailing_jsonl_without_newline() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(
            store.path(),
            "{\"role\":\"user\",\"content\":\"ok\"}\n{\"role\":\"assistant\"",
        )
        .await
        .unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["content"], "ok");
    }

    #[tokio::test]
    async fn append_repairs_partial_trailing_record_before_writing() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(
            store.path(),
            "{\"role\":\"user\",\"content\":\"kept\"}\n{\"role\":\"assistant\"",
        )
        .await
        .unwrap();
        assert_eq!(store.lines().await.unwrap().len(), 1);

        store.add_user("next").await.unwrap();

        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["content"], "kept");
        assert_eq!(lines[1]["content"], "next");
    }

    #[tokio::test]
    async fn append_preserves_valid_record_without_final_newline() {
        let store = temp_store();
        store.ensure().await.unwrap();
        tokio::fs::write(store.path(), "{\"role\":\"user\",\"content\":\"kept\"}")
            .await
            .unwrap();
        assert_eq!(store.lines().await.unwrap().len(), 1);

        store.add_user("next").await.unwrap();

        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["content"], "kept");
        assert_eq!(lines[1]["content"], "next");
    }

    #[test]
    fn build_tool_call_summary_labels() {
        let mut f = std::collections::BTreeMap::new();
        f.insert("path".into(), "/tmp/test.txt".into());
        assert!(build_tool_call_summary("Read", &f).contains("test.txt"));

        let mut f2 = std::collections::BTreeMap::new();
        f2.insert("command".into(), "echo hello".into());
        assert!(build_tool_call_summary("Bash", &f2).contains("echo hello"));

        let mut f3 = std::collections::BTreeMap::new();
        f3.insert("description".into(), "child task".into());
        assert!(build_tool_call_summary("SubAgent", &f3).contains("child task"));

        let mut todo = std::collections::BTreeMap::new();
        todo.insert("base_revision".into(), "4".into());
        todo.insert("add".into(), r#"[{"content":"one"}]"#.into());
        todo.insert(
            "update".into(),
            r#"[{"id":"T0001","content":"revised"}]"#.into(),
        );
        assert_eq!(
            build_tool_call_summary("TodoWrite", &todo),
            "TodoWrite(2 changes @r4)"
        );
        assert_eq!(
            build_tool_call_summary("TodoRead", &Default::default()),
            "TodoRead(state)"
        );
    }

    #[test]
    fn first_line_handles_newlines() {
        assert_eq!(first_line("one\ntwo\nthree"), "one");
        assert_eq!(first_line("single"), "single");
        assert_eq!(first_line(""), "");
    }
}
