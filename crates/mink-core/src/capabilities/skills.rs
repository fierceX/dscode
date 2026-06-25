use crate::capabilities::CapabilityWarning;
use crate::capabilities::source::{CapabilityExposure, SourceLevel, SourceMeta};
use crate::skills::{ResolvedSkill, SkillInfo, SkillSource};
use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct LoadContext<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub session_id: &'a str,
    pub resource_session_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct SkillCapability {
    pub name: String,
    pub description: String,
    pub content: String,
    pub base_dir: String,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub skill: SkillCapability,
    pub source: SourceMeta,
    pub exposure: CapabilityExposure,
    pub revision: String,
}

pub trait SkillProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn load_skills(&self, ctx: &LoadContext<'_>) -> Result<Vec<LoadedSkill>>;
}

#[derive(Debug, Clone, Default)]
pub struct SkillSnapshot {
    pub all: Vec<LoadedSkill>,
    pub discoverable: Vec<LoadedSkill>,
    pub selected: Vec<ResolvedSkill>,
    pub by_name: BTreeMap<String, LoadedSkill>,
    pub warnings: Vec<CapabilityWarning>,
    pub dependency_fingerprint: String,
}

pub fn build_default_skill_snapshot(
    cwd: &Path,
    home: &Path,
    session_id: &str,
    resource_session_id: &str,
    selected_skills: &[String],
) -> Result<SkillSnapshot> {
    let providers = default_skill_providers();
    SkillSnapshot::load(
        &providers,
        &LoadContext {
            cwd,
            home,
            session_id,
            resource_session_id,
        },
        selected_skills,
    )
}

pub fn default_skill_providers() -> Vec<Box<dyn SkillProvider>> {
    vec![
        Box::new(RuntimeSkillProvider::default()),
        Box::new(FileSystemSkillProvider::new(
            "project-claude-skills",
            "project .claude/skills",
            SourceLevel::Project,
            220,
            SkillBase::ProjectClaude,
        )),
        Box::new(FileSystemSkillProvider::new(
            "project-local-skills",
            "project skills",
            SourceLevel::Project,
            210,
            SkillBase::ProjectLocal,
        )),
        Box::new(FileSystemSkillProvider::new(
            "user-claude-skills",
            "user .claude/skills",
            SourceLevel::User,
            120,
            SkillBase::UserClaude,
        )),
        Box::new(BuiltInSkillProvider),
    ]
}

impl SkillSnapshot {
    pub fn load(
        providers: &[Box<dyn SkillProvider>],
        ctx: &LoadContext<'_>,
        selected_skills: &[String],
    ) -> Result<Self> {
        let mut order: Vec<usize> = (0..providers.len()).collect();
        order.sort_by(|a, b| {
            providers[*b]
                .priority()
                .cmp(&providers[*a].priority())
                .then_with(|| a.cmp(b))
        });

        let mut all = Vec::new();
        let mut by_name = BTreeMap::new();
        let mut seen_realpaths = BTreeSet::new();
        let mut warnings = Vec::new();

        for idx in order {
            let provider = &providers[idx];
            let mut loaded = provider.load_skills(ctx).map_err(|e| {
                anyhow!(
                    "Error: skill provider {} failed to load skills: {e}",
                    provider.id()
                )
            })?;
            loaded.sort_by(|a, b| a.skill.name.cmp(&b.skill.name));
            for skill in loaded {
                if let Some(path) = &skill.source.source_path
                    && let Ok(realpath) = path.canonicalize()
                    && !seen_realpaths.insert(realpath)
                {
                    continue;
                }
                if by_name.contains_key(&skill.skill.name) {
                    continue;
                }
                if matches!(skill.exposure, CapabilityExposure::HostOnly) {
                    warnings.push(CapabilityWarning {
                        provider_id: provider.id().to_string(),
                        message: format!("host-only skill '{}' is hidden", skill.skill.name),
                    });
                }
                by_name.insert(skill.skill.name.clone(), skill.clone());
                all.push(skill);
            }
        }

        let discoverable = all
            .iter()
            .filter(|skill| matches!(skill.exposure, CapabilityExposure::ModelDiscoverable))
            .cloned()
            .collect::<Vec<_>>();
        let mut selected = Vec::new();
        for raw_name in selected_skills {
            let name = raw_name.trim();
            let loaded = by_name
                .get(name)
                .ok_or_else(|| anyhow!("Error: skill not found: {name}"))?;
            if matches!(loaded.exposure, CapabilityExposure::HostOnly) {
                bail!("Error: skill is host-only and cannot be selected: {name}");
            }
            selected.push(to_resolved_skill(loaded));
        }

        let dependency_fingerprint =
            compute_dependency_fingerprint(&discoverable, &selected, &by_name);
        Ok(Self {
            all,
            discoverable,
            selected,
            by_name,
            warnings,
            dependency_fingerprint,
        })
    }
}

