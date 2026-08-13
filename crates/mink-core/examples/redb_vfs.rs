//! Example read-only VFS backed by redb.
//!
//! `mink-core` does not depend on redb. This example keeps the adapter in the
//! embedding application and uses only the public VFS hook.

use anyhow::{Result, anyhow, bail};
use mink::prelude::{
    AgentOptions, AgentRuntime, ReadOnlyFileSystem, VfsGlobRequest, VfsGlobResult, VfsGrepEntry,
    VfsGrepRequest, VfsGrepResult, VfsReadRequest, VfsReadResult, VfsScope,
};
use mink::runtime::{
    normalize_virtual_file_path, normalize_virtual_root, select_virtual_lines, tool_line_count,
    validate_virtual_glob_request, validate_virtual_grep_request,
};
use redb::{Database, ReadableDatabase, TableDefinition};
use regex::Regex;
use std::path::Path;
use std::sync::Arc;

const FILES: TableDefinition<&str, &str> = TableDefinition::new("virtual_files");

struct StoredFile {
    path: String,
    content: String,
}

struct RedbFileSystem {
    db: Database,
}

impl RedbFileSystem {
    fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            db: Database::create(path)?,
        })
    }

    /// Import is intentionally outside the VFS trait: the agent-facing hook is
    /// read-only, while the embedding service owns ingestion and authorization.
    fn put(&self, resource_session_id: &str, path: &str, content: &str) -> Result<()> {
        let path = normalize_path(path)?;
        let key = file_key(resource_session_id, &path)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(FILES)?;
            table.insert(key.as_str(), content)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn get(&self, resource_session_id: &str, path: &str) -> Result<Option<String>> {
        let path = normalize_path(path)?;
        let key = file_key(resource_session_id, &path)?;
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILES)?;
        Ok(table
            .get(key.as_str())?
            .map(|value| value.value().to_owned()))
    }

    fn scan_files<T>(
        &self,
        resource_session_id: &str,
        root: &str,
        include_content: bool,
        consume: impl FnOnce(&mut dyn Iterator<Item = Result<StoredFile>>) -> Result<T>,
    ) -> Result<T> {
        let session_prefix = session_prefix(resource_session_id)?;
        let root = normalize_virtual_root(root)?;
        let prefix = format!("{session_prefix}{root}");
        let session_upper = format!("{resource_session_id}\u{1}");
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILES)?;
        let root_prefix = (!root.is_empty()).then(|| format!("{root}/"));
        let mut files = table
            .range(prefix.as_str()..session_upper.as_str())?
            .map_while(|row| {
                let (key, value) = match row {
                    Ok(row) => row,
                    Err(error) => return Some(Err(error.into())),
                };
                let path = match key.value().strip_prefix(&session_prefix) {
                    Some(path) => path.to_string(),
                    None => return Some(Err(anyhow!("invalid redb VFS key"))),
                };
                if !root.is_empty()
                    && path != root
                    && !root_prefix
                        .as_deref()
                        .is_some_and(|prefix| path.starts_with(prefix))
                {
                    if root_prefix
                        .as_deref()
                        .is_some_and(|prefix| path.as_str() < prefix)
                    {
                        return Some(Ok(StoredFile {
                            path,
                            content: String::new(),
                        }));
                    }
                    return None;
                }
                Some(Ok(StoredFile {
                    path,
                    content: if include_content {
                        value.value().to_owned()
                    } else {
                        String::new()
                    },
                }))
            });
        consume(&mut files)
    }
}

