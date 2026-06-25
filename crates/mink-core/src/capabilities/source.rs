use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLevel {
    Runtime,
    Project,
    User,
    BuiltIn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityExposure {
    ModelDiscoverable,
    ModelAddressable,
    HostOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMeta {
    pub provider_id: String,
    pub provider_name: String,
    pub level: SourceLevel,
    pub source_path: Option<PathBuf>,
    pub display_label: Option<String>,
}

impl SourceMeta {
    pub fn model_display_label(&self) -> &str {
        self.display_label.as_deref().unwrap_or(&self.provider_name)
    }
}