#[derive(Default)]
pub struct RuntimeSkillProvider {
    skills: Vec<LoadedSkill>,
}

impl RuntimeSkillProvider {
    #[allow(dead_code)]
    pub fn new(skills: Vec<LoadedSkill>) -> Self {
        Self { skills }
    }
}

impl SkillProvider for RuntimeSkillProvider {
    fn id(&self) -> &'static str {
        "runtime-skills"
    }

    fn display_name(&self) -> &'static str {
        "runtime skills"
    }

    fn priority(&self) -> i32 {
        300
    }

    fn load_skills(&self, _ctx: &LoadContext<'_>) -> Result<Vec<LoadedSkill>> {
        Ok(self.skills.clone())
    }
}

#[derive(Clone, Copy)]
enum SkillBase {
    ProjectClaude,
    ProjectLocal,
    UserClaude,
}

struct FileSystemSkillProvider {
    id: &'static str,
    display_name: &'static str,
    level: SourceLevel,
    priority: i32,
    base: SkillBase,
}

impl FileSystemSkillProvider {
    fn new(
        id: &'static str,
        display_name: &'static str,
        level: SourceLevel,
        priority: i32,
        base: SkillBase,
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
            SkillBase::ProjectClaude => ctx.cwd.join(".claude/skills"),
            SkillBase::ProjectLocal => ctx.cwd.join("skills"),
            SkillBase::UserClaude => ctx.home.join(".claude/skills"),
        }
    }
}

impl SkillProvider for FileSystemSkillProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn load_skills(&self, ctx: &LoadContext<'_>) -> Result<Vec<LoadedSkill>> {
        let base = self.base_dir(ctx);
        let Ok(entries) = fs::read_dir(&base) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_skill_name(&name) {
                continue;
            }
            let skill_file = entry.path().join("SKILL.md");
            let Ok(raw_content) = fs::read_to_string(&skill_file) else {
                continue;
            };
            let base_dir = skill_file
                .parent()
                .unwrap_or(Path::new(""))
                .display()
                .to_string();
            let content = raw_content.replace("${MINK_SKILL_DIR}", &base_dir);
            let frontmatter = parse_frontmatter(&content);
            let disable_model_invocation = frontmatter.disable_model_invocation;
            let exposure = if frontmatter.hide || disable_model_invocation {
                CapabilityExposure::ModelAddressable
            } else {
                CapabilityExposure::ModelDiscoverable
            };
            out.push(LoadedSkill {
                revision: sha256_hex(&content),
                skill: SkillCapability {
                    name,
                    description: frontmatter
                        .description
                        .unwrap_or_else(|| crate::skills::extract_skill_summary(&content)),
                    content,
                    base_dir,
                    disable_model_invocation,
                },
                source: SourceMeta {
                    provider_id: self.id.to_string(),
                    provider_name: self.display_name.to_string(),
                    level: self.level.clone(),
                    source_path: Some(skill_file),
                    display_label: Some(display_label(&base, ctx.cwd, ctx.home)),
                },
                exposure,
            });
        }
        Ok(out)
    }
}

struct BuiltInSkillProvider;

impl SkillProvider for BuiltInSkillProvider {
    fn id(&self) -> &'static str {
        "built-in-skills"
    }

    fn display_name(&self) -> &'static str {
        "built-in skills"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn load_skills(&self, _ctx: &LoadContext<'_>) -> Result<Vec<LoadedSkill>> {
        Ok(crate::assets::embedded_skills::all()
            .into_iter()
            .map(|skill| {
                let content = skill.content.replace("${MINK_SKILL_DIR}", "<built-in>");
                LoadedSkill {
                    revision: sha256_hex(&content),
                    skill: SkillCapability {
                        name: skill.name.to_string(),
                        description: skill.description.to_string(),
                        content,
                        base_dir: "<built-in>".to_string(),
                        disable_model_invocation: false,
                    },
                    source: SourceMeta {
                        provider_id: self.id().to_string(),
                        provider_name: self.display_name().to_string(),
                        level: SourceLevel::BuiltIn,
                        source_path: None,
                        display_label: Some("built-in".to_string()),
                    },
                    exposure: CapabilityExposure::ModelDiscoverable,
                }
            })
            .collect())
    }
}

#[derive(Default)]
struct Frontmatter {
    description: Option<String>,
    hide: bool,
    disable_model_invocation: bool,
}

fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut out = Frontmatter::default();
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return out;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = trim_yaml_scalar(value);
            if !value.is_empty() {
                out.description = Some(value.to_string());
            }
        } else if let Some(value) = trimmed.strip_prefix("hide:") {
            out.hide = parse_bool(value);
        } else if let Some(value) = trimmed.strip_prefix("disable-model-invocation:") {
            out.disable_model_invocation = parse_bool(value);
        }
    }
    out
}

fn parse_bool(value: &str) -> bool {
    matches!(
        trim_yaml_scalar(value).to_ascii_lowercase().as_str(),
        "true" | "yes" | "1"
    )
}

