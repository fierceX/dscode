use anyhow::{Result, anyhow, bail};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TuiMode {
    #[default]
    Off,
    Full,
    Inline,
}

impl TuiMode {
    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiTokenParamConfig {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
}

impl OpenAiTokenParamConfig {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "max_tokens" | "max-tokens" | "max_tokens_param" => Some(Self::MaxTokens),
            "max_completion_tokens" | "max-completion-tokens" => Some(Self::MaxCompletionTokens),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMode {
    Off,
    Full,
}

impl SignalMode {
    pub fn from_env() -> Self {
        match std::env::var("MINK_SIGNAL_MODE")
            .unwrap_or_else(|_| "full".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "false" | "0" | "none" | "disabled" => Self::Off,
            _ => Self::Full,
        }
    }

    pub fn enabled(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Built-in model aliases used for DeepSeek defaults and legacy price tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelTier {
    #[default]
    Flash,
    Pro,
}

impl ModelTier {
    /// Convert tier to DeepSeek API model name.
    pub fn model_name(&self) -> &'static str {
        match self {
            ModelTier::Flash => "deepseek-v4-flash",
            ModelTier::Pro => "deepseek-v4-pro",
        }
    }

    /// Convert tier to display label for the title bar.
    pub fn label(&self) -> &'static str {
        match self {
            ModelTier::Flash => "flash",
            ModelTier::Pro => "pro",
        }
    }

    /// Price per million input tokens (uncached).
    pub fn price_input_per_m(&self) -> f64 {
        match self {
            ModelTier::Flash => 1.0,
            ModelTier::Pro => 3.0,
        }
    }

    /// Price per million output tokens.
    pub fn price_output_per_m(&self) -> f64 {
        match self {
            ModelTier::Flash => 2.0,
            ModelTier::Pro => 6.0,
        }
    }

    /// Price per million cache-read tokens.
    pub fn price_cache_read_per_m(&self) -> f64 {
        match self {
            ModelTier::Flash => 0.02,
            ModelTier::Pro => 0.025,
        }
    }

    /// Parse from CLI flag or config file string.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "flash" | "deepseek-v4-flash" => Ok(ModelTier::Flash),
            "pro" | "deepseek-v4-pro" => Ok(ModelTier::Pro),
            _ => bail!("unknown model tier: {s}. Use 'flash' or 'pro'"),
        }
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

/// TOML config file structure (optional, loaded from ~/.minkrc or <project>/.minkrc).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinkConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub model_aliases: Option<BTreeMap<String, String>>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: Option<bool>,
    pub openai_token_param: Option<String>,
    pub openai_tool_choice: Option<serde_json::Value>,
    pub openai_extra_body: Option<BTreeMap<String, serde_json::Value>>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub max_context: Option<String>, // supports K/M suffix
    pub max_search_files: Option<usize>,
    pub max_search_results: Option<usize>,
    pub tool_timeout: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub llm_first_event_timeout: Option<i32>,
    pub llm_idle_timeout: Option<i32>,
    pub llm_wait_heartbeat: Option<i32>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
    pub log_events: Option<bool>,
    pub output_format: Option<String>,
    pub approval_mode: Option<String>,
    pub enabled_tools: Option<Vec<String>>,
    pub edit_mode: Option<EditMode>,
    pub edit_fuzzy_match: Option<bool>,
    pub edit_fuzzy_threshold: Option<f64>,
    pub edit_enforce_seen_lines: Option<bool>,
    pub skills: Option<Vec<String>>,
    /// `[sandbox]` section — when enabled, mink re-execs itself inside a sandbox.
    #[serde(default)]
    pub sandbox: Option<SandboxConfigFile>,
    /// `[tools]` section — tool approval and policy settings.
    #[serde(default)]
    pub tools: Option<ToolsConfigFile>,
    /// `[sandbox_python]` section — CPython WASI 沙箱工具的配置。
    #[serde(default)]
    pub sandbox_python: Option<SandboxPythonConfigFile>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfigFile {
    pub approval_mode: Option<ToolApprovalMode>,
    pub approval: Option<BTreeMap<String, ToolApprovalPolicy>>,
}

/// The `[sandbox]` section in .minkrc (all fields optional, inherits defaults).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SandboxConfigFile {
    pub enabled: Option<bool>,
    pub backend: Option<String>,
    pub read_dirs: Option<Vec<String>>,
    pub write_dirs: Option<Vec<String>>,
    pub allow_network: Option<bool>,
    pub max_memory_mb: Option<u64>,
    pub max_pids: Option<u32>,
    pub timeout_secs: Option<u64>,
}

/// `[sandbox_python]` section in .minkrc — CPython WASI 沙箱配置。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SandboxPythonConfigFile {
    /// python.wasm 路径（默认: cpython-wasi/python.wasm）
    pub wasm_path: Option<String>,
    /// 标准库目录路径（挂载为 /usr/local）
    pub stdlib_dir: Option<String>,
    /// 超时秒数（默认: 30）
    pub timeout: Option<u64>,
    /// 允许读取的目录
    pub read_dirs: Option<Vec<String>>,
    /// 允许写入的目录
    pub write_dirs: Option<Vec<String>>,
    /// Python 包目录（挂载到 /packages）
    pub package_dirs: Option<Vec<String>>,
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

impl SandboxPythonConfig {
    pub fn from_file(cfg: Option<&SandboxPythonConfigFile>) -> Self {
        let Some(cfg) = cfg else {
            return Self::default();
        };
        Self {
            wasm_path: cfg
                .wasm_path
                .clone()
                .unwrap_or_else(|| "cpython-wasi/python.wasm".into()),
            stdlib_dir: cfg
                .stdlib_dir
                .clone()
                .unwrap_or_else(|| "cpython-wasi".into()),
            timeout: cfg.timeout.unwrap_or(30),
            read_dirs: cfg.read_dirs.clone().unwrap_or_default(),
            write_dirs: cfg.write_dirs.clone().unwrap_or_default(),
            package_dirs: cfg.package_dirs.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub model_aliases: BTreeMap<String, String>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: bool,
    pub openai_token_param: OpenAiTokenParamConfig,
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
    pub tui_mode: TuiMode,
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
    pub skills: Vec<String>,
    pub interactive: bool,
    pub session_id: String,
    pub continue_session: bool,
    pub list_sessions: bool,
    pub list_skills: bool,
    pub log_events: bool,
    pub cli_overrides: CliOverrides,
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
    /// 从 --config CLI 参数解析的 TOML 配置（最高优先级，在 apply_config_sources 中应用）
    pub cli_config: Option<MinkConfigFile>,
    /// 工具选择：`None` 使用默认工具集；`Some(vec![])` 不启用任何工具。
    pub enabled_tools: Option<Vec<String>>,
    /// Tool approval mode.
    pub tool_approval_mode: ToolApprovalMode,
    /// Per-tool approval overrides keyed by tool name.
    pub tool_approval: BTreeMap<String, ToolApprovalPolicy>,
}

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub model: bool,
    pub max_tokens: bool,
    pub tool_timeout_secs: bool,
    pub sub_agent_timeout_secs: bool,
    pub llm_first_event_timeout_secs: bool,
    pub llm_idle_timeout_secs: bool,
    pub llm_wait_heartbeat_secs: bool,
    pub api_key: bool,
    pub base_url: bool,
    pub max_turns: bool,
    pub max_context_tokens: bool,
    pub tool_approval_mode: bool,
    pub output_format: bool,
    pub enabled_tools: bool,
    pub edit_mode: bool,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: bool,
    pub edit_enforce_seen_lines: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            model_aliases: BTreeMap::new(),
            openai_reasoning_effort: Some("max".to_string()),
            openai_include_usage: true,
            openai_token_param: OpenAiTokenParamConfig::MaxTokens,
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
            tui_mode: TuiMode::Off,
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
            skills: Vec::new(),
            interactive: false,
            session_id: String::new(),
            continue_session: false,
            list_sessions: false,
            list_skills: false,
            log_events: true,
            cli_overrides: CliOverrides::default(),
            agent_jsonl: false,
            sandbox: SandboxConfig::default(),
            sandbox_python: SandboxPythonConfig::default(),
            mission_file: None,
            mission_content: None,
            enabled_tools: None,
            cli_config: None,
            tool_approval_mode: ToolApprovalMode::Yolo,
            tool_approval: BTreeMap::new(),
        }
    }
}

