use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub tool: String,
    pub path: String,
    pub bytes: u64,
    pub created_at: String,
    pub description: String,
}

#[derive(Debug)]
pub struct ArtifactManager {
    root: PathBuf,
    index_path: PathBuf,
    counter: AtomicU64,
    index_lock: Mutex<()>,
}

impl ArtifactManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            index_path: root.join("index.jsonl"),
            root,
            counter: AtomicU64::new(0),
            index_lock: Mutex::new(()),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        if !self.index_path.exists() {
            std::fs::File::create(&self.index_path)?;
        }
        self.repair_index_tail()?;
        if self.counter.load(Ordering::SeqCst) == 0 {
            let next = self.next_counter_from_index()?;
            let _ = self
                .counter
                .compare_exchange(0, next, Ordering::SeqCst, Ordering::SeqCst);
        }
        Ok(())
    }

    pub fn write_text(
        &self,
        tool: &str,
        description: &str,
        content: &str,
    ) -> Result<ArtifactRecord> {
        self.ensure()?;
        let (id, filename, mut file) = loop {
            let id = self.next_id(tool);
            let filename = format!("{id}.txt");
            let path = self.root.join(&filename);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(file) => break (id, filename, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        let record = ArtifactRecord {
            id,
            tool: tool.to_string(),
            path: filename,
            bytes: content.len() as u64,
            created_at: crate::session::stats::chrono_now_rfc3339(),
            description: description.to_string(),
        };
        self.append_index(&record)?;
        Ok(record)
    }

    pub fn read_text(&self, id: &str) -> Result<String> {
        let record = self.get(id)?;
        self.read_record_text(&record)
    }

    pub fn read_text_prefix(&self, id: &str, max_bytes: usize) -> Result<(String, bool)> {
        let record = self.get(id)?;
        let file = std::fs::File::open(self.root.join(&record.path))?;
        let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
        file.take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        let truncated = bytes.len() > max_bytes;
        bytes.truncate(max_bytes);
        while std::str::from_utf8(&bytes).is_err() {
            if bytes.pop().is_none() {
                break;
            }
        }
        // pop 循环保证退出时必为合法 UTF-8（含空串）。
        Ok((
            String::from_utf8(bytes).expect("bytes truncated to a char boundary"),
            truncated,
        ))
    }

    pub fn read_record_text(&self, record: &ArtifactRecord) -> Result<String> {
        std::fs::read_to_string(self.root.join(&record.path)).map_err(Into::into)
    }

    pub fn get(&self, id: &str) -> Result<ArtifactRecord> {
        validate_id(id)?;
        let index = match std::fs::read_to_string(&self.index_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        for line in index.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ArtifactRecord = serde_json::from_str(line)?;
            if record.id == id {
                return Ok(record);
            }
        }
        bail!("Error: artifact not found: {id}");
    }

    fn next_id(&self, tool: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("{}-{n:04}", sanitize_tool_name(tool))
    }

    fn next_counter_from_index(&self) -> Result<u64> {
        let index = std::fs::read_to_string(&self.index_path)?;
        let mut max = 0u64;
        for line in index.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<ArtifactRecord>(line) else {
                continue;
            };
            if let Some(number) = record
                .id
                .rsplit_once('-')
                .and_then(|(_, suffix)| suffix.parse::<u64>().ok())
            {
                max = max.max(number);
            }
        }
        max.checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("artifact id counter exhausted"))
    }

    fn append_index(&self, record: &ArtifactRecord) -> Result<()> {
        let _guard = self
            .index_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let line = format!("{}\n", serde_json::to_string(record)?);
        crate::session::jsonl::append_line(&self.index_path, line.as_bytes(), true)
    }

    fn repair_index_tail(&self) -> Result<()> {
        // 与 conversation.jsonl / usage.jsonl 共用同一尾部修复策略。
        let _guard = self
            .index_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::session::jsonl::repair_unterminated_tail(&self.index_path)
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!("Error: invalid artifact id: {id}");
    }
    Ok(())
}

fn sanitize_tool_name(tool: &str) -> String {
    let mut out = String::new();
    for ch in tool.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "artifact".to_string()
    } else {
        out
    }
}

pub fn artifact_id_from_url(input: &str) -> Option<&str> {
    input.strip_prefix("artifact://").and_then(|rest| {
        let id = rest.split_once(':').map_or(rest, |(id, _)| id);
        (!id.is_empty()).then_some(id)
    })
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
