use crate::tools::metadata::{ApprovalTier, ToolResultKind};
use crate::tools::semantic_capabilities::{
    CapabilityAvailability, CapabilityUseScope, ProviderTier, ToolSemanticCapability,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    ParallelReadOnly,
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolActivation {
    Enabled,
    ExplicitOnly,
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub summary: String,
    pub input_schema: Value,
    pub approval: ApprovalTier,
    pub result_kind: ToolResultKind,
    pub execution: ToolExecutionMode,
    pub mutating: bool,
    pub discoverable: bool,
    pub storm_exempt: bool,
    pub activation: ToolActivation,
    pub hard_dependencies: Vec<String>,
    pub semantic_capabilities: Vec<ToolCapabilityOffer>,
}

#[derive(Debug, Clone)]
pub struct ToolCapabilityOffer {
    pub capability: ToolSemanticCapability,
    pub tier: ProviderTier,
    pub priority: u16,
    pub available_if: CapabilityAvailability,
    pub use_scope: CapabilityUseScope,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, summary: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
            input_schema,
            approval: ApprovalTier::Read,
            result_kind: ToolResultKind::Text,
            execution: ToolExecutionMode::ParallelReadOnly,
            mutating: false,
            discoverable: false,
            storm_exempt: false,
            activation: ToolActivation::Enabled,
            hard_dependencies: Vec::new(),
            semantic_capabilities: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct ToolExecutionContext {
    cwd: PathBuf,
    interrupt: Arc<AtomicBool>,
}

impl ToolExecutionContext {
    pub(crate) fn new(cwd: PathBuf, interrupt: Arc<AtomicBool>) -> Self {
        Self { cwd, interrupt }
    }
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub fn is_cancelled(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub conversation_content: Option<String>,
    pub success: bool,
    pub exit_code: Option<i32>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            conversation_content: None,
            success: true,
            exit_code: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolError {
    message: String,
}
impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ToolError {}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(
        &self,
        input: Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Clone)]
pub(crate) struct RegisteredCustomTool {
    pub definition: ToolDefinition,
    pub executor: Arc<dyn AgentTool>,
}

pub(crate) fn freeze_custom_tools(tools: Vec<Arc<dyn AgentTool>>) -> Vec<RegisteredCustomTool> {
    tools
        .into_iter()
        .map(|executor| RegisteredCustomTool {
            definition: executor.definition(),
            executor,
        })
        .collect()
}
