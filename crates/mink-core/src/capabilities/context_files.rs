use crate::capabilities::source::{CapabilityExposure, SourceLevel, SourceMeta};
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ContextFileCapability {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct LoadedContextFile {
    pub context_file: ContextFileCapability,
    pub source: SourceMeta,
    pub exposure: CapabilityExposure,
    pub revision: String,
}

#[derive(Debug, Clone, Default)]
pub struct ContextFileSnapshot {
    pub all: Vec<LoadedContextFile>,
    pub always_apply: Vec<LoadedContextFile>,
    pub warnings: Vec<crate::capabilities::CapabilityWarning>,
    pub dependency_fingerprint: String,
}

pub fn build_default_context_file_snapshot(
    cwd: &Path,
    home: &Path,
    _session_id: &str,
    _resource_session_id: &str,
) -> Result<ContextFileSnapshot> {
    let mut all = Vec::new();
    for (base, name, provider_id, provider_name, level) in [
        (
            home.join(".mink"),
            "global",
            "user-instructions",
            "user instructions",
            SourceLevel::User,
        ),
        (
            cwd.to_path_buf(),
            "project",
            "project-instructions",
            "project instructions",
            SourceLevel::Project,
        ),
    ] {
        let Some(path) = find_instruction_file_in_dir(&base) else {
            continue;
        };
        let content = std::fs::read_to_string(&path)?;
        if content.trim().is_empty() {
            continue;
        }
        all.push(LoadedContextFile {
            revision: crate::capabilities::fingerprint::sha256_hex(&content),
            context_file: ContextFileCapability {
                name: name.to_string(),
                content,
            },
            source: SourceMeta {
                provider_id: provider_id.to_string(),
                provider_name: provider_name.to_string(),
                level,
                source_path: Some(path),
                display_label: Some(display_label(&base, cwd, home)),
            },
            exposure: CapabilityExposure::ModelDiscoverable,
        });
    }
    let always_apply = all
        .iter()
        .filter(|file| !matches!(file.exposure, CapabilityExposure::HostOnly))
        .cloned()
        .collect::<Vec<_>>();
    let dependency_fingerprint = compute_dependency_fingerprint(&always_apply);
    Ok(ContextFileSnapshot {
        all,
        always_apply,
        warnings: Vec::new(),
        dependency_fingerprint,
    })
}

fn find_instruction_file_in_dir(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        dir.join("AGENTS.md"),
        dir.join("AGENT.md"),
        dir.join("CLAUDE.md"),
        dir.join(".claude/CLAUDE.md"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn display_label(path: &Path, cwd: &Path, home: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(cwd) {
        let rendered = relative.display().to_string();
        if rendered.is_empty() {
            ".".to_string()
        } else {
            rendered
        }
    } else if let Ok(relative) = path.strip_prefix(home) {
        let rendered = relative.display().to_string();
        if rendered.is_empty() {
            "~".to_string()
        } else {
            format!("~/{rendered}")
        }
    } else {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    }
}

fn compute_dependency_fingerprint(always_apply: &[LoadedContextFile]) -> String {
    let mut input = String::new();
    for file in always_apply {
        input.push_str("context-file\0");
        input.push_str(&file.context_file.name);
        input.push('\0');
        input.push_str(&file.source.provider_id);
        input.push('\0');
        input.push_str(&file.revision);
        input.push('\0');
    }
    crate::capabilities::fingerprint::sha256_hex(&input)
}

#[cfg(test)]
#[path = "context_files_tests.rs"]
mod tests;
