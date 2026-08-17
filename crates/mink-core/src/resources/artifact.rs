use crate::context::ToolContext;
use crate::resources::router::{Resource, ResourceHandler, ResourceRequest};
use anyhow::{Result, anyhow};

pub struct ArtifactResourceHandler;

impl ResourceHandler for ArtifactResourceHandler {
    fn scheme(&self) -> &'static str {
        "artifact"
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource> {
        let id = crate::session::artifacts::artifact_id_from_url(&req.resource_url)
            .ok_or_else(|| anyhow!("Error: invalid artifact resource: {}", req.resource_url))?;
        let content = ctx.artifacts.read_text(id)?;
        Ok(Resource {
            canonical_url: format!("artifact://{id}"),
            content,
        })
    }
}