pub fn parse_args(args: Vec<String>) -> Result<Config> {
    let mut cfg = Config::default();
    let mut i = 0usize;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-m" | "--model" => {
                let val = require_value(&args, i)?;
                if val.trim().is_empty() {
                    bail!("model must not be empty");
                }
                cfg.model = val;
                cfg.cli_overrides.model = true;
                i += 2;
            }
            "--mission" => {
                cfg.mission_file = Some(require_value(&args, i)?.into());
                i += 2;
            }
            "--api-key" => {
                cfg.api_key = require_value(&args, i)?;
                cfg.cli_overrides.api_key = true;
                i += 2;
            }
            "--base-url" => {
                cfg.base_url = require_value(&args, i)?;
                cfg.cli_overrides.base_url = true;
                i += 2;
            }
            "--print" => {
                cfg.output_format = OutputFormat::StreamJson;
                i += 1;
            }
            "--session" => {
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    cfg.session_id = args[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            arg if arg.starts_with("--session=") => {
                let value = arg.strip_prefix("--session=").unwrap_or_default().trim();
                if value.is_empty() {
                    bail!("missing value for --session");
                }
                cfg.session_id = value.to_string();
                i += 1;
            }
            "--continue" => {
                cfg.continue_session = true;
                i += 1;
            }
            "--list-sessions" => {
                cfg.list_sessions = true;
                i += 1;
            }
            "--list-skills" => {
                cfg.list_skills = true;
                i += 1;
            }
            "-v" | "--verbose" => {
                cfg.verbose = true;
                i += 1;
            }
            "--tui" => {
                cfg.tui_mode = TuiMode::Full;
                i += 1;
            }
            "--tui=full" => {
                cfg.tui_mode = TuiMode::Full;
                i += 1;
            }
            "--tui=inline" => {
                cfg.tui_mode = TuiMode::Inline;
                i += 1;
            }
            "-i" | "--interactive" => {
                cfg.interactive = true;
                i += 1;
            }
            "-h" | "--help" => {
                return Err(anyhow!("__HELP__"));
            }
            "--agent-jsonl" => {
                cfg.agent_jsonl = true;
                cfg.output_format = OutputFormat::StreamJson;
                i += 1;
            }
            "--enabled-tools" => {
                let value = require_value(&args, i)?;
                cfg.enabled_tools = Some(parse_enabled_tools(value)?);
                cfg.cli_overrides.enabled_tools = true;
                i += 2;
            }
            "--edit-mode" => {
                cfg.edit_mode = EditMode::parse(&require_value(&args, i)?)?;
                cfg.cli_overrides.edit_mode = true;
                i += 2;
            }
            "--edit-fuzzy-match" => {
                cfg.edit_fuzzy_match =
                    parse_bool_value("--edit-fuzzy-match", &require_value(&args, i)?)?;
                cfg.cli_overrides.edit_fuzzy_match = true;
                i += 2;
            }
            "--edit-fuzzy-threshold" => {
                cfg.edit_fuzzy_threshold = require_value(&args, i)?.parse().map_err(|_| {
                    anyhow!("--edit-fuzzy-threshold requires a number in 0.0..=1.0")
                })?;
                cfg.cli_overrides.edit_fuzzy_threshold = true;
                i += 2;
            }
            "--edit-enforce-seen-lines" => {
                cfg.edit_enforce_seen_lines =
                    parse_bool_value("--edit-enforce-seen-lines", &require_value(&args, i)?)?;
                cfg.cli_overrides.edit_enforce_seen_lines = true;
                i += 2;
            }
            "--config" => {
                let toml_str = require_value(&args, i)?;
                cfg.cli_config = Some(toml::from_str::<MinkConfigFile>(&toml_str)?);
                i += 2;
            }
            _ => {
                if arg.starts_with('-') {
                    bail!("unknown option: {arg}");
                }
                cfg.prompt = arg.clone();
                i += 1;
            }
        }
    }

    Ok(cfg)
}

fn require_value(args: &[String], i: usize) -> Result<String> {
    if i + 1 >= args.len() {
        bail!("missing value for {}", args[i]);
    }
    Ok(args[i + 1].clone())
}

