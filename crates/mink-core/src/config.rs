use crate::llm::client::TokenParamKind;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    StreamJson,
}

/// Runtime-selected wire protocol and matching engine for the `Edit` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EditMode {
    #[default]
    Hashline,
    Replace,
}

impl EditMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hashline" => Ok(Self::Hashline),
            "replace" => Ok(Self::Replace),
            _ => bail!("invalid edit_mode {value:?}; expected 'hashline' or 'replace'"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hashline => "hashline",
            Self::Replace => "replace",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SignalPolicy {
    Off,
    Evidence,
    StateOps,
    Restart,
    #[default]
    Full,
}

impl SignalPolicy {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "evidence" => Ok(Self::Evidence),
            "state_ops" | "state-ops" => Ok(Self::StateOps),
            "restart" => Ok(Self::Restart),
            "full" => Ok(Self::Full),
            _ => bail!(
                "invalid signal policy {value:?}; expected off, evidence, state_ops, restart, or full"
            ),
        }
    }

    pub fn enabled(self) -> bool {
        self != Self::Off
    }

    pub fn allows_state_ops(self) -> bool {
        self >= Self::StateOps
    }

    pub fn allows_restart(self) -> bool {
        self >= Self::Restart
    }

    pub fn allows_handover(self) -> bool {
        self >= Self::Full
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub requested: String,
    pub actual: String,
    pub label: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelResolver {
    aliases: BTreeMap<String, String>,
}

impl ModelResolver {
    pub fn new(custom_aliases: &BTreeMap<String, String>) -> Self {
        let mut aliases = BTreeMap::from([
            ("flash".to_string(), "deepseek-v4-flash".to_string()),
            ("pro".to_string(), "deepseek-v4-pro".to_string()),
        ]);
        for (alias, model) in custom_aliases {
            let alias = alias.trim();
            let model = model.trim();
            if !alias.is_empty() && !model.is_empty() {
                aliases.insert(alias.to_string(), model.to_string());
            }
        }
        Self { aliases }
    }

    pub fn resolve(&self, model: &str) -> ResolvedModel {
        let requested = model.trim();
        let requested = if requested.is_empty() {
            "flash"
        } else {
            requested
        };
        if let Some(actual) = self.aliases.get(requested) {
            return ResolvedModel {
                requested: requested.to_string(),
                actual: actual.clone(),
                label: requested.to_string(),
                alias: Some(requested.to_string()),
            };
        }
        ResolvedModel {
            requested: requested.to_string(),
            actual: requested.to_string(),
            label: requested.to_string(),
            alias: None,
        }
    }
}

/// CPython WASI 沙箱工具的运行时配置（从 .minkrc 的 `[sandbox_python]` 加载）。
#[derive(Debug, Clone)]
pub struct SandboxPythonConfig {
    pub wasm_path: String,
    pub stdlib_dir: String,
    pub timeout: u64,
    pub read_dirs: Vec<String>,
    pub write_dirs: Vec<String>,
    pub package_dirs: Vec<String>,
}

impl Default for SandboxPythonConfig {
    fn default() -> Self {
        Self {
            wasm_path: "cpython-wasi/python.wasm".into(),
            stdlib_dir: "cpython-wasi".into(),
            timeout: 30,
            read_dirs: Vec::new(),
            write_dirs: Vec::new(),
            package_dirs: Vec::new(),
        }
    }
}

/// Fixed runtime constants for the signal algorithm.
#[derive(Debug, Clone)]
pub(crate) struct SignalConfig {
    /// B >= remind_threshold 时无响应。
    pub remind_threshold: f64,
    /// B < warn_threshold 时升级为 Warning 级响应。
    pub warn_threshold: f64,
    /// B < abort_threshold 时中止（用户接管/降级重启）。
    pub abort_threshold: f64,
    /// Beta 先验 alpha（成功证据）。
    pub alpha_prior: f64,
    /// Beta 先验 beta（失败证据）。
    pub beta_prior: f64,
    /// 滑动窗口大小。
    pub window_size: usize,
    /// 每用户输入结束时对信念的衰减因子（0 = 完全重置，1 = 不衰减）。
    pub decay_per_input: f64,
    /// 轨迹证据注入的字符预算。
    pub evidence_max_chars: usize,
    /// 恢复守卫连续拦截上限；达到后绕过守卫并强制证据注入。
    pub guard_max_blocks: usize,
    /// 注入后的冷却轮数。
    pub cooldown_turns: usize,
    /// Fresh recovery agent 的最大内部轮数。
    pub replan_max_turns: i32,
    /// Fresh recovery agent 的最大输出长度。
    pub replan_token_budget: i32,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            remind_threshold: 0.70,
            warn_threshold: 0.50,
            abort_threshold: 0.30,
            alpha_prior: 3.0,
            beta_prior: 1.0,
            window_size: 16,
            decay_per_input: 0.6,
            evidence_max_chars: 4_000,
            guard_max_blocks: 3,
            cooldown_turns: 3,
            replan_max_turns: 12,
            replan_token_budget: 24_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub model: String,
    pub model_aliases: BTreeMap<String, String>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: bool,
    pub openai_token_param: TokenParamKind,
    pub openai_tool_choice: Option<serde_json::Value>,
    pub openai_extra_body: BTreeMap<String, serde_json::Value>,
    pub max_tokens: i32,
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub llm_first_event_timeout_secs: i32,
    pub llm_idle_timeout_secs: i32,
    pub llm_wait_heartbeat_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    pub edit_mode: EditMode,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: f64,
    pub edit_enforce_seen_lines: bool,
    pub max_search_files: usize,
    pub max_search_results: usize,
    pub output_format: OutputFormat,
    pub verbose: bool,
    pub api_key: String,
    pub base_url: String,
    pub prompt: String,
    pub max_turns: i32,
    pub max_context_tokens: usize,
    pub context_compact_pct: u8,
    pub context_reserve_tokens: usize,
    pub context_compact_tail_tokens: usize,
    pub context_compact_max_output_tokens: i32,
    pub context_compact_input_reduction: bool,
    /// Project the confirmed plan as the **last** message (default true) so
    /// plan edits stay outside the cacheable prefix; false restores the legacy
    /// head projection (after leading system messages) for A/B fallback.
    pub plan_projection_tail: bool,
    pub skills: Vec<String>,
    pub interactive: bool,
    pub session_id: String,
    pub continue_session: bool,
    pub log_events: bool,
    /// Stable single-shot SDK protocol: stdin request + stdout JSONL events + final.
    pub agent_jsonl: bool,
    /// 沙箱配置（从 .minkrc 加载）
    pub sandbox: SandboxConfig,
    /// CPython WASI 沙箱工具配置
    pub sandbox_python: SandboxPythonConfig,
    /// 自定义系统提示词文件（MISSION.md）
    pub mission_file: Option<PathBuf>,
    /// 内联 mission 内容（通过 SDK 协议传入，不写临时文件）
    pub mission_content: Option<String>,
    /// 工具选择：`None` 使用默认工具集；`Some(vec![])` 不启用任何工具。
    pub enabled_tools: Option<Vec<String>>,
    /// Tool approval mode.
    pub tool_approval_mode: ToolApprovalMode,
    /// Per-tool approval overrides keyed by tool name.
    pub tool_approval: BTreeMap<String, ToolApprovalPolicy>,
    pub signal_policy: SignalPolicy,
    pub(crate) signal: SignalConfig,
}
impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            model_aliases: BTreeMap::new(),
            openai_reasoning_effort: Some("max".to_string()),
            openai_include_usage: true,
            openai_token_param: TokenParamKind::MaxTokens,
            openai_tool_choice: None,
            openai_extra_body: BTreeMap::new(),
            max_tokens: 81920,
            tool_timeout_secs: 600,
            sub_agent_timeout_secs: 300,
            llm_first_event_timeout_secs: 60,
            llm_idle_timeout_secs: 90,
            llm_wait_heartbeat_secs: 30,
            tool_result_max_bytes: 100_000,
            file_write_max_bytes: 1_048_576,
            edit_mode: EditMode::Hashline,
            edit_fuzzy_match: true,
            edit_fuzzy_threshold: 0.95,
            edit_enforce_seen_lines: false,
            max_search_files: 5000,
            max_search_results: 1000,
            output_format: OutputFormat::Human,
            verbose: false,
            api_key: String::new(),
            base_url: String::new(),
            prompt: String::new(),
            max_turns: 40,
            max_context_tokens: 1_000_000,
            context_compact_pct: 94,
            context_reserve_tokens: 64_000,
            context_compact_tail_tokens: 256_000,
            context_compact_max_output_tokens: 8_192,
            context_compact_input_reduction: false,
            plan_projection_tail: true,
            skills: Vec::new(),
            interactive: false,
            session_id: String::new(),
            continue_session: false,
            log_events: true,
            agent_jsonl: false,
            sandbox: SandboxConfig::default(),
            sandbox_python: SandboxPythonConfig::default(),
            mission_file: None,
            mission_content: None,
            enabled_tools: None,
            tool_approval_mode: ToolApprovalMode::Yolo,
            tool_approval: BTreeMap::new(),
            signal_policy: SignalPolicy::Full,
            signal: SignalConfig::default(),
        }
    }
}

