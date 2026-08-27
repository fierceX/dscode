use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use mink::runtime::{
    EditMode, OutputFormat, SandboxConfig, SandboxPythonConfig, SignalPolicy, TokenParamKind,
    ToolApprovalMode, ToolApprovalPolicy,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliEarlyExit {
    Help,
    Version,
}

impl std::fmt::Display for CliEarlyExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Help => f.write_str("help requested"),
            Self::Version => f.write_str("version requested"),
        }
    }
}

impl std::error::Error for CliEarlyExit {}

/// TOML config file structure (optional, loaded from ~/.minkrc or <project>/.minkrc).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinkConfigFile {
    #[serde(default)]
    pub provider: ProviderConfigFile,
    #[serde(default)]
    pub generation: GenerationConfigFile,
    #[serde(default)]
    pub context: ContextConfigFile,
    #[serde(default)]
    pub tools: ToolsConfigFile,
    #[serde(default)]
    pub signal: SignalPolicyFile,
    #[serde(default)]
    pub sandbox: SandboxConfigFile,
    #[serde(default)]
    pub sandbox_python: SandboxPythonConfigFile,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub model_aliases: Option<BTreeMap<String, String>>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: Option<bool>,
    pub openai_token_param: Option<String>,
    pub openai_tool_choice: Option<serde_json::Value>,
    pub openai_extra_body: Option<BTreeMap<String, serde_json::Value>>,
    /// Explicit image-input capability: "on" | "off". Overrides the
    /// backend declaration (v7 §3.1).
    pub image_input: Option<String>,
    /// Model ids declared image-capable. When set, this replaces the built-in
    /// vision model list (empty list disables image capture entirely).
    pub vision_models: Option<Vec<String>>,
    /// `[provider.image]` — per-field image limit overrides applied on top
    /// of the resolved capability (never enables an Unsupported session).
    pub image: ImageConfigFile,
}