fn parse_enabled_tools(value: String) -> Result<Vec<String>> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    let tools = value
        .split(',')
        .map(str::trim)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if tools.is_empty() || tools.iter().any(String::is_empty) {
        bail!("--enabled-tools requires comma-separated tool names or 'none'");
    }
    Ok(tools)
}

fn parse_bool_value(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!("{name} requires true or false"),
    }
}

pub fn apply_config_file(cfg: &mut Config) -> Result<()> {
    let defaults = Config::default();
    apply_env_defaults(cfg, &defaults)?;
    // SDK 协议模式：所有配置已通过 --config TOML 传入，跳过文件 I/O
    if cfg.agent_jsonl {
        let cli_cfg = cfg.cli_config.take();
        apply_config_sources(cfg, &defaults, None, None, cli_cfg.as_ref());
        apply_sandbox_config(cfg, None, None, cli_cfg.as_ref());
        cfg.cli_config = cli_cfg;
        return Ok(());
    }
    // Priority: CLI > project .minkrc > user ~/.minkrc > env > default.
    // CLI is inferred by comparing the already-parsed config to defaults.
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    let user_cfg = read_config_file(&home.join(".minkrc"))?;
    let project_cfg = read_config_file(&cwd.join(".minkrc"))?;
    let cli_cfg = cfg.cli_config.take();
    apply_config_sources(
        cfg,
        &defaults,
        user_cfg.as_ref(),
        project_cfg.as_ref(),
        cli_cfg.as_ref(),
    );
    apply_sandbox_config(
        cfg,
        user_cfg.as_ref(),
        project_cfg.as_ref(),
        cli_cfg.as_ref(),
    );
    cfg.cli_config = cli_cfg;
    Ok(())
}

fn apply_env_defaults(cfg: &mut Config, defaults: &Config) -> Result<()> {
    if cfg.log_events == defaults.log_events
        && let Ok(v) = std::env::var("LOG_EVENTS")
    {
        apply_log_events_env_value(cfg, v.as_str());
    }
    if !cfg.cli_overrides.edit_mode
        && let Ok(value) = std::env::var("MINK_EDIT_MODE")
    {
        cfg.edit_mode = EditMode::parse(&value)?;
    }
    if !cfg.cli_overrides.edit_fuzzy_match
        && let Ok(value) = std::env::var("MINK_EDIT_FUZZY_MATCH")
    {
        cfg.edit_fuzzy_match = parse_bool_value("MINK_EDIT_FUZZY_MATCH", &value)?;
    }
    if !cfg.cli_overrides.edit_fuzzy_threshold
        && let Ok(value) = std::env::var("MINK_EDIT_FUZZY_THRESHOLD")
    {
        cfg.edit_fuzzy_threshold = value
            .parse()
            .map_err(|_| anyhow!("MINK_EDIT_FUZZY_THRESHOLD requires a number in 0.0..=1.0"))?;
    }
    if !cfg.cli_overrides.edit_enforce_seen_lines
        && let Ok(value) = std::env::var("MINK_EDIT_ENFORCE_SEEN_LINES")
    {
        cfg.edit_enforce_seen_lines = parse_bool_value("MINK_EDIT_ENFORCE_SEEN_LINES", &value)?;
    }
    Ok(())
}

fn apply_log_events_env_value(cfg: &mut Config, value: &str) {
    cfg.log_events = value != "0" && value != "false" && value != "no";
}

fn read_config_file(path: &std::path::Path) -> Result<Option<MinkConfigFile>> {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "[mink] Warning: failed to read config file {}: {}",
                    path.display(),
                    e
                );
            }
            return if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(e.into())
            };
        }
    };
    toml::from_str(&data)
        .map(Some)
        .map_err(|e| anyhow!("failed to parse config file {}: {e}", path.display()))
}