pub fn api_url(cfg: &ResolvedConfig) -> String {
    let base = if cfg.base_url.is_empty() {
        "https://api.deepseek.com/v1"
    } else {
        cfg.base_url.as_str()
    };
    format!("{}/chat/completions", base.trim_end_matches('/'))
}

pub fn model_resolver(cfg: &ResolvedConfig) -> ModelResolver {
    ModelResolver::new(&cfg.model_aliases)
}

pub fn validate_runtime_config(cfg: &ResolvedConfig) -> Result<()> {
    if !cfg.edit_fuzzy_threshold.is_finite() || !(0.0..=1.0).contains(&cfg.edit_fuzzy_threshold) {
        bail!("edit_fuzzy_threshold must be a finite number in 0.0..=1.0");
    }
    if cfg.max_tokens <= 0 {
        bail!("max_tokens must be greater than 0");
    }
    if !(1..=100).contains(&cfg.context_compact_pct) {
        bail!("context_compact_pct must be between 1 and 100");
    }
    if cfg.context_reserve_tokens == 0 {
        bail!("context_reserve_tokens must be greater than 0");
    }
    if cfg.context_compact_tail_tokens == 0 {
        bail!("context_compact_tail_tokens must be greater than 0");
    }
    if cfg.context_compact_max_output_tokens <= 0 {
        bail!("context_compact_max_output_tokens must be greater than 0");
    }
    let s = &cfg.signal;
    if !(0.0 < s.abort_threshold
        && s.abort_threshold < s.warn_threshold
        && s.warn_threshold < s.remind_threshold
        && s.remind_threshold < 1.0)
    {
        bail!(
            "signal thresholds must satisfy 0 < abort ({}) < warn ({}) < remind ({}) < 1",
            s.abort_threshold,
            s.warn_threshold,
            s.remind_threshold
        );
    }
    if s.alpha_prior <= 0.0 || s.beta_prior <= 0.0 {
        bail!("signal alpha_prior/beta_prior must be positive");
    }
    if s.window_size == 0 {
        bail!("signal window_size must be greater than 0");
    }
    if !(0.0..=1.0).contains(&s.decay_per_input) {
        bail!("signal decay_per_input must be in 0.0..=1.0");
    }
    if s.guard_max_blocks == 0 {
        bail!("signal guard_max_blocks must be greater than 0");
    }
    if s.replan_max_turns <= 0 {
        bail!("signal replan_max_turns must be greater than 0");
    }
    if s.replan_token_budget <= 0 {
        bail!("signal replan_token_budget must be greater than 0");
    }
    if cfg.max_context_tokens == 0 {
        return Ok(());
    }

    let max_context = cfg.max_context_tokens;
    if cfg.context_reserve_tokens >= max_context {
        bail!(
            "context_reserve_tokens ({}) must be less than max_context ({max_context})",
            cfg.context_reserve_tokens
        );
    }
    let compact_output = usize::try_from(cfg.context_compact_max_output_tokens)
        .map_err(|_| anyhow::anyhow!("context_compact_max_output_tokens is too large"))?;
    if compact_output >= max_context {
        bail!(
            "context_compact_max_output_tokens ({compact_output}) must be less than max_context ({max_context})"
        );
    }

    let requested_output =
        usize::try_from(cfg.max_tokens).map_err(|_| anyhow::anyhow!("max_tokens is too large"))?;
    let response_budget = requested_output.min(cfg.context_reserve_tokens);
    let request_input_budget = max_context - response_budget;
    if cfg.context_compact_tail_tokens >= request_input_budget {
        bail!(
            "context_compact_tail_tokens ({}) must be less than the request input budget ({request_input_budget} = max_context {max_context} - response budget {response_budget})",
            cfg.context_compact_tail_tokens
        );
    }
    Ok(())
}
// ── Sandbox configuration ──────────────────────────────────────────

