use crate::protocol::ToolCallEvent;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

/// Result of executing a single tool call, persisted in the conversation store.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub tool_args: std::collections::BTreeMap<String, String>,
    pub content: String,
    pub conv_content: String,
}

/// ConversationStore provides async JSONL conversation persistence.
pub struct ConversationStore {
    path: PathBuf,
    /// In-memory cache of parsed lines, lazily loaded and invalidated on write.
    cache: RwLock<Option<Vec<Value>>>,
}

impl ConversationStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: RwLock::new(None),
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
        self.append_line(&json!({"role":"user","content":content})).await
    }

    pub async fn add_assistant(&self, text: &str, thinking: &str, calls: &[ToolCallEvent]) -> Result<()> {
        let mut content = Vec::new();
        content.push(json!({"type":"thinking","thinking":thinking}));
        content.push(json!({"type":"text","text":text}));
        for c in calls {
            content.push(json!({"type":"tool_use","id":c.id,"name":c.name,"input":c.input_json}));
        }
        self.append_line(&json!({"role":"assistant","content":content})).await
    }

    pub async fn add_tool_results(&self, results: &[ToolResult]) -> Result<()> {
        let content: Vec<Value> = results
            .iter()
            .map(|r| {
                let conv = if r.conv_content.is_empty() { &r.content } else { &r.conv_content };
                json!({"type":"tool_result","tool_use_id":r.tool_use_id,"content":conv})
            })
            .collect();
        self.append_line(&json!({"role":"user","content":content})).await
    }

    pub async fn lines(&self) -> Result<Vec<Value>> {
        // Try cache first
        {
            let cache = self.cache.read().await;
            if let Some(ref lines) = *cache {
                return Ok(lines.clone());
            }
        }
        let lines = self.read_lines_from_disk().await?;
        let mut cache = self.cache.write().await;
        *cache = Some(lines.clone());
        Ok(lines)
    }

    async fn read_lines_from_disk(&self) -> Result<Vec<Value>> {
        let data = tokio::fs::read_to_string(&self.path).await?;
        let lines: Vec<Value> = data
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(lines)
    }

    pub async fn trim_keep_last(&self, keep_lines: usize) -> Result<()> {
        let lines = self.lines().await?;
        if keep_lines >= lines.len() {
            return Ok(());
        }
        let kept = &lines[lines.len() - keep_lines..];
        let mut out = String::new();
        for v in kept {
            out.push_str(&serde_json::to_string(v)?);
            out.push('\n');
        }
        tokio::fs::write(&self.path, out.as_bytes()).await?;
        // Invalidate cache
        let mut cache = self.cache.write().await;
        *cache = Some(kept.to_vec());
        Ok(())
    }

    async fn append_line(&self, value: &Value) -> Result<()> {
        let line = serde_json::to_string(value)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        // Append to cache instead of invalidating
        let mut cache = self.cache.write().await;
        if let Some(ref mut lines) = *cache {
            lines.push(value.clone());
        }
        Ok(())
    }
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
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
            label = fields.get("command").cloned().unwrap_or_default().replace('\n', " ");
            if label.chars().count() > 80 {
                let truncated: String = label.chars().take(77).collect();
                label = format!("{truncated}...");
            }
        }
        "TodoWrite" => {
            if let Some(summary) = fields.get("summary").cloned()
                && !summary.is_empty() { label = summary; }
            if label.is_empty()
                && let Some(todos) = fields.get("todos")
                    && let Ok(arr) = serde_json::from_str::<Vec<Value>>(todos) {
                        let total = arr.len();
                        let completed = arr.iter().filter(|item| {
                            item.get("status").and_then(Value::as_str) == Some("completed")
                        }).count();
                        label = format!("{completed}/{total}");
                    }
        }
        "Skill" => label = fields.get("name").cloned().unwrap_or_default(),
        "SubAgent" => label = fields.get("description").cloned().unwrap_or_default(),
        _ => {}
    }
    if label.is_empty() { name.to_string() } else { format!("{name}({label})") }
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
        store.add_assistant("response", "thinking...", &calls).await.unwrap();
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
            tool_use_id: "id1".into(), tool_name: "Bash".into(),
            tool_args: Default::default(), content: "output".into(), conv_content: "".into(),
        }];
        store.add_tool_results(&results).await.unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["role"], "user");
    }

    #[tokio::test]
    async fn trim_keep_last_removes_prefix() {
        let store = temp_store();
        store.ensure().await.unwrap();
        store.add_user("msg1").await.unwrap();
        store.add_user("msg2").await.unwrap();
        store.add_user("msg3").await.unwrap();
        store.trim_keep_last(2).await.unwrap();
        let lines = store.lines().await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["content"], "msg2");
        assert_eq!(lines[1]["content"], "msg3");
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
    }

    #[test]
    fn first_line_handles_newlines() {
        assert_eq!(first_line("one\ntwo\nthree"), "one");
        assert_eq!(first_line("single"), "single");
        assert_eq!(first_line(""), "");
    }
}
