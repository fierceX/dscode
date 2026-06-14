use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    BuiltIn,
    FileSystem,
}

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub base_dir: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub info: SkillInfo,
    pub content: String,
}

pub fn list_available_skills(cwd: &Path, home: &Path) -> Vec<SkillInfo> {
    let mut skills = BTreeMap::new();
    for skill in crate::assets::embedded_skills::all() {
        skills.insert(
            skill.name.to_string(),
            SkillInfo {
                name: skill.name.to_string(),
                description: skill.description.to_string(),
                source: SkillSource::BuiltIn,
                base_dir: "<built-in>".to_string(),
            },
        );
    }

    let mut bases = skill_base_dirs(cwd, home);
    bases.reverse();
    for base in bases {
        let Ok(entries) = fs::read_dir(&base) else {
            continue;
        };
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
            let Ok(content) = fs::read_to_string(&skill_file) else {
                continue;
            };
            skills.insert(
                name.clone(),
                SkillInfo {
                    name,
                    description: extract_skill_summary(&content),
                    source: SkillSource::FileSystem,
                    base_dir: skill_file
                        .parent()
                        .unwrap_or(Path::new(""))
                        .display()
                        .to_string(),
                },
            );
        }
    }

    skills.into_values().collect()
}

pub fn resolve_skill(cwd: &Path, home: &Path, name: &str) -> Result<ResolvedSkill> {
    let name = name.trim();
    if !is_valid_skill_name(name) {
        bail!("Error: invalid skill name: {name}");
    }

    if let Some(skill_file) = resolve_skill_file(cwd, home, name) {
        let content = fs::read_to_string(&skill_file)?;
        let base_dir = skill_file
            .parent()
            .unwrap_or(Path::new(""))
            .display()
            .to_string();
        let content = content.replace("${MINK_SKILL_DIR}", &base_dir);
        return Ok(ResolvedSkill {
            info: SkillInfo {
                name: name.to_string(),
                description: extract_skill_summary(&content),
                source: SkillSource::FileSystem,
                base_dir,
            },
            content,
        });
    }

    let skill = crate::assets::embedded_skills::find(name)
        .ok_or_else(|| anyhow!("Error: skill not found: {name}"))?;
    Ok(ResolvedSkill {
        info: SkillInfo {
            name: skill.name.to_string(),
            description: skill.description.to_string(),
            source: SkillSource::BuiltIn,
            base_dir: "<built-in>".to_string(),
        },
        content: skill.content.replace("${MINK_SKILL_DIR}", "<built-in>"),
    })
}

pub fn resolve_skill_file(cwd: &Path, home: &Path, name: &str) -> Option<PathBuf> {
    if !is_valid_skill_name(name) {
        return None;
    }
    for base in skill_base_dirs(cwd, home) {
        let path = base.join(name).join("SKILL.md");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn skill_base_dirs(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let project = cwd.join(".claude/skills");
    if project.is_dir() {
        out.push(project);
    }
    let project_dev = cwd.join("skills");
    if project_dev.is_dir() {
        out.push(project_dev);
    }
    let global = home.join(".claude/skills");
    if global.is_dir() {
        out.push(global);
    }
    out
}

fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

pub fn extract_skill_summary(content: &str) -> String {
    let mut in_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            if !in_frontmatter {
                in_frontmatter = true;
                continue;
            } else if in_frontmatter {
                break;
            }
        }
        if in_frontmatter && let Some(desc) = trimmed.strip_prefix("description:") {
            return desc.trim().trim_matches('"').to_string();
        }
    }
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "---")
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mink-skills-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn resolves_filesystem_skill_before_builtin() {
        let root = temp_home("override");
        let home = root.join("home");
        let cwd = root.join("workspace");
        let skill_dir = cwd.join(".claude/skills/debugging");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local debugging\"\n---\n\nlocal body",
        )
        .unwrap();

        let skill = resolve_skill(&cwd, &home, "debugging").unwrap();

        assert_eq!(skill.info.source, SkillSource::FileSystem);
        assert_eq!(skill.info.description, "Local debugging");
        assert!(skill.content.contains("local body"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_includes_filesystem_only_skill() {
        let root = temp_home("list");
        let home = root.join("home");
        let cwd = root.join("workspace");
        let skill_dir = cwd.join("skills/local-review");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Local review\"\n---\n\nbody",
        )
        .unwrap();

        let skills = list_available_skills(&cwd, &home);

        assert!(skills.iter().any(|skill| skill.name == "local-review"
            && skill.description == "Local review"
            && skill.source == SkillSource::FileSystem));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_nested_skill_names() {
        assert!(resolve_skill(Path::new("."), Path::new("."), "../x").is_err());
        assert!(resolve_skill_file(Path::new("."), Path::new("."), "x/y").is_none());
    }

    #[test]
    fn extracts_summary_from_frontmatter() {
        assert_eq!(
            extract_skill_summary("---\ndescription: \"Test skill\"\n---\n\nbody"),
            "Test skill"
        );
    }

    #[test]
    fn summary_falls_back_to_first_content_line() {
        assert_eq!(
            extract_skill_summary("\n# Heading\n\nUse this skill for focused checks."),
            "# Heading"
        );
    }
}