fn apply_config_sources(
    cfg: &mut Config,
    defaults: &Config,
    user_cfg: Option<&MinkConfigFile>,
    project_cfg: Option<&MinkConfigFile>,
    cli_cfg: Option<&MinkConfigFile>,
) {
    let cli_model = cfg.cli_overrides.model || cfg.model != defaults.model;
    let cli_api_key = cfg.cli_overrides.api_key || cfg.api_key != defaults.api_key;
    let cli_base_url = cfg.cli_overrides.base_url || cfg.base_url != defaults.base_url;
    let cli_max_tokens = cfg.cli_overrides.max_tokens || cfg.max_tokens != defaults.max_tokens;
    let cli_max_turns = cfg.cli_overrides.max_turns || cfg.max_turns != defaults.max_turns;
    let cli_max_context = cfg.cli_overrides.max_context_tokens
        || cfg.max_context_tokens != defaults.max_context_tokens;
    let cli_tool_timeout =
        cfg.cli_overrides.tool_timeout_secs || cfg.tool_timeout_secs != defaults.tool_timeout_secs;
    let cli_sub_agent_timeout = cfg.cli_overrides.sub_agent_timeout_secs
        || cfg.sub_agent_timeout_secs != defaults.sub_agent_timeout_secs;
    let cli_llm_first_event_timeout = cfg.cli_overrides.llm_first_event_timeout_secs
        || cfg.llm_first_event_timeout_secs != defaults.llm_first_event_timeout_secs;
    let cli_llm_idle_timeout = cfg.cli_overrides.llm_idle_timeout_secs
        || cfg.llm_idle_timeout_secs != defaults.llm_idle_timeout_secs;
    let cli_llm_wait_heartbeat = cfg.cli_overrides.llm_wait_heartbeat_secs
        || cfg.llm_wait_heartbeat_secs != defaults.llm_wait_heartbeat_secs;
    let cli_tool_approval_mode = cfg.cli_overrides.tool_approval_mode
        || cfg.tool_approval_mode != defaults.tool_approval_mode;
    let cli_output_format =
        cfg.cli_overrides.output_format || cfg.output_format != defaults.output_format;
    let cli_enabled_tools = cfg.cli_overrides.enabled_tools;
    let cli_edit_mode = cfg.cli_overrides.edit_mode;
    let cli_edit_fuzzy_match = cfg.cli_overrides.edit_fuzzy_match;
    let cli_edit_fuzzy_threshold = cfg.cli_overrides.edit_fuzzy_threshold;
    let cli_edit_enforce_seen_lines = cfg.cli_overrides.edit_enforce_seen_lines;

    for toml_cfg in [user_cfg, project_cfg, cli_cfg].into_iter().flatten() {
        if !cli_model && let Some(model) = &toml_cfg.model {
            cfg.model = model.clone();
        }
        if let Some(model_aliases) = &toml_cfg.model_aliases {
            for (alias, model) in model_aliases {
                cfg.model_aliases.insert(alias.clone(), model.clone());
            }
        }
        if let Some(reasoning_effort) = &toml_cfg.openai_reasoning_effort {
            cfg.openai_reasoning_effort = normalize_openai_reasoning_effort(reasoning_effort);
        }
        if let Some(include_usage) = toml_cfg.openai_include_usage {
            cfg.openai_include_usage = include_usage;
        }
        if let Some(token_param) = &toml_cfg.openai_token_param {
            if let Some(parsed) = OpenAiTokenParamConfig::parse(token_param) {
                cfg.openai_token_param = parsed;
            } else {
                eprintln!("[mink] Warning: ignoring openai_token_param={token_param:?}");
            }
        }
        if let Some(tool_choice) = &toml_cfg.openai_tool_choice {
            cfg.openai_tool_choice = Some(tool_choice.clone());
        }
        if let Some(extra_body) = &toml_cfg.openai_extra_body {
            cfg.openai_extra_body.extend(extra_body.clone());
        }
        if !cli_api_key && let Some(api_key) = &toml_cfg.api_key {
            cfg.api_key = api_key.clone();
        }
        if !cli_base_url && let Some(base_url) = &toml_cfg.base_url {
            cfg.base_url = base_url.clone();
        }
        if !cli_max_tokens && let Some(max_tokens) = toml_cfg.max_tokens {
            cfg.max_tokens = max_tokens;
        }
        if !cli_max_turns && let Some(max_turns) = toml_cfg.max_turns {
            cfg.max_turns = max_turns;
        }
        if !cli_max_context
            && let Some(max_context) = &toml_cfg.max_context
            && let Ok(v) = parse_size_bytes(max_context)
        {
            cfg.max_context_tokens = v;
        }
        if let Some(v) = toml_cfg.max_search_files {
            cfg.max_search_files = v;
        }
        if let Some(v) = toml_cfg.max_search_results {
            cfg.max_search_results = v;
        }
        if !cli_tool_timeout && let Some(tool_timeout) = toml_cfg.tool_timeout {
            apply_positive_i32_config(&mut cfg.tool_timeout_secs, tool_timeout, "tool_timeout");
        }
        if !cli_sub_agent_timeout && let Some(sub_agent_timeout) = toml_cfg.sub_agent_timeout {
            apply_positive_i32_config(
                &mut cfg.sub_agent_timeout_secs,
                sub_agent_timeout,
                "sub_agent_timeout",
            );
        }
        if !cli_llm_first_event_timeout && let Some(timeout) = toml_cfg.llm_first_event_timeout {
            apply_positive_i32_config(
                &mut cfg.llm_first_event_timeout_secs,
                timeout,
                "llm_first_event_timeout",
            );
        }
        if !cli_llm_idle_timeout && let Some(timeout) = toml_cfg.llm_idle_timeout {
            apply_positive_i32_config(&mut cfg.llm_idle_timeout_secs, timeout, "llm_idle_timeout");
        }
        if !cli_llm_wait_heartbeat && let Some(timeout) = toml_cfg.llm_wait_heartbeat {
            apply_nonnegative_i32_config(
                &mut cfg.llm_wait_heartbeat_secs,
                timeout,
                "llm_wait_heartbeat",
            );
        }
        if let Some(context_compact_pct) = toml_cfg.context_compact_pct {
            if (1..=100).contains(&context_compact_pct) {
                cfg.context_compact_pct = context_compact_pct;
            } else {
                eprintln!(
                    "[mink] Warning: ignoring context_compact_pct={context_compact_pct}; expected 1-100"
                );
            }
        }
        if let Some(tokens) = toml_cfg.context_reserve_tokens {
            if tokens > 0 {
                cfg.context_reserve_tokens = tokens;
            } else {
                eprintln!("[mink] Warning: ignoring context_reserve_tokens=0");
            }
        }
        if let Some(tokens) = toml_cfg.context_compact_tail_tokens {
            if tokens > 0 {
                cfg.context_compact_tail_tokens = tokens;
            } else {
                eprintln!("[mink] Warning: ignoring context_compact_tail_tokens=0");
            }
        }
        if let Some(tokens) = toml_cfg.context_compact_max_output_tokens {
            apply_positive_i32_config(
                &mut cfg.context_compact_max_output_tokens,
                tokens,
                "context_compact_max_output_tokens",
            );
        }
        if let Some(enabled) = toml_cfg.context_compact_input_reduction {
            cfg.context_compact_input_reduction = enabled;
        }
        if let Some(log_events) = toml_cfg.log_events {
            cfg.log_events = log_events;
        }
        if let Some(tools) = &toml_cfg.tools {
            if !cli_tool_approval_mode && let Some(mode) = tools.approval_mode {
                cfg.tool_approval_mode = mode;
            }
            if let Some(approval) = &tools.approval {
                for (name, policy) in approval {
                    cfg.tool_approval.insert(name.clone(), *policy);
                }
            }
        }
        if !cli_output_format && let Some(ref v) = toml_cfg.output_format {
            match v.as_str() {
                "human" => cfg.output_format = OutputFormat::Human,
                "stream-json" => cfg.output_format = OutputFormat::StreamJson,
                _ => {}
            }
        }
        if let Some(ref v) = toml_cfg.skills {
            cfg.skills = v.clone();
        }
        if !cli_enabled_tools && let Some(ref v) = toml_cfg.enabled_tools {
            cfg.enabled_tools = Some(v.clone());
        }
        if !cli_edit_mode && let Some(mode) = toml_cfg.edit_mode {
            cfg.edit_mode = mode;
        }
        if !cli_edit_fuzzy_match && let Some(enabled) = toml_cfg.edit_fuzzy_match {
            cfg.edit_fuzzy_match = enabled;
        }
        if !cli_edit_fuzzy_threshold && let Some(threshold) = toml_cfg.edit_fuzzy_threshold {
            cfg.edit_fuzzy_threshold = threshold;
        }
        if !cli_edit_enforce_seen_lines && let Some(enabled) = toml_cfg.edit_enforce_seen_lines {
            cfg.edit_enforce_seen_lines = enabled;
        }
        if !cli_tool_approval_mode
            && let Some(ref v) = toml_cfg.approval_mode
            && let Ok(m) = ToolApprovalMode::parse(v)
        {
            cfg.tool_approval_mode = m;
        }
    }
}