impl ReadOnlyFileSystem for RedbFileSystem {
    fn read(&self, scope: &VfsScope, request: &VfsReadRequest) -> Result<VfsReadResult> {
        let path = normalize_path(&request.path)?;
        let full = self
            .get(&scope.resource_session_id, &path)?
            .ok_or_else(|| anyhow!("Error: file not found or unreadable: {path}"))?;
        if request.offset.is_none()
            && request.limit.is_none()
            && full.len() > request.max_full_read_bytes
        {
            bail!(
                "Error: file too large for full Read ({} bytes > {} bytes): {}. Use a line selector such as '{}:1-200' or pass offset/limit.",
                full.len(),
                request.max_full_read_bytes,
                path,
                path
            );
        }
        let content = select_virtual_lines(&full, request.offset, request.limit, &path)?;
        Ok(VfsReadResult {
            content,
            total_lines: tool_line_count(&full),
            total_bytes: full.len(),
        })
    }

    fn glob(&self, scope: &VfsScope, request: &VfsGlobRequest) -> Result<VfsGlobResult> {
        validate_virtual_glob_request(request)?;
        let matcher = globset::GlobBuilder::new(&request.pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| anyhow!("Error: invalid glob pattern '{}': {e}", request.pattern))?
            .compile_matcher();
        let root = normalize_virtual_root(&request.path)?;
        self.scan_files(&scope.resource_session_id, &request.path, false, |files| {
            let mut result = VfsGlobResult::default();
            for file in files {
                let file = file?;
                let Some(relative) = relative_path(&file.path, &root) else {
                    continue;
                };
                result.scanned_files += 1;
                if result.scanned_files > request.max_files {
                    result.scanned_files = request.max_files;
                    result.truncated = true;
                    break;
                }
                if matcher.is_match(relative) {
                    result.paths.push(relative.to_string());
                }
            }
            Ok(result)
        })
    }

    fn grep(&self, scope: &VfsScope, request: &VfsGrepRequest) -> Result<VfsGrepResult> {
        validate_virtual_grep_request(request)?;
        let regex = Regex::new(&request.pattern)
            .map_err(|e| anyhow!("Error: invalid regex pattern '{}': {e}", request.pattern))?;
        let file_matcher = if request.file_glob.is_empty() {
            None
        } else {
            Some(
                globset::GlobBuilder::new(&request.file_glob)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| anyhow!("Error: invalid glob '{}': {e}", request.file_glob))?
                    .compile_matcher(),
            )
        };
        let root = normalize_virtual_root(&request.path)?;
        let context = request.context.unwrap_or(0);
        self.scan_files(&scope.resource_session_id, &request.path, true, |files| {
            let mut result = VfsGrepResult::default();
            for file in files {
                let file = file?;
                let Some(relative) = relative_path(&file.path, &root) else {
                    continue;
                };
                result.scanned_files += 1;
                if result.scanned_files > request.max_files {
                    result.scanned_files = request.max_files;
                    result.truncated_files = true;
                    break;
                }
                if file_matcher
                    .as_ref()
                    .is_some_and(|matcher| !matcher.is_match(relative))
                {
                    continue;
                }

                let lines: Vec<&str> = file.content.lines().collect();
                for (index, line) in lines.iter().enumerate() {
                    if !regex.is_match(line) {
                        continue;
                    }
                    if result.match_count >= request.max_results {
                        result.truncated_results = true;
                        break;
                    }
                    if context > 0 {
                        let start = index.saturating_sub(context);
                        let end = (index + 1 + context).min(lines.len());
                        for (line_index, context_line) in
                            lines.iter().enumerate().take(end).skip(start)
                        {
                            result.entries.push(VfsGrepEntry::Line {
                                path: file.path.clone(),
                                line_number: line_index + 1,
                                content: (*context_line).to_string(),
                                matched: line_index == index,
                            });
                        }
                    } else {
                        result.entries.push(VfsGrepEntry::Line {
                            path: file.path.clone(),
                            line_number: index + 1,
                            content: (*line).to_string(),
                            matched: true,
                        });
                    }
                    result.match_count += 1;
                    if result.match_count >= request.max_results {
                        result.truncated_results = true;
                        break;
                    }
                }
                if result.truncated_results {
                    break;
                }
            }
            Ok(result)
        })
    }
}

