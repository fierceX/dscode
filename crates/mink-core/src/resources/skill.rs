use crate::capabilities::CapabilityExposure;
use crate::capabilities::SourceLevel;
use crate::context::ToolContext;
use crate::resources::router::{Resource, ResourceHandler, ResourceRequest};
use anyhow::{Result, anyhow, bail};
use std::path::{Component, Path, PathBuf};

pub struct SkillResourceHandler;

impl ResourceHandler for SkillResourceHandler {
    fn scheme(&self) -> &'static str {
        "skill"
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource> {
        resolve_skill_resource(&req.resource_url, ctx).map(|resolved| Resource {
            canonical_url: req.resource_url.clone(),
            content: resolved.content,
        })
    }
}

struct ResolvedSkillResource {
    content: String,
}

fn resolve_skill_resource(url: &str, ctx: &ToolContext) -> Result<ResolvedSkillResource> {
    let rest = url
        .strip_prefix("skill://")
        .ok_or_else(|| anyhow!("Error: invalid skill resource: {url}"))?
        .trim_end_matches('/');
    if rest.starts_with('/') {
        bail!("Error: invalid skill resource: {url}");
    }
    if rest.is_empty() || rest == "list" {
        return Ok(ResolvedSkillResource {
            content: render_discoverable_skill_list(ctx),
        });
    }
    if rest == "list/all" || rest == "all" {
        return Ok(ResolvedSkillResource {
            content: render_all_skill_list(ctx),
        });
    }
    let (name_raw, relative_raw) = rest
        .split_once('/')
        .map_or((rest, None), |(name, relative)| (name, Some(relative)));
    let name = percent_decode_component(name_raw)?;
    let loaded = ctx
        .capability_snapshot
        .skills
        .by_name
        .get(&name)
        .ok_or_else(|| anyhow!("Error: skill not found: {name}"))?;
    if matches!(loaded.exposure, CapabilityExposure::HostOnly) {
        bail!("Error: skill is host-only and cannot be read: {name}");
    }
    if let Some(relative_raw) = relative_raw {
        let relative = validate_skill_relative_path(relative_raw)?;
        if !matches!(
            loaded.source.level,
            SourceLevel::Project | SourceLevel::User
        ) || loaded.source.source_path.is_none()
        {
            bail!(
                "Error: skill subresources are only available for filesystem-backed skills: {url}"
            );
        }
        let base = Path::new(&loaded.skill.base_dir)
            .canonicalize()
            .map_err(|e| anyhow!("Error: skill base directory is not readable: {e}"))?;
        let target = base
            .join(&relative)
            .canonicalize()
            .map_err(|e| anyhow!("Error: skill resource not found: {url}: {e}"))?;
        if target != base && !target.starts_with(&base) {
            bail!("Error: skill resource path escapes skill directory: {url}");
        }
        if !target.is_file() {
            bail!("Error: skill resource is not a file: {url}");
        }
        let content = std::fs::read_to_string(&target)
            .map_err(|e| anyhow!("Error: skill resource is not valid UTF-8 text: {url}: {e}"))?;
        let rendered_path = relative.to_string_lossy().replace('\\', "/");
        return Ok(ResolvedSkillResource {
            content: format!(
                "# skill://{}/{}\n\nSource: {}\nContent-Type: {}\n\n{}",
                loaded.skill.name,
                rendered_path,
                loaded.source.model_display_label(),
                content_type_label(&target),
                content
            ),
        });
    }
    Ok(ResolvedSkillResource {
        content: format!(
            "# skill://{}\n\nDescription: {}\nBase directory: {}\n\n{}",
            loaded.skill.name,
            loaded.skill.description,
            loaded.skill.base_dir,
            loaded.skill.content
        ),
    })
}

fn render_discoverable_skill_list(ctx: &ToolContext) -> String {
    format!(
        "# Skills\n{}",
        ctx.capability_snapshot.skills.format_discoverable_skills()
    )
}

fn render_all_skill_list(ctx: &ToolContext) -> String {
    let mut out = String::from("# Skills\n\n");
    out.push_str("## Discoverable\n");
    let discoverable = ctx
        .capability_snapshot
        .skills
        .discoverable
        .iter()
        .filter(|skill| !matches!(skill.exposure, CapabilityExposure::HostOnly))
        .collect::<Vec<_>>();
    if discoverable.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for skill in discoverable {
            out.push_str(&format_diagnostic_skill_line(skill));
        }
    }

    out.push_str("\n## Addressable\n");
    let addressable = ctx
        .capability_snapshot
        .skills
        .all
        .iter()
        .filter(|skill| matches!(skill.exposure, CapabilityExposure::ModelAddressable))
        .collect::<Vec<_>>();
    if addressable.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for skill in addressable {
            out.push_str(&format_diagnostic_skill_line(skill));
        }
    }

    out.push_str("\n## Selected\n");
    if ctx.capability_snapshot.skills.selected.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for selected in &ctx.capability_snapshot.skills.selected {
            if let Some(loaded) = ctx
                .capability_snapshot
                .skills
                .by_name
                .get(&selected.info.name)
                && !matches!(loaded.exposure, CapabilityExposure::HostOnly)
            {
                out.push_str(&format!(
                    "- {} [{}]: {}\n",
                    selected.info.name,
                    loaded.source.model_display_label(),
                    selected.info.description
                ));
            }
        }
    }

    out.push_str("\n## Warnings\n");
    if ctx.capability_snapshot.skills.warnings.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for warning in &ctx.capability_snapshot.skills.warnings {
            out.push_str(&format!("- {}: {}\n", warning.provider_id, warning.message));
        }
    }

    out
}

fn format_diagnostic_skill_line(skill: &crate::capabilities::LoadedSkill) -> String {
    format!(
        "- {} [{}, {}]: {}\n",
        skill.skill.name,
        skill.source.model_display_label(),
        crate::capabilities::source::exposure_label(&skill.exposure),
        skill.skill.description
    )
}

fn validate_skill_relative_path(raw: &str) -> Result<PathBuf> {
    let decoded = percent_decode_component(raw)?;
    if decoded.is_empty() {
        bail!("Error: skill resource path cannot be empty");
    }
    let path = Path::new(&decoded);
    if path.is_absolute() {
        bail!("Error: skill resource path must be relative: {decoded}");
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("Error: skill resource path escapes skill directory: {decoded}");
            }
            Component::Normal(_) => {}
            Component::CurDir => {}
        }
    }
    Ok(path.to_path_buf())
}

fn percent_decode_component(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'%' => {
                if idx + 2 >= bytes.len() {
                    bail!("Error: invalid percent encoding in skill resource: {input}");
                }
                let h = hex_value(bytes[idx + 1]).ok_or_else(|| {
                    anyhow!("Error: invalid percent encoding in skill resource: {input}")
                })?;
                let l = hex_value(bytes[idx + 2]).ok_or_else(|| {
                    anyhow!("Error: invalid percent encoding in skill resource: {input}")
                })?;
                out.push((h << 4) | l);
                idx += 3;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out)
        .map_err(|_| anyhow!("Error: invalid UTF-8 percent encoding in skill resource: {input}"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn content_type_label(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        _ => "text/plain",
    }
}
