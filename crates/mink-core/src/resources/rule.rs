use crate::capabilities::CapabilityExposure;
use crate::context::ToolContext;
use crate::resources::router::{Resource, ResourceHandler, ResourceRequest};
use anyhow::{Result, anyhow, bail};

pub struct RuleResourceHandler;

impl ResourceHandler for RuleResourceHandler {
    fn scheme(&self) -> &'static str {
        "rule"
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource> {
        let content = read_rule_resource(&req.resource_url, ctx)?;
        Ok(Resource {
            canonical_url: req.resource_url.clone(),
            content,
        })
    }
}

pub(crate) fn read_rule_resource(url: &str, ctx: &ToolContext) -> Result<String> {
    let rest = url
        .strip_prefix("rule://")
        .ok_or_else(|| anyhow!("Error: invalid rule resource: {url}"))?
        .trim_matches('/');
    if rest.is_empty() || rest == "list" || rest == "all" {
        let mut out = String::from("# Rules\n");
        for rule in &ctx.capability_snapshot.rules.discoverable {
            let source = rule.source.model_display_label();
            out.push_str(&format!(
                "- {} [{}]: {}\n",
                rule.rule.name, source, rule.rule.description
            ));
        }
        return Ok(out);
    }
    if rest.contains('/') {
        bail!("Error: invalid rule resource: {url}");
    }
    let loaded = ctx
        .capability_snapshot
        .rules
        .by_name
        .get(rest)
        .ok_or_else(|| anyhow!("Error: rule not found: {rest}"))?;
    if matches!(loaded.exposure, CapabilityExposure::HostOnly) {
        bail!("Error: rule is host-only and cannot be read: {rest}");
    }
    Ok(format!(
        "# rule://{}\n\nDescription: {}\nSource: {}\n\n{}",
        loaded.rule.name,
        loaded.rule.description,
        loaded.source.model_display_label(),
        loaded.rule.content
    ))
}