/// `[provider.image]` TOML section (all fields optional). Byte/pixel values
/// accept plain integers or K/M/G suffixes (e.g. "32M").
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfigFile {
    pub detail: Option<String>,
    pub max_images_per_request: Option<usize>,
    pub max_image_bytes_per_request: Option<String>,
    pub max_image_bytes: Option<String>,
    pub max_dimension: Option<u32>,
    pub max_pixels: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationConfigFile {
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub llm_first_event_timeout: Option<i32>,
    pub llm_idle_timeout: Option<i32>,
    pub llm_wait_heartbeat: Option<i32>,
    pub log_events: Option<bool>,
    pub output_format: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextConfigFile {
    pub max_context: Option<String>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
    pub plan_projection_tail: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditConfigFile {
    pub mode: Option<EditMode>,
    pub fuzzy_match: Option<bool>,
    pub fuzzy_threshold: Option<f64>,
    pub enforce_seen_lines: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfigFile {
    pub approval_mode: Option<ToolApprovalMode>,
    pub approval: Option<BTreeMap<String, ToolApprovalPolicy>>,
    pub enabled_tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub tool_timeout: Option<i32>,
    /// 单次 Bash/Python/自定义工具调用的超时上限（默认 600，至少 5）。
    pub tool_timeout_max: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub max_search_files: Option<usize>,
    pub max_search_results: Option<usize>,
    pub edit: EditConfigFile,
}

/// `[signal]` TOML section：全部字段可省略，未提供的字段保留 CliConfig 默认值。
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SignalPolicyFile {
    pub policy: Option<SignalPolicy>,
}

/// The `[sandbox]` section in .minkrc (all fields optional, inherits defaults).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone)]
pub struct CliConfig {
    pub model: String,
    pub model_aliases: BTreeMap<String, String>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: bool,
    pub openai_token_param: TokenParamKind,
    pub openai_tool_choice: Option<serde_json::Value>,
    pub openai_extra_body: BTreeMap<String, serde_json::Value>,
    pub max_tokens: i32,
    pub tool_timeout_secs: i32,
    /// 单次 Bash/Python/自定义工具调用的超时上限（默认 600 秒，至少 5 秒）。
    pub tool_timeout_max_secs: i32,
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
    /// Project the confirmed plan as the **last** message (default true) so
    /// plan edits stay outside the cacheable prefix; false restores the legacy
    /// head projection (after leading system messages) for A/B fallback.
    pub plan_projection_tail: bool,
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
    /// Prefab template to seed/restructure with; `Some("default")` uses the
    /// bundled generic template, `Some(path)` loads a template directory.
    pub prefab: Option<String>,
    /// Router mode; `Some("flash")` enables the Flash routing backend.
    pub router: Option<String>,
    /// Explicit image-input capability: `Some(cap)` overrides the backend
    /// declaration; `None` defers to it (v7 §3.1).
    pub image_input: Option<mink::runtime::ImageInputCapability>,
    /// Explicit vision model ids; `Some(list)` replaces the built-in default
    /// list (empty disables image capture), `None` keeps the built-in list.
    pub vision_models: Option<Vec<String>>,
    /// `[provider.image]` overrides, merged across config-file layers
    /// (higher layer wins per field).
    pub image_limits: Option<mink::runtime::ImageLimitsOverrides>,
    /// 从 --config CLI 参数解析的 TOML 配置（最高优先级，在 apply_config_sources 中应用）
    pub cli_config: Option<MinkConfigFile>,
    /// 工具选择：`None` 使用默认工具集；`Some(vec![])` 不启用任何工具。
    pub enabled_tools: Option<Vec<String>>,
    /// Tool approval mode.
    pub tool_approval_mode: ToolApprovalMode,
    /// Per-tool approval overrides keyed by tool name.
    pub tool_approval: BTreeMap<String, ToolApprovalPolicy>,
    pub signal_policy: SignalPolicy,
}

/// Fields explicitly provided by CLI flags. Only these inputs outrank the
/// config-file layer; env vars are applied before files and never set these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    pub model: bool,
    pub api_key: bool,
    pub base_url: bool,
    pub output_format: bool,
    pub enabled_tools: bool,
    pub skills: bool,
    pub edit_mode: bool,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: bool,
    pub edit_enforce_seen_lines: bool,
}

impl Default for CliConfig {
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
            tool_timeout_max_secs: 600,
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
            plan_projection_tail: true,
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
            prefab: None,
            router: None,
            image_input: None,
            vision_models: None,
            image_limits: None,
            enabled_tools: None,
            cli_config: None,
            tool_approval_mode: ToolApprovalMode::Yolo,
            tool_approval: BTreeMap::new(),
            signal_policy: SignalPolicy::Full,
        }
    }
}

pub fn parse_args(args: Vec<String>) -> Result<CliConfig> {
    let mut cfg = CliConfig::default();
    let mut i = 0usize;
    let mut seen_prompt = false;

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
            "--prefab" => {
                cfg.prefab = Some("default".to_string());
                i += 1;
            }
            arg if arg.starts_with("--prefab=") => {
                let value = arg.strip_prefix("--prefab=").unwrap_or_default().trim();
                if value.is_empty() {
                    bail!("missing value for --prefab");
                }
                cfg.prefab = Some(value.to_string());
                i += 1;
            }
            "--router" => {
                cfg.router = Some("flash".to_string());
                i += 1;
            }
            arg if arg.starts_with("--router=") => {
                let value = arg.strip_prefix("--router=").unwrap_or_default().trim();
                if value.is_empty() {
                    bail!("missing value for --router");
                }
                if value.eq_ignore_ascii_case("off") {
                    cfg.router = None;
                } else {
                    cfg.router = Some(value.to_string());
                }
                i += 1;
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
                cfg.cli_overrides.output_format = true;
                i += 1;
            }
            "--session" => {
                let value = require_value(&args, i)?;
                if value.trim().is_empty() {
                    bail!("session name must not be empty");
                }
                cfg.session_id = value;
                i += 2;
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
                return Err(anyhow!(CliEarlyExit::Help));
            }
            "-V" | "--version" => {
                return Err(anyhow!(CliEarlyExit::Version));
            }
            "--agent-jsonl" => {
                cfg.agent_jsonl = true;
                cfg.output_format = OutputFormat::StreamJson;
                cfg.cli_overrides.output_format = true;
                i += 1;
            }
            "--skill" => {
                let value = require_value(&args, i)?;
                if value.trim().is_empty() {
                    bail!("skill name must not be empty");
                }
                if !cfg.skills.iter().any(|s| s == &value) {
                    cfg.skills.push(value);
                }
                cfg.cli_overrides.skills = true;
                i += 2;
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
                if seen_prompt {
                    bail!(
                        "unexpected extra argument: {arg} (prompt already set to {:?})",
                        cfg.prompt
                    );
                }
                cfg.prompt = arg.clone();
                seen_prompt = true;
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
    let value = args[i + 1].clone();
    if value.starts_with('-') && value.len() > 1 {
        bail!("missing value for {}", args[i]);
    }
    Ok(value)
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

/// Parse `image_input = "on" | "off"` from TOML/env into the explicit
/// capability override (v7 §3.1 priority: explicit > backend > Unsupported).
pub fn parse_image_input(value: &str) -> Result<mink::runtime::ImageInputCapability> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Ok(mink::runtime::ImageInputCapability::OpenAiChatImageUrl(
            mink::runtime::OpenAiChatImageUrlLimits::default(),
        )),
        "off" | "false" | "0" => Ok(mink::runtime::ImageInputCapability::Unsupported),
        other => anyhow::bail!(
            "invalid image_input {other:?}: expected \"on\" or \"off\""
        ),
    }
}

/// Parse the `[provider.image]` TOML section into per-field overrides.
/// `Ok(None)` for an all-empty section; any invalid field fails the whole
/// section (callers warn and ignore).
pub fn parse_image_limits(
    file: &ImageConfigFile,
) -> Result<Option<mink::runtime::ImageLimitsOverrides>> {
    let detail = match file.detail.as_deref() {
        None => None,
        Some(value) => Some(match value.trim().to_ascii_lowercase().as_str() {
            "high" => mink::runtime::ImageDetail::High,
            "low" => mink::runtime::ImageDetail::Low,
            other => anyhow::bail!(
                "invalid image.detail {other:?}: expected \"high\" or \"low\""
            ),
        }),
    };
    let parse_u64 = |name: &str, value: &str| -> Result<u64> {
        parse_size_bytes(value)
            .map(|n| n as u64)
            .map_err(|_| anyhow!("invalid image.{name} {value:?}: expected a byte count"))
    };
    let max_image_bytes_per_request = match file.max_image_bytes_per_request.as_deref() {
        None => None,
        Some(value) => Some(parse_u64("max_image_bytes_per_request", value)?),
    };
    let max_image_bytes = match file.max_image_bytes.as_deref() {
        None => None,
        Some(value) => Some(parse_u64("max_image_bytes", value)?),
    };
    let max_pixels = match file.max_pixels.as_deref() {
        None => None,
        Some(value) => Some(parse_u64("max_pixels", value)?),
    };
    let overrides = mink::runtime::ImageLimitsOverrides {
        detail,
        max_images_per_request: file.max_images_per_request,
        max_image_bytes_per_request,
        max_image_bytes,
        max_dimension: file.max_dimension,
        max_pixels,
    };
    Ok((!overrides.is_empty()).then_some(overrides))
}

pub(crate) fn default_home() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    )
}

