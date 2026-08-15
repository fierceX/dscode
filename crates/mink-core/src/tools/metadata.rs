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

/// Stable failure classification for tool executions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailureKind {
    /// 执行超时（Bash/Python/工具 deadline）。
    Timeout,
    /// 引用的快照 tag 已过期或不可复用。
    StaleTag,
    /// 匹配目标不唯一（锚点/replace 多候选）。
    AmbiguousMatch,
    /// 路径越权或超出允许范围。
    PathOutOfScope,
    /// 被 bash/python 安全策略拦截。
    SafetyBlocked,
    /// 参数缺失或非法。
    ArgumentInvalid,
    /// 进程非零退出。
    ProcessFailed,
    /// 被用户或运行时中断。
    Aborted,
    /// 无法归类到稳定码的失败。
    Unknown,
}

impl ToolFailureKind {
    /// 硬错误 = 确定性失败（超时/进程失败/安全拦截/中断），单独出现即参与决策。
    pub fn is_hard(&self) -> bool {
        matches!(
            self,
            ToolFailureKind::Timeout
                | ToolFailureKind::ProcessFailed
                | ToolFailureKind::SafetyBlocked
                | ToolFailureKind::Aborted
        )
    }
}

impl ToolFailureKind {
    /// 模型可见的稳定标签。
    pub fn label(&self) -> &'static str {
        match self {
            ToolFailureKind::Timeout => "Timeout",
            ToolFailureKind::StaleTag => "StaleTag",
            ToolFailureKind::AmbiguousMatch => "AmbiguousMatch",
            ToolFailureKind::PathOutOfScope => "PathOutOfScope",
            ToolFailureKind::SafetyBlocked => "SafetyBlocked",
            ToolFailureKind::ArgumentInvalid => "ArgumentInvalid",
            ToolFailureKind::ProcessFailed => "ProcessFailed",
            ToolFailureKind::Aborted => "Aborted",
            ToolFailureKind::Unknown => "Unknown",
        }
    }
}

/// 从失败结果文本 + 退出码分类出稳定错误码。
///
/// 兜底分类只服务展示与统计；确定性错误码应尽早由执行路径显式携带
/// （见 `ToolExecution.error_code`），避免依赖文本嗅探。
pub fn classify_failure_kind(content: &str, exit_code: Option<i32>) -> ToolFailureKind {
    if let Some(code) = exit_code
        && code != 0
    {
        return ToolFailureKind::ProcessFailed;
    }
    let lower = content.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        return ToolFailureKind::Timeout;
    }
    if lower.contains("safety policy") || lower.contains("blocked by bash") {
        return ToolFailureKind::SafetyBlocked;
    }
    if lower.contains("stale") || lower.contains("invalid tag") {
        return ToolFailureKind::StaleTag;
    }
    if lower.contains("not unique")
        || lower.contains("ambiguous")
        || lower.contains("multiple matches")
    {
        return ToolFailureKind::AmbiguousMatch;
    }
    if lower.contains("outside")
        || lower.contains("beyond allowed")
        || lower.contains("permission denied")
    {
        return ToolFailureKind::PathOutOfScope;
    }
    if lower.contains("no command provided")
        || lower.contains("no path provided")
        || lower.contains("invalid todo status")
        || lower.contains("invalid argument")
    {
        return ToolFailureKind::ArgumentInvalid;
    }
    if lower.contains("interrupted") || lower.contains("aborted") {
        return ToolFailureKind::Aborted;
    }
    ToolFailureKind::Unknown
}

/// Runtime reason for preventing an otherwise valid tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolBlocker {
    RecoveryGuard,
    ToolSurface,
    StormBreaker,
}

/// Authoritative execution state. Display text is never used to recover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum ToolStatus {
    Succeeded,
    Failed(ToolFailureKind),
    Blocked(ToolBlocker),
    Interrupted,
}

impl ToolStatus {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }

    pub fn failure_kind(self) -> Option<ToolFailureKind> {
        match self {
            Self::Failed(kind) => Some(kind),
            Self::Interrupted => Some(ToolFailureKind::Aborted),
            Self::Succeeded | Self::Blocked(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub name: std::borrow::Cow<'static, str>,
    pub summary: std::borrow::Cow<'static, str>,
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
            name: std::borrow::Cow::Borrowed(name),
            summary: std::borrow::Cow::Borrowed(summary),
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

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
