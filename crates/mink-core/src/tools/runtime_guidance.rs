use crate::tools::surface::ModelToolSurface;
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct RenderedRuntimeGuidance {
    pub content: String,
    pub referenced_tools: BTreeSet<String>,
}

impl RenderedRuntimeGuidance {
    pub fn validate(&self, surface: &ModelToolSurface) -> Result<()> {
        for name in &self.referenced_tools {
            ensure!(
                surface.has(name),
                "runtime guidance referenced inactive tool '{name}'"
            );
        }
        Ok(())
    }
}
