use crate::capabilities::source::{CapabilityExposure, SourceLevel, SourceMeta};
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct LoadContext<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub session_id: &'a str,
    pub resource_session_id: &'a str,
}

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

pub trait ContextFileProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn load_context_files(&self, ctx: &LoadContext<'_>) -> Result<Vec<LoadedContextFile>>;
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
    session_id: &str,
    resource_session_id: &str,
) -> Result<ContextFileSnapshot> {
    let providers = default_context_file_providers();
    ContextFileSnapshot::load(
        &providers,
        &LoadContext {
            cwd,
            home,
            session_id,
            resource_session_id,
        },
    )
}

pub fn default_context_file_providers() -> Vec<Arc<dyn ContextFileProvider>> {
    vec![
        Arc::new(InstructionFileProvider::new(
            "user-instructions",
            "user instructions",
            SourceLevel::User,
            200,
            InstructionBase::UserMink,
        )),
        Arc::new(InstructionFileProvider::new(
            "project-instructions",
            "project instructions",
            SourceLevel::Project,
            100,
            InstructionBase::Project,
        )),
    ]
}

impl ContextFileSnapshot {
    pub fn load(providers: &[Arc<dyn ContextFileProvider>], ctx: &LoadContext<'_>) -> Result<Self> {
        let mut order: Vec<usize> = (0..providers.len()).collect();
        order.sort_by(|a, b| {
            providers[*b]
                .priority()
                .cmp(&providers[*a].priority())
                .then_with(|| a.cmp(b))
        });

        let mut all = Vec::new();
        for idx in order {
            let provider = &providers[idx];
            let mut loaded = provider.load_context_files(ctx).map_err(|e| {
                anyhow!(
                    "Error: context file provider {} failed to load files: {e}",
                    provider.id()
                )
            })?;
            loaded.sort_by(|a, b| a.context_file.name.cmp(&b.context_file.name));
            all.extend(loaded);
        }
        let always_apply = all
            .iter()
            .filter(|file| !matches!(file.exposure, CapabilityExposure::HostOnly))
            .cloned()
            .collect::<Vec<_>>();
        let dependency_fingerprint = compute_dependency_fingerprint(&always_apply);
        Ok(Self {
            all,
            always_apply,
            warnings: Vec::new(),
            dependency_fingerprint,
        })
    }
}

#[derive(Clone, Copy)]
enum InstructionBase {
    Project,
    UserMink,
}

struct InstructionFileProvider {
    id: &'static str,
    display_name: &'static str,
    level: SourceLevel,
    priority: i32,
    base: InstructionBase,
}

impl InstructionFileProvider {
    fn new(
        id: &'static str,
        display_name: &'static str,
        level: SourceLevel,
        priority: i32,
        base: InstructionBase,
    ) -> Self {
        Self {
            id,
            display_name,
            level,
            priority,
            base,
        }
    }

    fn base_dir(&self, ctx: &LoadContext<'_>) -> PathBuf {
        match self.base {
            InstructionBase::Project => ctx.cwd.to_path_buf(),
            InstructionBase::UserMink => ctx.home.join(".mink"),
        }
    }
}

impl ContextFileProvider for InstructionFileProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn load_context_files(&self, ctx: &LoadContext<'_>) -> Result<Vec<LoadedContextFile>> {
        let base = self.base_dir(ctx);
        let Some(path) = find_instruction_file_in_dir(&base) else {
            return Ok(Vec::new());
        };
        let content = std::fs::read_to_string(&path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        let name = match self.base {
            InstructionBase::Project => "project",
            InstructionBase::UserMink => "global",
        }
        .to_string();
        Ok(vec![LoadedContextFile {
            revision: crate::util::sha256_hex(&content),
            context_file: ContextFileCapability { name, content },
            source: SourceMeta {
                provider_id: self.id.to_string(),
                provider_name: self.display_name.to_string(),
                level: self.level.clone(),
                source_path: Some(path),
                display_label: Some(display_label(&base, ctx.cwd, ctx.home)),
            },
            exposure: CapabilityExposure::ModelDiscoverable,
        }])
    }
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
    crate::util::sha256_hex(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mink-cap-context-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn context_file_snapshot_loads_project_and_user_files() {
        let root = temp_root("load");
        let home = root.join("home");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(home.join(".mink")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(home.join(".mink/AGENTS.md"), "global instructions").unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "project instructions").unwrap();

        let snapshot =
            build_default_context_file_snapshot(&cwd, &home, "session", "session").unwrap();

        assert_eq!(snapshot.always_apply.len(), 2);
        assert!(
            snapshot
                .always_apply
                .iter()
                .any(|file| file.context_file.name == "project")
        );
        assert!(
            snapshot
                .always_apply
                .iter()
                .any(|file| file.context_file.name == "global")
        );
        assert_eq!(snapshot.always_apply[0].context_file.name, "global");
        assert_eq!(snapshot.always_apply[1].context_file.name, "project");
        let _ = std::fs::remove_dir_all(root);
    }
}
