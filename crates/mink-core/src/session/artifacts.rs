use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
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
    pub source: Option<String>,
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
            counter: AtomicU64::new(1),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        if !self.index_path.exists() {
            std::fs::File::create(&self.index_path)?;
        }
        Ok(())
    }

    pub fn write_text(
        &self,
        tool: &str,
        description: &str,
        source: Option<&str>,
        content: &str,
    ) -> Result<ArtifactRecord> {
        self.ensure()?;
        let id = self.next_id(tool);
        let filename = format!("{id}.txt");
        let path = self.root.join(&filename);
        std::fs::write(&path, content)?;
        let record = ArtifactRecord {
            id,
            tool: tool.to_string(),
            path: filename,
            bytes: content.len() as u64,
            created_at: crate::session::stats::chrono_now_rfc3339(),
            description: description.to_string(),
            source: source.map(ToString::to_string),
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

    pub fn find_latest_by_source(
        &self,
        tool: &str,
        source: &str,
    ) -> Result<Option<ArtifactRecord>> {
        let index = match std::fs::read_to_string(&self.index_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        for line in index.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<ArtifactRecord>(line) else {
                continue;
            };
            if record.tool == tool && record.source.as_deref() == Some(source) {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn next_id(&self, tool: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("{}-{n:04}", sanitize_tool_name(tool))
    }

    fn append_index(&self, record: &ArtifactRecord) -> Result<()> {
        use std::io::Write;
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
        let record = manager
            .write_text("Bash", "full output", Some("echo"), "hello")
            .unwrap();
        assert_eq!(record.id, "bash-0001");
        assert_eq!(manager.read_text(&record.id).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(manager.root);
    }

    #[test]
    fn finds_latest_artifact_by_tool_and_source() {
        let manager = temp_manager("find-source");
        manager
            .write_text("ReadUrl", "old", Some("https://example.com"), "old")
            .unwrap();
        let latest = manager
            .write_text("ReadUrl", "new", Some("https://example.com"), "new")
            .unwrap();
        manager
            .write_text("ReadUrl", "other", Some("https://other.example"), "other")
            .unwrap();

        let found = manager
            .find_latest_by_source("ReadUrl", "https://example.com")
            .unwrap()
            .unwrap();

        assert_eq!(found.id, latest.id);
        assert_eq!(manager.read_text(&found.id).unwrap(), "new");
        let _ = std::fs::remove_dir_all(manager.root);
    }

    #[test]
    fn source_lookup_skips_corrupt_index_lines() {
        use std::io::Write;

        let manager = temp_manager("find-source-corrupt");
        let latest = manager
            .write_text("ReadUrl", "new", Some("https://example.com"), "new")
            .unwrap();
        let mut index = std::fs::OpenOptions::new()
            .append(true)
            .open(&manager.index_path)
            .unwrap();
        writeln!(index, "{{not-json").unwrap();

        let found = manager
            .find_latest_by_source("ReadUrl", "https://example.com")
            .unwrap()
            .unwrap();

        assert_eq!(found.id, latest.id);
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
        let record = manager
            .write_text("Web Fetch", "full output", None, "x")
            .unwrap();
        assert_eq!(record.id, "webfetch-0001");
        let _ = std::fs::remove_dir_all(manager.root);
    }
}
