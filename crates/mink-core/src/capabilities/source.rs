use std::path::{Path, PathBuf};

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

pub(crate) fn display_label(path: &Path, cwd: &Path, home: &Path) -> String {
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

pub(crate) fn exposure_label(exposure: &CapabilityExposure) -> &'static str {
    match exposure {
        CapabilityExposure::ModelDiscoverable => "model-discoverable",
        CapabilityExposure::ModelAddressable => "model-addressable",
        CapabilityExposure::HostOnly => "host-only",
    }
}

pub(crate) fn is_valid_skill_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed == name
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}
