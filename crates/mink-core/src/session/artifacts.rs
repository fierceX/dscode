use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
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
}

impl ArtifactManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            index_path: root.join("index.jsonl"),
            root,
            counter: AtomicU64::new(0),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        if !self.index_path.exists() {
            std::fs::File::create(&self.index_path)?;
        }
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
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.index_path)?;
        writeln!(file, "{}", serde_json::to_string(record)?)?;
        Ok(())
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
mod tests {
    use super::*;

    fn temp_manager(name: &str) -> ArtifactManager {
        let dir =
            std::env::temp_dir().join(format!("mink-artifacts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ArtifactManager::new(dir)
    }

    #[test]
    fn write_and_read_artifact() {
        let manager = temp_manager("write-read");
        let record = manager.write_text("Bash", "full output", "hello").unwrap();
        assert_eq!(record.id, "bash-0001");
        assert_eq!(manager.read_text(&record.id).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(manager.root);
    }

    #[test]
    fn resumed_manager_continues_ids_without_overwriting() {
        let manager = temp_manager("resume-counter");
        let root = manager.root.clone();
        let old = manager
            .write_text("Bash", "old output", "inherited")
            .unwrap();
        drop(manager);

        let resumed = ArtifactManager::new(root.clone());
        resumed.ensure().unwrap();
        let new = resumed
            .write_text("Bash", "new output", "continued")
            .unwrap();

        assert_eq!(old.id, "bash-0001");
        assert_eq!(new.id, "bash-0002");
        assert_eq!(resumed.read_text(&old.id).unwrap(), "inherited");
        assert_eq!(resumed.read_text(&new.id).unwrap(), "continued");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_artifact_file_is_not_overwritten() {
        let manager = temp_manager("orphan-file");
        manager.ensure().unwrap();
        std::fs::write(manager.root.join("bash-0001.txt"), "orphaned content").unwrap();

        let record = manager
            .write_text("Bash", "new output", "new content")
            .unwrap();

        assert_eq!(record.id, "bash-0002");
        assert_eq!(
            std::fs::read_to_string(manager.root.join("bash-0001.txt")).unwrap(),
            "orphaned content"
        );
        let _ = std::fs::remove_dir_all(manager.root);
    }

    #[test]
    fn rejects_path_traversal_id() {
        let manager = temp_manager("reject");
        assert!(manager.read_text("../secret").is_err());
        let _ = std::fs::remove_dir_all(manager.root);
    }

    #[test]
    fn parses_artifact_url_id() {
        assert_eq!(
            artifact_id_from_url("artifact://bash-0001"),
            Some("bash-0001")
        );
        assert_eq!(
            artifact_id_from_url("artifact://bash-0001:1-20"),
            Some("bash-0001")
        );
        assert_eq!(artifact_id_from_url("file://x"), None);
    }

    #[test]
    fn sanitizes_tool_name_for_id() {
        let manager = temp_manager("sanitize");
        let record = manager.write_text("Tool Name", "full output", "x").unwrap();
        assert_eq!(record.id, "toolname-0001");
        let _ = std::fs::remove_dir_all(manager.root);
    }
}