fn relative_path<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root.is_empty() {
        Some(path)
    } else if path == root {
        Some("")
    } else {
        path.strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

fn file_key(resource_session_id: &str, path: &str) -> Result<String> {
    Ok(format!("{}{path}", session_prefix(resource_session_id)?))
}

fn normalize_path(path: &str) -> Result<String> {
    normalize_virtual_file_path(path)
}

fn session_prefix(resource_session_id: &str) -> Result<String> {
    if resource_session_id.is_empty()
        || resource_session_id.contains('\0')
        || resource_session_id.contains('\u{1}')
    {
        bail!("invalid resource session id");
    }
    Ok(format!("{resource_session_id}\0"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::temp_dir().join("mink-redb-vfs-example");
    std::fs::create_dir_all(&root)?;
    let fs = Arc::new(RedbFileSystem::open(root.join("knowledge.redb"))?);
    fs.put(
        "tenant-task-001",
        "knowledge/refunds.md",
        "# Refunds\nRefunds are reviewed within two business days.\n",
    )?;

    let runtime = AgentRuntime::start(
        AgentOptions::new(root.join("session"), ".")
            .with_resource_session_id("tenant-task-001")
            .with_read_only_file_system(fs),
    )
    .await?;

    let outcome = runtime
        .run_turn("Read knowledge/refunds.md and summarize it.")
        .await?;
    println!("{}", outcome.text);
    runtime.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mink-redb-vfs-{name}-{nanos}.redb"))
    }

    fn scope(resource_session_id: &str) -> VfsScope {
        VfsScope {
            resource_session_id: resource_session_id.into(),
            agent_session_id: "agent-1".into(),
        }
    }

    #[test]
    fn isolates_sessions_and_applies_read_ranges() {
        let path = temp_db("read");
        let fs = RedbFileSystem::open(&path).unwrap();
        fs.put("tenant-a", "docs/guide.md", "a1\na2\na3\n").unwrap();
        fs.put("tenant-b", "docs/guide.md", "b1\nb2\n").unwrap();

        let result = fs
            .read(
                &scope("tenant-a"),
                &VfsReadRequest {
                    path: "./docs/guide.md".into(),
                    offset: Some(2),
                    limit: Some(1),
                    max_full_read_bytes: 100,
                },
            )
            .unwrap();
        assert_eq!(result.content, "a2");
        assert_eq!(result.total_lines, 4);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn glob_and_grep_respect_root_limits_and_session() {
        let path = temp_db("search");
        let fs = RedbFileSystem::open(&path).unwrap();
        fs.put("tenant-a", "docs/a.md", "intro\nneedle\n").unwrap();
        fs.put("tenant-a", "docs/b.md", "other\nneedle\n").unwrap();
        fs.put("tenant-a", "docs-old.md", "must not hide docs subtree\n")
            .unwrap();
        fs.put("tenant-a", "src/lib.rs", "needle\n").unwrap();
        fs.put("tenant-b", "docs/private.md", "needle\n").unwrap();

        let glob_request = VfsGlobRequest {
            pattern: "*.md".into(),
            path: "./docs".into(),
            max_files: 1,
        };
        let glob = fs.glob(&scope("tenant-a"), &glob_request).unwrap();
        assert_eq!(glob.paths, vec!["a.md"]);
        assert!(glob.truncated);

        let grep_request = VfsGrepRequest {
            pattern: "needle".into(),
            path: "docs/../docs".into(),
            file_glob: "*.md".into(),
            context: Some(1),
            max_files: 10,
            max_results: 1,
        };
        let grep = fs.grep(&scope("tenant-a"), &grep_request).unwrap();
        assert_eq!(grep.match_count, 1);
        assert!(grep.truncated_results);
        assert!(grep.entries.iter().all(|entry| match entry {
            mink::runtime::VfsGrepEntry::Separator => true,
            mink::runtime::VfsGrepEntry::Line { path, .. } => {
                path.starts_with("docs/") && !path.contains("private")
            }
        }));
        std::fs::remove_file(path).ok();
    }
}