/// 沙箱限制配置 — 从 `.minkrc` 的 `[sandbox]` 段或环境变量 `MINK_LIMITS` 加载。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// 是否启用沙箱（自举重新执行）
    pub enabled: bool,
    /// 沙箱后端: "auto" | "nsjail" | "bwrap" | "sandbox-exec" | "off"
    pub backend: String,
    /// 允许读取的目录白名单（相对于 cwd 或绝对路径）
    pub read_dirs: Vec<String>,
    /// 允许写入的目录白名单
    pub write_dirs: Vec<String>,
    /// 允许网络访问（含 LLM API 调用）
    pub allow_network: bool,
    /// 最大内存（MB）
    pub max_memory_mb: u64,
    /// 最大进程数
    pub max_pids: u32,
    /// 超时（秒）
    pub timeout_secs: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: "auto".into(),
            read_dirs: Vec::new(),
            write_dirs: Vec::new(),
            allow_network: true,
            max_memory_mb: 1024,
            max_pids: 64,
            timeout_secs: 600,
        }
    }
}

impl SandboxConfig {
    /// 是否实际需要沙箱（enabled 且 backend 不为 "off"）
    pub fn is_active(&self) -> bool {
        self.enabled && self.backend != "off"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolApprovalMode {
    AlwaysAsk,
    Write,
    Yolo,
}

impl ToolApprovalMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "always-ask" => Ok(Self::AlwaysAsk),
            "write" => Ok(Self::Write),
            "yolo" => Ok(Self::Yolo),
            _ => bail!("unknown approval mode: {raw}. Use always-ask, write, or yolo"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolApprovalPolicy {
    Allow,
    Deny,
    Prompt,
}