fn apply_positive_i32_config(target: &mut i32, value: i32, name: &str) {
    if value > 0 {
        *target = value;
    } else {
        eprintln!("[mink] Warning: ignoring {name}={value}; must be greater than 0");
    }
}

fn apply_nonnegative_i32_config(target: &mut i32, value: i32, name: &str) {
    if value >= 0 {
        *target = value;
    } else {
        eprintln!("[mink] Warning: ignoring {name}={value}; must be zero or greater");
    }
}

fn normalize_openai_reasoning_effort(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "off" | "none" | "false" | "disabled"
        )
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Apply sandbox config from TOML `[sandbox]` sections.
/// Project-level overrides user-level; both override defaults.
/// Only active when `sandbox.enabled = true` in the highest-priority config.
fn apply_sandbox_config(
    cfg: &mut Config,
    user_cfg: Option<&MinkConfigFile>,
    project_cfg: Option<&MinkConfigFile>,
    cli_cfg: Option<&MinkConfigFile>,
) {
    for toml_cfg in [user_cfg, project_cfg, cli_cfg].into_iter().flatten() {
        if let Some(ref sb) = toml_cfg.sandbox {
            if let Some(v) = sb.enabled {
                cfg.sandbox.enabled = v;
            }
            if let Some(ref v) = sb.backend {
                cfg.sandbox.backend = v.clone();
            }
            if let Some(ref v) = sb.read_dirs {
                cfg.sandbox.read_dirs = v.clone();
            }
            if let Some(ref v) = sb.write_dirs {
                cfg.sandbox.write_dirs = v.clone();
            }
            if let Some(v) = sb.allow_network {
                cfg.sandbox.allow_network = v;
            }
            if let Some(v) = sb.max_memory_mb {
                cfg.sandbox.max_memory_mb = v;
            }
            if let Some(v) = sb.max_pids {
                cfg.sandbox.max_pids = v;
            }
            if let Some(v) = sb.timeout_secs {
                cfg.sandbox.timeout_secs = v;
            }
        }

        // 合并 sandbox_python 配置（project 覆盖 user 覆盖 default）
        if let Some(ref sp) = toml_cfg.sandbox_python {
            if let Some(ref v) = sp.wasm_path {
                cfg.sandbox_python.wasm_path = v.clone();
            }
            if let Some(ref v) = sp.stdlib_dir {
                cfg.sandbox_python.stdlib_dir = v.clone();
            }
            if let Some(v) = sp.timeout {
                cfg.sandbox_python.timeout = v;
            }
            if let Some(ref v) = sp.read_dirs {
                cfg.sandbox_python.read_dirs = v.clone();
            }
            if let Some(ref v) = sp.write_dirs {
                cfg.sandbox_python.write_dirs = v.clone();
            }
            if let Some(ref v) = sp.package_dirs {
                cfg.sandbox_python.package_dirs = v.clone();
            }
        }
    }

    // Also check MINK_LIMITS env var (JSON format) — highest priority after CLI
    if let Ok(json) = std::env::var("MINK_LIMITS")
        && let Ok(sb) = serde_json::from_str::<SandboxConfig>(&json)
        && sb.enabled
    {
        cfg.sandbox = sb;
    }
}

pub fn apply_provider_defaults(cfg: &mut Config) -> Result<()> {
    // Env var overrides for size limits
    if let Ok(v) = std::env::var("TOOL_RESULT_MAX_BYTES")
        && let Ok(n) = v.parse::<usize>()
    {
        cfg.tool_result_max_bytes = n;
    }
    if let Ok(v) = std::env::var("FILE_WRITE_MAX_BYTES")
        && let Ok(n) = v.parse::<usize>()
    {
        cfg.file_write_max_bytes = n;
    }
    if let Ok(v) = std::env::var("MAX_SEARCH_FILES")
        && let Ok(n) = v.parse::<usize>()
    {
        cfg.max_search_files = n;
    }
    if let Ok(v) = std::env::var("MAX_SEARCH_RESULTS")
        && let Ok(n) = v.parse::<usize>()
    {
        cfg.max_search_results = n;
    }
    // API key: CLI/config > DEEPSEEK_API_KEY
    if cfg.api_key.is_empty() {
        cfg.api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_default();
    }
    // Base URL: DEEPSEEK_BASE_URL > CLI flag > default
    if cfg.base_url.is_empty() {
        cfg.base_url = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_default();
    }
    // Default model tier
    if cfg.model.is_empty() {
        cfg.model = "flash".to_string();
    }
    if cfg.api_key.is_empty() && cfg.base_url.is_empty() {
        bail!("no API key. Set DEEPSEEK_API_KEY or use --api-key");
    }
    Ok(())
}

pub fn api_url(cfg: &Config) -> String {
    let base = if cfg.base_url.is_empty() {
        "https://api.deepseek.com/v1"
    } else {
        cfg.base_url.as_str()
    };
    format!("{}/chat/completions", base.trim_end_matches('/'))
}

pub fn model_resolver(cfg: &Config) -> ModelResolver {
    ModelResolver::new(&cfg.model_aliases)
}