fn trim_yaml_scalar(value: &str) -> &str {
    value.trim().trim_matches('"').trim_matches('\'')
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

fn to_resolved_skill(loaded: &LoadedSkill) -> ResolvedSkill {
    ResolvedSkill {
        info: SkillInfo {
            name: loaded.skill.name.clone(),
            description: loaded.skill.description.clone(),
            source: match loaded.source.level {
                SourceLevel::BuiltIn => SkillSource::BuiltIn,
                _ => SkillSource::FileSystem,
            },
            base_dir: loaded.skill.base_dir.clone(),
        },
        content: loaded.skill.content.clone(),
    }
}

fn compute_dependency_fingerprint(
    discoverable: &[LoadedSkill],
    selected: &[ResolvedSkill],
    by_name: &BTreeMap<String, LoadedSkill>,
) -> String {
    let mut input = String::new();
    for skill in discoverable {
        input.push_str("discoverable\0");
        input.push_str(&skill.skill.name);
        input.push('\0');
        input.push_str(&skill.skill.description);
        input.push('\0');
        input.push_str(exposure_label(&skill.exposure));
        input.push('\0');
        input.push_str(&skill.source.provider_id);
        input.push('\0');
        input.push_str(&skill.revision);
        input.push('\0');
    }
    for skill in selected {
        input.push_str("selected\0");
        input.push_str(&skill.info.name);
        input.push('\0');
        if let Some(loaded) = by_name.get(&skill.info.name) {
            input.push_str(&loaded.revision);
        }
        input.push('\0');
        input.push_str(&sha256_hex(&skill.content));
        input.push('\0');
    }
    sha256_hex(&input)
}

fn exposure_label(exposure: &CapabilityExposure) -> &'static str {
    match exposure {
        CapabilityExposure::ModelDiscoverable => "model-discoverable",
        CapabilityExposure::ModelAddressable => "model-addressable",
        CapabilityExposure::HostOnly => "host-only",
    }
}

fn sha256_hex(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mink-cap-skills-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn write_skill(root: &Path, relative: &str, content: &str) {
        let dir = root.join(relative);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn skill_snapshot_prefers_project_over_builtin() {
        let root = temp_root("override");
        let home = root.join("home");
        let cwd = root.join("workspace");
        write_skill(
            &cwd,
            ".claude/skills/debugging",
            "---\ndescription: \"Local debugging\"\n---\n\nlocal body",
        );

        let snapshot =
            build_default_skill_snapshot(&cwd, &home, "session-1", "session-1", &[]).unwrap();

        let loaded = snapshot.by_name.get("debugging").unwrap();
        assert_eq!(loaded.skill.description, "Local debugging");
        assert!(loaded.skill.content.contains("local body"));
        assert_eq!(loaded.source.level, SourceLevel::Project);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skill_snapshot_model_addressable_hidden_from_index() {
        let root = temp_root("hidden");
        let home = root.join("home");
        let cwd = root.join("workspace");
        write_skill(
            &cwd,
            "skills/hidden-review",
            "---\ndescription: \"Hidden review\"\nhide: true\n---\n\nbody",
        );

        let snapshot =
            build_default_skill_snapshot(&cwd, &home, "session-1", "session-1", &[]).unwrap();

        assert!(snapshot.by_name.contains_key("hidden-review"));
        assert!(
            !snapshot
                .discoverable
                .iter()
                .any(|skill| skill.skill.name == "hidden-review")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn selected_model_addressable_skill_enters_prompt() {
        let root = temp_root("selected-hidden");
        let home = root.join("home");
        let cwd = root.join("workspace");
        write_skill(
            &cwd,
            "skills/hidden-review",
            "---\ndescription: \"Hidden review\"\ndisable-model-invocation: true\n---\n\nbody",
        );

        let snapshot = build_default_skill_snapshot(
            &cwd,
            &home,
            "session-1",
            "session-1",
            &["hidden-review".to_string()],
        )
        .unwrap();

        assert_eq!(snapshot.selected.len(), 1);
        assert_eq!(snapshot.selected[0].info.name, "hidden-review");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skill_snapshot_dependency_fingerprint_changes_on_content_change() {
        let root = temp_root("fingerprint");
        let home = root.join("home");
        let cwd = root.join("workspace");
        write_skill(
            &cwd,
            "skills/local-review",
            "---\ndescription: \"Local review\"\n---\n\nbody 1",
        );
        let first =
            build_default_skill_snapshot(&cwd, &home, "session-1", "session-1", &[]).unwrap();
        write_skill(
            &cwd,
            "skills/local-review",
            "---\ndescription: \"Local review\"\n---\n\nbody 2",
        );
        let second =
            build_default_skill_snapshot(&cwd, &home, "session-1", "session-1", &[]).unwrap();

        assert_ne!(first.dependency_fingerprint, second.dependency_fingerprint);
        let _ = fs::remove_dir_all(root);
    }
}
