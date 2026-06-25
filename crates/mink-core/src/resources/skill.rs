use crate::capabilities::CapabilityExposure;
use crate::context::ToolContext;
use crate::resources::router::{
    Resource, ResourceContentType, ResourceHandler, ResourceMetadata, ResourceRequest,
};
use anyhow::{Result, anyhow, bail};

pub struct SkillResourceHandler;

impl ResourceHandler for SkillResourceHandler {
    fn scheme(&self) -> &'static str {
        "skill"
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource> {
        let content = read_skill_resource(&req.resource_url, ctx)?;
        Ok(Resource {
            canonical_url: req.resource_url.clone(),
            content_type: ResourceContentType::Markdown,
            immutable: Some(true),
            metadata: ResourceMetadata::default(),
            content,
        })
    }
}

pub(crate) fn read_skill_resource(url: &str, ctx: &ToolContext) -> Result<String> {
    let rest = url
        .strip_prefix("skill://")
        .ok_or_else(|| anyhow!("Error: invalid skill resource: {url}"))?
        .trim_matches('/');
    if rest.is_empty() || rest == "list" || rest == "all" {
        let mut out = String::from("# Skills\n");
        for skill in &ctx.capability_snapshot.skills.discoverable {
            let source = skill.source.model_display_label();
            out.push_str(&format!(
                "- {} [{}]: {}\n",
                skill.skill.name, source, skill.skill.description
            ));
        }
        return Ok(out);
    }
    let loaded = ctx
        .capability_snapshot
        .skills
        .by_name
        .get(rest)
        .ok_or_else(|| anyhow!("Error: skill not found: {rest}"))?;
    if matches!(loaded.exposure, CapabilityExposure::HostOnly) {
        bail!("Error: skill is host-only and cannot be read: {rest}");
    }
    Ok(format!(
        "# skill://{}\n\nDescription: {}\nBase directory: {}\n\n{}",
        loaded.skill.name, loaded.skill.description, loaded.skill.base_dir, loaded.skill.content
    ))
}