pub fn validate_runtime_config(cfg: &Config) -> Result<()> {
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

/// Resolve the actual API model name from a Config model string with default aliases.
pub fn resolve_model_name(model: &str) -> String {
    ModelResolver::new(&BTreeMap::new()).resolve(model).actual
}

/// Resolve the display label for the title bar.
pub fn resolve_model_label(model: &str) -> String {
    ModelResolver::new(&BTreeMap::new()).resolve(model).label
}

pub fn parse_size_bytes(raw: &str) -> Result<usize> {
    if raw.is_empty() {
        bail!("empty size");
    }
    let lower = raw.to_lowercase();
    let (num, m) = if let Some(v) = lower.strip_suffix('k') {
        (v, 1_000usize)
    } else if let Some(v) = lower.strip_suffix('m') {
        (v, 1_000_000usize)
    } else if let Some(v) = lower.strip_suffix('g') {
        (v, 1_000_000_000usize)
    } else {
        (lower.as_str(), 1usize)
    };
    Ok(num.parse::<usize>()? * m)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_bytes_plain() {
        assert_eq!(parse_size_bytes("100").unwrap(), 100);
        assert_eq!(parse_size_bytes("0").unwrap(), 0);
    }

    #[test]
    fn parse_size_bytes_k() {
        assert_eq!(parse_size_bytes("1k").unwrap(), 1000);
        assert_eq!(parse_size_bytes("50k").unwrap(), 50_000);
    }

    #[test]
    fn parse_size_bytes_m() {
        assert_eq!(parse_size_bytes("1m").unwrap(), 1_000_000);
        assert_eq!(parse_size_bytes("5M").unwrap(), 5_000_000);
    }

    #[test]
    fn parse_size_bytes_g() {
        assert_eq!(parse_size_bytes("1g").unwrap(), 1_000_000_000);
    }

    #[test]
    fn parse_size_bytes_empty_error() {
        assert!(parse_size_bytes("").is_err());
    }

    #[test]
    fn parse_args_model_provider() {
        let cfg = parse_args(vec!["-m".into(), "flash".into()]).unwrap();
        assert_eq!(cfg.model, "flash");
    }

    #[test]
    fn parse_args_model_accepts_custom_model_name() {
        let cfg = parse_args(vec!["-m".into(), "gpt-4.1".into()]).unwrap();
        assert_eq!(cfg.model, "gpt-4.1");
    }

    #[test]
    fn parse_args_config_rejects_invalid_toml() {
        let err = parse_args(vec!["--config".into(), "max_tokens =".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("TOML"), "{err}");
    }

    #[test]
    fn model_resolver_maps_aliases_and_preserves_custom_names() {
        let mut aliases = BTreeMap::new();
        aliases.insert("flash".to_string(), "local-fast".to_string());
        aliases.insert("coder".to_string(), "qwen3-coder-plus".to_string());
        let resolver = ModelResolver::new(&aliases);

        let flash = resolver.resolve("flash");
        assert_eq!(flash.actual, "local-fast");
        assert_eq!(flash.alias.as_deref(), Some("flash"));

        let custom = resolver.resolve("gpt-4.1");
        assert_eq!(custom.actual, "gpt-4.1");
        assert_eq!(custom.alias, None);
    }

    #[test]
    fn model_resolver_defaults_empty_model_to_flash_alias() {
        let resolver = ModelResolver::new(&BTreeMap::new());
        let resolved = resolver.resolve(" ");
        assert_eq!(resolved.requested, "flash");
        assert_eq!(resolved.actual, "deepseek-v4-flash");
        assert_eq!(resolved.label, "flash");
        assert_eq!(resolved.alias.as_deref(), Some("flash"));
    }

    #[test]
    fn parse_args_flags() {
        let cfg = parse_args(vec!["-v".into(), "-i".into(), "--print".into()]).unwrap();
        assert!(cfg.verbose);
        assert!(cfg.interactive);
        assert_eq!(cfg.output_format, OutputFormat::StreamJson);
    }

    #[test]
    fn parse_args_selects_full_and_inline_tui_modes() {
        assert_eq!(
            parse_args(vec!["--tui".into()]).unwrap().tui_mode,
            TuiMode::Full
        );
        assert_eq!(
            parse_args(vec!["--tui=full".into()]).unwrap().tui_mode,
            TuiMode::Full
        );
        assert_eq!(
            parse_args(vec!["--tui=inline".into()]).unwrap().tui_mode,
            TuiMode::Inline
        );
    }

    #[test]
    fn parse_args_enabled_tools_is_the_only_tool_selection_cli() {
        let selected = parse_args(vec![
            "--enabled-tools".into(),
            "Read, Bash,PythonSandbox".into(),
        ])
        .unwrap();
        assert_eq!(
            selected.enabled_tools,
            Some(vec!["Read".into(), "Bash".into(), "PythonSandbox".into()])
        );

        let none = parse_args(vec!["--enabled-tools".into(), "none".into()]).unwrap();
        assert_eq!(none.enabled_tools, Some(Vec::new()));
        assert!(parse_args(vec!["--disable-bash".into()]).is_err());
    }

    #[test]
    fn parse_args_session_accepts_separate_and_equals_forms() {
        let separate = parse_args(vec!["--session".into(), "feature-x".into()]).unwrap();
        assert_eq!(separate.session_id, "feature-x");

        let equals = parse_args(vec!["--session=feature-x".into()]).unwrap();
        assert_eq!(equals.session_id, "feature-x");

        let empty = parse_args(vec!["--session=".into()]).unwrap_err();
        assert!(empty.to_string().contains("missing value for --session"));
    }

    #[test]
    fn parse_args_agent_jsonl_enables_single_shot_protocol() {
        let cfg = parse_args(vec!["--agent-jsonl".into()]).unwrap();
        assert!(cfg.agent_jsonl);
        assert_eq!(cfg.output_format, OutputFormat::StreamJson);
    }

    #[test]
    fn agent_jsonl_applies_cli_config_without_file_io() {
        let toml = "max_search_files = 15000\nmax_search_results = 10000";
        let mut cfg =
            parse_args(vec!["--agent-jsonl".into(), "--config".into(), toml.into()]).unwrap();
        apply_config_file(&mut cfg).unwrap();
        assert_eq!(cfg.max_search_files, 15000);
        assert_eq!(cfg.max_search_results, 10000);
    }

    #[test]
    fn parse_args_json_rpc_is_removed() {
        let err = parse_args(vec!["--json-rpc".into()]).unwrap_err();
        assert!(err.to_string().contains("unknown option"));
    }

    #[test]
    fn parse_args_approval_mode() {
        let toml = r#"approval_mode = "write""#;
        let mut cfg = parse_args(vec!["--config".into(), toml.into()]).unwrap();
        let defaults = Config::default();
        let cli = cfg.cli_config.take();
        apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
        cfg.cli_config = cli;
        assert_eq!(cfg.tool_approval_mode, ToolApprovalMode::Write);
    }

    #[test]
    fn parse_args_prompt() {
        let cfg = parse_args(vec!["hello world".into()]).unwrap();
        assert_eq!(cfg.prompt, "hello world");
    }

    #[test]
    fn parse_args_unknown_flag_error() {
        assert!(parse_args(vec!["--unknown".into()]).is_err());
    }

    #[test]
    fn parse_args_llm_timeout_via_config() {
        let toml = "llm_first_event_timeout = 7\nllm_idle_timeout = 8\nllm_wait_heartbeat = 9";
        let mut cfg = parse_args(vec!["--config".into(), toml.into()]).unwrap();
        let defaults = Config::default();
        let cli = cfg.cli_config.take();
        apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
        cfg.cli_config = cli;
        assert_eq!(cfg.llm_first_event_timeout_secs, 7);
        assert_eq!(cfg.llm_idle_timeout_secs, 8);
        assert_eq!(cfg.llm_wait_heartbeat_secs, 9);
    }

    #[test]
    fn config_llm_timeout_via_toml() {
        let mut cfg = parse_args(vec!["--config".into(), "llm_wait_heartbeat = 0".into()]).unwrap();
        let defaults = Config::default();
        let cli = cfg.cli_config.take();
        apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
        cfg.cli_config = cli;
        assert_eq!(cfg.llm_wait_heartbeat_secs, 0);
    }

    #[test]
    fn parse_config_file_overrides_model() {
        let toml_str = r#"
model = "pro"
max_tokens = 163840
max_context = "500K"
tool_timeout = 120
llm_first_event_timeout = 11
llm_idle_timeout = 22
llm_wait_heartbeat = 3
"#;
        let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.model.unwrap(), "pro");
        assert_eq!(parsed.max_tokens.unwrap(), 163840);
        assert_eq!(parsed.max_context.unwrap(), "500K");
        assert_eq!(parsed.tool_timeout.unwrap(), 120);
        assert_eq!(parsed.llm_first_event_timeout.unwrap(), 11);
        assert_eq!(parsed.llm_idle_timeout.unwrap(), 22);
        assert_eq!(parsed.llm_wait_heartbeat.unwrap(), 3);
    }

    #[test]
    fn parse_config_file_openai_compatible_options() {
        let toml_str = r#"
openai_reasoning_effort = "off"
openai_include_usage = false
openai_token_param = "max_completion_tokens"
openai_tool_choice = "auto"

[openai_extra_body]
enable_thinking = true
thinking_budget = 8192
temperature = 0.2
"#;
        let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.openai_reasoning_effort.unwrap(), "off");
        assert_eq!(parsed.openai_include_usage, Some(false));
        assert_eq!(
            parsed.openai_token_param.unwrap(),
            "max_completion_tokens".to_string()
        );
        assert_eq!(
            parsed.openai_tool_choice.unwrap(),
            serde_json::json!("auto")
        );
        let extra_body = parsed.openai_extra_body.unwrap();
        assert_eq!(extra_body["enable_thinking"], serde_json::json!(true));
        assert_eq!(extra_body["thinking_budget"], serde_json::json!(8192));
        assert_eq!(extra_body["temperature"], serde_json::json!(0.2));
    }

    #[test]
    fn parse_config_file_partial_fields() {
        // Only setting one field should not require others
        let toml_str = r#"log_events = false"#;
        let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
        assert!(!parsed.log_events.unwrap());
        assert!(parsed.model.is_none());
        assert!(parsed.api_key.is_none());
    }

    #[test]
    fn parse_config_file_tools_approval() {
        let toml_str = r#"
[tools]
approval_mode = "write"

[tools.approval]
Bash = "prompt"
Read = "allow"
"#;
        let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
        let tools = parsed.tools.unwrap();
        assert_eq!(tools.approval_mode.unwrap(), ToolApprovalMode::Write);
        let approval = tools.approval.unwrap();
        assert_eq!(approval["Bash"], ToolApprovalPolicy::Prompt);
        assert_eq!(approval["Read"], ToolApprovalPolicy::Allow);
    }

    #[test]
    fn config_cli_overrides_project_config() {
        let defaults = Config::default();
        let project = MinkConfigFile {
            model: Some("pro".into()),
            max_turns: Some(99),
            ..Default::default()
        };
        let mut cfg = Config {
            model: "flash".into(),
            max_turns: 12,
            ..Default::default()
        };
        apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
        assert_eq!(cfg.model, "flash");
        assert_eq!(cfg.max_turns, 12);
    }

    #[test]
    fn config_project_overrides_user_config() {
        let defaults = Config::default();
        let user = MinkConfigFile {
            api_key: Some("user-key".into()),
            model: Some("flash".into()),
            max_turns: Some(10),
            ..Default::default()
        };
        let project = MinkConfigFile {
            api_key: Some("project-key".into()),
            model: Some("pro".into()),
            max_turns: Some(20),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), Some(&project), None);
        assert_eq!(cfg.api_key, "project-key");
        assert_eq!(cfg.model, "pro");
        assert_eq!(cfg.max_turns, 20);
    }

    #[test]
    fn config_user_overrides_default() {
        let defaults = Config::default();
        let user = MinkConfigFile {
            api_key: Some("user-key".into()),
            base_url: Some("https://user.example".into()),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
        assert_eq!(cfg.api_key, "user-key");
        assert_eq!(cfg.base_url, "https://user.example");
    }

    #[test]
    fn config_file_sets_compaction_policy() {
        let defaults = Config::default();
        let user = MinkConfigFile {
            context_compact_pct: Some(72),
            context_reserve_tokens: Some(8_000),
            context_compact_tail_tokens: Some(12_000),
            context_compact_max_output_tokens: Some(2_048),
            context_compact_input_reduction: Some(true),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
        assert_eq!(cfg.context_compact_pct, 72);
        assert_eq!(cfg.context_reserve_tokens, 8_000);
        assert_eq!(cfg.context_compact_tail_tokens, 12_000);
        assert_eq!(cfg.context_compact_max_output_tokens, 2_048);
        assert!(cfg.context_compact_input_reduction);
    }

    #[test]
    fn invalid_compaction_policy_keeps_defaults() {
        let defaults = Config::default();
        let user = MinkConfigFile {
            context_compact_pct: Some(0),
            context_reserve_tokens: Some(0),
            context_compact_tail_tokens: Some(0),
            context_compact_max_output_tokens: Some(0),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
        assert_eq!(cfg.context_compact_pct, defaults.context_compact_pct);
        assert_eq!(cfg.context_reserve_tokens, defaults.context_reserve_tokens);
        assert_eq!(
            cfg.context_compact_tail_tokens,
            defaults.context_compact_tail_tokens
        );
        assert_eq!(
            cfg.context_compact_max_output_tokens,
            defaults.context_compact_max_output_tokens
        );
    }

    #[test]
    fn runtime_config_rejects_unusable_context_budget_combinations() {
        let mut cfg = Config {
            max_context_tokens: 64_000,
            ..Config::default()
        };
        let error = validate_runtime_config(&cfg).unwrap_err().to_string();
        assert!(
            error.contains("context_reserve_tokens (64000) must be less than max_context (64000)"),
            "{error}"
        );

        cfg.context_reserve_tokens = 12_000;
        let error = validate_runtime_config(&cfg).unwrap_err().to_string();
        assert!(
            error.contains("context_compact_tail_tokens (256000) must be less than"),
            "{error}"
        );

        cfg.context_compact_tail_tokens = 16_000;
        validate_runtime_config(&cfg).unwrap();

        cfg.context_compact_max_output_tokens = 64_000;
        let error = validate_runtime_config(&cfg).unwrap_err().to_string();
        assert!(
            error.contains(
                "context_compact_max_output_tokens (64000) must be less than max_context (64000)"
            ),
            "{error}"
        );
    }

    #[test]
    fn runtime_config_allows_zero_context_window() {
        let cfg = Config {
            max_context_tokens: 0,
            ..Config::default()
        };
        validate_runtime_config(&cfg).unwrap();
    }

    #[test]
    fn config_file_sets_openai_compatible_options() {
        let defaults = Config::default();
        let project = MinkConfigFile {
            openai_reasoning_effort: Some("off".into()),
            openai_include_usage: Some(false),
            openai_token_param: Some("max_completion_tokens".into()),
            openai_tool_choice: Some(serde_json::json!("auto")),
            openai_extra_body: Some(BTreeMap::from([
                ("enable_thinking".to_string(), serde_json::json!(true)),
                ("thinking_budget".to_string(), serde_json::json!(8192)),
            ])),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
        assert_eq!(cfg.openai_reasoning_effort, None);
        assert!(!cfg.openai_include_usage);
        assert_eq!(
            cfg.openai_token_param,
            OpenAiTokenParamConfig::MaxCompletionTokens
        );
        assert_eq!(cfg.openai_tool_choice, Some(serde_json::json!("auto")));
        assert_eq!(
            cfg.openai_extra_body["enable_thinking"],
            serde_json::json!(true)
        );
        assert_eq!(
            cfg.openai_extra_body["thinking_budget"],
            serde_json::json!(8192)
        );
    }

    #[test]
    fn config_file_log_events_overrides_env_default() {
        let defaults = Config::default();
        let project = MinkConfigFile {
            log_events: Some(true),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_log_events_env_value(&mut cfg, "0");
        apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
        assert!(cfg.log_events);
    }

    #[test]
    fn config_file_invalid_llm_timeouts_are_ignored() {
        let defaults = Config::default();
        let project = MinkConfigFile {
            tool_timeout: Some(0),
            sub_agent_timeout: Some(-5),
            llm_first_event_timeout: Some(0),
            llm_idle_timeout: Some(-5),
            llm_wait_heartbeat: Some(-1),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
        assert_eq!(cfg.tool_timeout_secs, defaults.tool_timeout_secs);
        assert_eq!(cfg.sub_agent_timeout_secs, defaults.sub_agent_timeout_secs);
        assert_eq!(
            cfg.llm_first_event_timeout_secs,
            defaults.llm_first_event_timeout_secs
        );
        assert_eq!(cfg.llm_idle_timeout_secs, defaults.llm_idle_timeout_secs);
        assert_eq!(
            cfg.llm_wait_heartbeat_secs,
            defaults.llm_wait_heartbeat_secs
        );
    }

    #[test]
    fn config_parse_toml_via_cli() {
        let mut cfg = parse_args(vec![
            "--config".into(),
            "max_turns = 50\ntool_timeout = 300".into(),
        ])
        .unwrap();
        let defaults = Config::default();
        let cli = cfg.cli_config.take();
        apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
        cfg.cli_config = cli;
        assert_eq!(cfg.max_turns, 50);
        assert_eq!(cfg.tool_timeout_secs, 300);
    }

    #[test]
    fn edit_configuration_defaults_and_cli_overrides_are_typed() {
        let defaults = Config::default();
        assert_eq!(defaults.edit_mode, EditMode::Hashline);
        assert!(defaults.edit_fuzzy_match);
        assert_eq!(defaults.edit_fuzzy_threshold, 0.95);
        assert!(!defaults.edit_enforce_seen_lines);

        let cfg = parse_args(vec![
            "--edit-mode".into(),
            "replace".into(),
            "--edit-fuzzy-match".into(),
            "false".into(),
            "--edit-fuzzy-threshold".into(),
            "0.88".into(),
            "--edit-enforce-seen-lines".into(),
            "true".into(),
        ])
        .unwrap();
        assert_eq!(cfg.edit_mode, EditMode::Replace);
        assert!(!cfg.edit_fuzzy_match);
        assert_eq!(cfg.edit_fuzzy_threshold, 0.88);
        assert!(cfg.edit_enforce_seen_lines);
    }

    #[test]
    fn edit_configuration_toml_and_threshold_validation_fail_fast() {
        let file: MinkConfigFile = toml::from_str(
            "edit_mode = 'replace'\nedit_fuzzy_match = false\nedit_fuzzy_threshold = 0.9\nedit_enforce_seen_lines = true",
        )
        .unwrap();
        assert_eq!(file.edit_mode, Some(EditMode::Replace));
        assert_eq!(file.edit_fuzzy_match, Some(false));
        assert_eq!(file.edit_fuzzy_threshold, Some(0.9));
        assert_eq!(file.edit_enforce_seen_lines, Some(true));
        assert!(toml::from_str::<MinkConfigFile>("edit_mode = 'patch'").is_err());

        let mut cfg = Config {
            edit_fuzzy_threshold: f64::NAN,
            ..Config::default()
        };
        assert!(
            validate_runtime_config(&cfg)
                .unwrap_err()
                .to_string()
                .contains("finite")
        );
        cfg.edit_fuzzy_threshold = 1.01;
        assert!(validate_runtime_config(&cfg).is_err());
    }
}