pub fn apply_config_file(cfg: &mut CliConfig) -> Result<()> {
    let defaults = CliConfig::default();
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
    let home = default_home();

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

fn apply_env_defaults(cfg: &mut CliConfig, defaults: &CliConfig) -> Result<()> {
    if let Ok(value) = std::env::var("MINK_SIGNAL_POLICY") {
        cfg.signal_policy = SignalPolicy::parse(&value)?;
    }
    if let Ok(value) = std::env::var("MINK_IMAGE_INPUT") {
        cfg.image_input = Some(parse_image_input(&value)?);
    }
    if let Ok(value) = std::env::var("MINK_VISION_MODELS") {
        cfg.vision_models = Some(
            value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }
    if cfg.log_events == defaults.log_events
        && let Ok(v) = std::env::var("LOG_EVENTS")
    {
        apply_log_events_env_value(cfg, v.as_str());
    }
    // Size-limit env vars sit at the env layer: applied before config files
    // so [tools] settings in .minkrc/--config outrank them (documented
    // priority: CLI > --config > project .minkrc > user .minkrc > env).
    let parse_env = |name: &str| -> Option<usize> {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
    };
    apply_size_limit_envs(
        cfg,
        parse_env("TOOL_RESULT_MAX_BYTES"),
        parse_env("FILE_WRITE_MAX_BYTES"),
        parse_env("MAX_SEARCH_FILES"),
        parse_env("MAX_SEARCH_RESULTS"),
    );
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

fn apply_size_limit_envs(
    cfg: &mut CliConfig,
    tool_result_max_bytes: Option<usize>,
    file_write_max_bytes: Option<usize>,
    max_search_files: Option<usize>,
    max_search_results: Option<usize>,
) {
    if let Some(n) = tool_result_max_bytes {
        cfg.tool_result_max_bytes = n;
    }
    if let Some(n) = file_write_max_bytes {
        cfg.file_write_max_bytes = n;
    }
    if let Some(n) = max_search_files {
        cfg.max_search_files = n;
    }
    if let Some(n) = max_search_results {
        cfg.max_search_results = n;
    }
}

fn apply_log_events_env_value(cfg: &mut CliConfig, value: &str) {
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
    cfg: &mut CliConfig,
    _defaults: &CliConfig,
    user_cfg: Option<&MinkConfigFile>,
    project_cfg: Option<&MinkConfigFile>,
    cli_cfg: Option<&MinkConfigFile>,
) {
    // CLI-provided flags outrank every file layer; snapshot the flags once so
    // the loop below can mutate `cfg` freely.
    let cli = cfg.cli_overrides.clone();

    for toml_cfg in [user_cfg, project_cfg, cli_cfg].into_iter().flatten() {
        if !cli.model
            && let Some(model) = &toml_cfg.provider.model
        {
            cfg.model = model.clone();
        }
        if let Some(model_aliases) = &toml_cfg.provider.model_aliases {
            for (alias, model) in model_aliases {
                cfg.model_aliases.insert(alias.clone(), model.clone());
            }
        }
        if let Some(reasoning_effort) = &toml_cfg.provider.openai_reasoning_effort {
            cfg.openai_reasoning_effort = normalize_openai_reasoning_effort(reasoning_effort);
        }
        if let Some(include_usage) = toml_cfg.provider.openai_include_usage {
            cfg.openai_include_usage = include_usage;
        }
        if let Some(token_param) = &toml_cfg.provider.openai_token_param {
            if let Some(parsed) = TokenParamKind::parse(token_param) {
                cfg.openai_token_param = parsed;
            } else {
                eprintln!("[mink] Warning: ignoring openai_token_param={token_param:?}");
            }
        }
        if let Some(tool_choice) = &toml_cfg.provider.openai_tool_choice {
            cfg.openai_tool_choice = Some(tool_choice.clone());
        }
        if let Some(value) = &toml_cfg.provider.image_input {
            match parse_image_input(value) {
                Ok(capability) => cfg.image_input = Some(capability),
                Err(error) => eprintln!(
                    "[mink] Warning: ignoring image_input={value:?}: {error}"
                ),
            }
        }
        if let Some(models) = &toml_cfg.provider.vision_models {
            cfg.vision_models = Some(models.clone());
        }
        // `[provider.image]` overrides: higher layers win per field; an
        // invalid section is warned and ignored entirely (same stance as
        // invalid image_input).
        match parse_image_limits(&toml_cfg.provider.image) {
            Ok(Some(overrides)) => match &mut cfg.image_limits {
                Some(existing) => existing.merge(overrides),
                None => cfg.image_limits = Some(overrides),
            },
            Ok(None) => {}
            Err(error) => eprintln!("[mink] Warning: ignoring [provider.image]: {error}"),
        }
        if let Some(extra_body) = &toml_cfg.provider.openai_extra_body {
            cfg.openai_extra_body.extend(extra_body.clone());
        }
        if !cli.api_key
            && let Some(api_key) = &toml_cfg.provider.api_key
        {
            cfg.api_key = api_key.clone();
        }
        if !cli.base_url
            && let Some(base_url) = &toml_cfg.provider.base_url
        {
            cfg.base_url = base_url.clone();
        }
        if let Some(max_tokens) = toml_cfg.generation.max_tokens {
            cfg.max_tokens = max_tokens;
        }
        if let Some(max_turns) = toml_cfg.generation.max_turns {
            cfg.max_turns = max_turns;
        }
        if let Some(max_context) = &toml_cfg.context.max_context
            && let Ok(v) = parse_size_bytes(max_context)
        {
            cfg.max_context_tokens = v;
        }
        if let Some(v) = toml_cfg.tools.max_search_files {
            cfg.max_search_files = v;
        }
        if let Some(v) = toml_cfg.tools.max_search_results {
            cfg.max_search_results = v;
        }
        if let Some(tool_timeout) = toml_cfg.tools.tool_timeout {
            apply_positive_i32_config(&mut cfg.tool_timeout_secs, tool_timeout, "tool_timeout");
        }
        if let Some(tool_timeout_max) = toml_cfg.tools.tool_timeout_max {
            apply_tool_timeout_max_config(
                &mut cfg.tool_timeout_max_secs,
                tool_timeout_max,
                "tool_timeout_max",
            );
        }
        if let Some(sub_agent_timeout) = toml_cfg.tools.sub_agent_timeout {
            apply_positive_i32_config(
                &mut cfg.sub_agent_timeout_secs,
                sub_agent_timeout,
                "sub_agent_timeout",
            );
        }
        if let Some(timeout) = toml_cfg.generation.llm_first_event_timeout {
            apply_positive_i32_config(
                &mut cfg.llm_first_event_timeout_secs,
                timeout,
                "llm_first_event_timeout",
            );
        }
        if let Some(timeout) = toml_cfg.generation.llm_idle_timeout {
            apply_positive_i32_config(&mut cfg.llm_idle_timeout_secs, timeout, "llm_idle_timeout");
        }
        if let Some(timeout) = toml_cfg.generation.llm_wait_heartbeat {
            apply_nonnegative_i32_config(
                &mut cfg.llm_wait_heartbeat_secs,
                timeout,
                "llm_wait_heartbeat",
            );
        }
        if let Some(context_compact_pct) = toml_cfg.context.context_compact_pct {
            if (1..=100).contains(&context_compact_pct) {
                cfg.context_compact_pct = context_compact_pct;
            } else {
                eprintln!(
                    "[mink] Warning: ignoring context_compact_pct={context_compact_pct}; expected 1-100"
                );
            }
        }
        if let Some(tokens) = toml_cfg.context.context_reserve_tokens {
            if tokens > 0 {
                cfg.context_reserve_tokens = tokens;
            } else {
                eprintln!("[mink] Warning: ignoring context_reserve_tokens=0");
            }
        }
        if let Some(tokens) = toml_cfg.context.context_compact_tail_tokens {
            if tokens > 0 {
                cfg.context_compact_tail_tokens = tokens;
            } else {
                eprintln!("[mink] Warning: ignoring context_compact_tail_tokens=0");
            }
        }
        if let Some(tokens) = toml_cfg.context.context_compact_max_output_tokens {
            apply_positive_i32_config(
                &mut cfg.context_compact_max_output_tokens,
                tokens,
                "context_compact_max_output_tokens",
            );
        }
        if let Some(enabled) = toml_cfg.context.context_compact_input_reduction {
            cfg.context_compact_input_reduction = enabled;
        }
        if let Some(tail) = toml_cfg.context.plan_projection_tail {
            cfg.plan_projection_tail = tail;
        }
        if let Some(log_events) = toml_cfg.generation.log_events {
            cfg.log_events = log_events;
        }
        let tools = &toml_cfg.tools;
        if let Some(mode) = tools.approval_mode {
            cfg.tool_approval_mode = mode;
        }
        if let Some(approval) = &tools.approval {
            for (name, policy) in approval {
                cfg.tool_approval.insert(name.clone(), *policy);
            }
        }
        if !cli.output_format
            && let Some(ref v) = toml_cfg.generation.output_format
        {
            match v.as_str() {
                "human" => cfg.output_format = OutputFormat::Human,
                "stream-json" => cfg.output_format = OutputFormat::StreamJson,
                _ => {}
            }
        }
        if !cli.skills
            && let Some(ref v) = toml_cfg.tools.skills
        {
            cfg.skills = v.clone();
        }
        if !cli.enabled_tools
            && let Some(ref v) = toml_cfg.tools.enabled_tools
        {
            cfg.enabled_tools = Some(v.clone());
        }
        if !cli.edit_mode
            && let Some(mode) = toml_cfg.tools.edit.mode
        {
            cfg.edit_mode = mode;
        }
        if !cli.edit_fuzzy_match
            && let Some(enabled) = toml_cfg.tools.edit.fuzzy_match
        {
            cfg.edit_fuzzy_match = enabled;
        }
        if !cli.edit_fuzzy_threshold
            && let Some(threshold) = toml_cfg.tools.edit.fuzzy_threshold
        {
            cfg.edit_fuzzy_threshold = threshold;
        }
        if !cli.edit_enforce_seen_lines
            && let Some(enabled) = toml_cfg.tools.edit.enforce_seen_lines
        {
            cfg.edit_enforce_seen_lines = enabled;
        }
        if let Some(policy) = toml_cfg.signal.policy {
            cfg.signal_policy = policy;
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

fn apply_tool_timeout_max_config(target: &mut i32, value: i32, name: &str) {
    if value >= 5 {
        *target = value;
    } else {
        eprintln!("[mink] Warning: ignoring {name}={value}; must be at least 5");
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
    cfg: &mut CliConfig,
    user_cfg: Option<&MinkConfigFile>,
    project_cfg: Option<&MinkConfigFile>,
    cli_cfg: Option<&MinkConfigFile>,
) {
    for toml_cfg in [user_cfg, project_cfg, cli_cfg].into_iter().flatten() {
        let sb = &toml_cfg.sandbox;
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
        let sp = &toml_cfg.sandbox_python;
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

    // Also check MINK_LIMITS env var (JSON format) — highest priority after CLI
    if let Ok(json) = std::env::var("MINK_LIMITS")
        && let Ok(sb) = serde_json::from_str::<SandboxConfig>(&json)
        && sb.enabled
    {
        cfg.sandbox = sb;
    }
}

pub fn apply_provider_defaults(cfg: &mut CliConfig) -> Result<()> {
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

pub fn validate_runtime_config(cfg: &CliConfig) -> Result<()> {
    // 唯一实现位于 mink-core（此前整函数复制两份且已漂移）。
    mink::runtime::validate_runtime_limits(
        cfg.edit_fuzzy_threshold,
        cfg.max_tokens,
        cfg.max_turns,
        cfg.context_compact_pct,
        cfg.context_reserve_tokens,
        cfg.context_compact_tail_tokens,
        cfg.context_compact_max_output_tokens,
        cfg.max_context_tokens,
    )?;
    if cfg.tool_timeout_max_secs < 5 {
        bail!("tool_timeout_max_secs must be at least 5 seconds");
    }
    Ok(())
}

/// Resolve the display label for the title bar.
#[cfg(feature = "tui")]
pub fn resolve_model_label(model: &str) -> String {
    crate::runtime::ModelResolver::new(&BTreeMap::new())
        .resolve(model)
        .label
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

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

pub(crate) fn apply_sdk_request_options(
    cfg: &mut CliConfig,
    request: &mink::sdk_protocol::SdkRequest,
) {
    let options = &request.options;
    if let Some(model) = &options.provider.model {
        cfg.model = model.clone();
    }
    let generation = &options.generation;
    for (value, target) in [
        (generation.max_tokens, &mut cfg.max_tokens),
        (generation.max_turns, &mut cfg.max_turns),
        (
            generation.llm_first_event_timeout,
            &mut cfg.llm_first_event_timeout_secs,
        ),
        (generation.llm_idle_timeout, &mut cfg.llm_idle_timeout_secs),
        (
            generation.llm_wait_heartbeat,
            &mut cfg.llm_wait_heartbeat_secs,
        ),
    ] {
        if let Some(value) = value {
            *target = value;
        }
    }
    let context = &options.context;
    if let Some(value) = context.max_context {
        cfg.max_context_tokens = value;
    }
    if let Some(value) = context.context_compact_pct {
        cfg.context_compact_pct = value;
    }
    if let Some(value) = context.context_reserve_tokens {
        cfg.context_reserve_tokens = value;
    }
    if let Some(value) = context.context_compact_tail_tokens {
        cfg.context_compact_tail_tokens = value;
    }
    if let Some(value) = context.context_compact_max_output_tokens {
        cfg.context_compact_max_output_tokens = value;
    }
    if let Some(value) = context.context_compact_input_reduction {
        cfg.context_compact_input_reduction = value;
    }
    let tools = &options.tools;
    if let Some(value) = tools.tool_timeout {
        cfg.tool_timeout_secs = value;
    }
    if let Some(value) = tools.tool_timeout_max {
        cfg.tool_timeout_max_secs = value;
    }
    if let Some(value) = tools.sub_agent_timeout {
        cfg.sub_agent_timeout_secs = value;
    }
    if let Some(value) = &tools.enabled_tools {
        cfg.enabled_tools = Some(value.clone());
    }
    if let Some(value) = tools.edit_mode {
        cfg.edit_mode = match value {
            mink::runtime::EditMode::Hashline => EditMode::Hashline,
            mink::runtime::EditMode::Replace => EditMode::Replace,
        };
    }
    if let Some(value) = tools.edit_fuzzy_match {
        cfg.edit_fuzzy_match = value;
    }
    if let Some(value) = tools.edit_fuzzy_threshold {
        cfg.edit_fuzzy_threshold = value;
    }
    if let Some(value) = tools.edit_enforce_seen_lines {
        cfg.edit_enforce_seen_lines = value;
    }
    if !cfg.cli_overrides.skills
        && let Some(value) = &tools.skills
    {
        cfg.skills = value.clone();
    }
    if let Some(value) = options.output.verbose {
        cfg.verbose = value;
    }
    if options.output.stream_events == Some(false) {
        cfg.output_format = OutputFormat::Human;
    }
    if let Some(value) = request
        .session_id
        .as_deref()
        .or(options.session.session_id.as_deref())
        .filter(|value| !value.is_empty())
    {
        cfg.session_id = value.to_string();
    }
    if let Some(value) = &request.mission {
        cfg.mission_content = Some(value.clone());
    }
    if let Some(value) = options.signal.policy {
        cfg.signal_policy = match value {
            mink::runtime::SignalPolicy::Off => SignalPolicy::Off,
            mink::runtime::SignalPolicy::Evidence => SignalPolicy::Evidence,
            mink::runtime::SignalPolicy::StateOps => SignalPolicy::StateOps,
            mink::runtime::SignalPolicy::Restart => SignalPolicy::Restart,
            mink::runtime::SignalPolicy::Full => SignalPolicy::Full,
        };
    }
}
