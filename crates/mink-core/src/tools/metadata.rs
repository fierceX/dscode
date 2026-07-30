#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTier {
    Read,
    Write,
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultKind {
    Text,
    FileRead,
    FileWrite,
    Edit,
    Command,
    Search,
    Control,
    SubAgent,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolMetadata {
    pub name: &'static str,
    pub summary: &'static str,
    pub approval: ApprovalTier,
    pub result_kind: ToolResultKind,
    pub mutating: bool,
    pub storm_exempt: bool,
    pub internal: bool,
    pub discoverable: bool,
    pub spawns_sub_agent: bool,
}

impl ToolMetadata {
    pub const fn new(
        name: &'static str,
        summary: &'static str,
        approval: ApprovalTier,
        result_kind: ToolResultKind,
    ) -> Self {
        Self {
            name,
            summary,
            approval,
            result_kind,
            mutating: false,
            storm_exempt: false,
            internal: false,
            discoverable: false,
            spawns_sub_agent: false,
        }
    }

    pub const fn mutating(mut self) -> Self {
        self.mutating = true;
        self
    }

    pub const fn storm_exempt(mut self) -> Self {
        self.storm_exempt = true;
        self
    }

    pub const fn internal(mut self) -> Self {
        self.internal = true;
        self
    }

    pub const fn discoverable(mut self) -> Self {
        self.discoverable = true;
        self
    }

    pub const fn spawns_sub_agent(mut self) -> Self {
        self.spawns_sub_agent = true;
        self
    }
}
