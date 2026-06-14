use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    StreamJson,
}

/// Model tier — only two options. Maps internally to DeepSeek API model names.
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

/// TOML config file structure (optional, loaded from ~/.minkrc or <project>/.minkrc).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MinkConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
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
    pub log_events: Option<bool>,
    pub output_format: Option<String>,
    pub approval_mode: Option<String>,
    pub enabled_tools: Option<Vec<String>>,
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
    pub allow_bash: Option<bool>,
    pub bash_allow_commands: Option<Vec<String>>,
    pub allow_python: Option<bool>,
    pub allow_network: Option<bool>,
    pub allow_sub_agent: Option<bool>,
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
    /// 是否启用 PythonSandbox 工具（默认禁用，需显式开启）
    pub enable: Option<bool>,
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
    pub max_tokens: i32,
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub llm_first_event_timeout_secs: i32,
    pub llm_idle_timeout_secs: i32,
    pub llm_wait_heartbeat_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    pub max_search_files: usize,
    pub max_search_results: usize,
    pub output_format: OutputFormat,
    pub verbose: bool,
    pub tui_mode: bool,
    pub api_key: String,
    pub base_url: String,
    pub prompt: String,
    pub max_turns: i32,
    pub max_context_tokens: usize,
    pub context_compact_pct: u8,
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
    /// 工具禁用开关（从 CLI 或 Agent JSONL 加载）
    /// 从 --config CLI 参数解析的 TOML 配置（最高优先级，在 apply_config_sources 中应用）
    pub cli_config: Option<MinkConfigFile>,
    pub tool_disable: ToolDisableFlags,
    /// 工具白名单：仅启用列表中的工具。空/None 表示全部启用。
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 81920,
            tool_timeout_secs: 600,
            sub_agent_timeout_secs: 300,
            llm_first_event_timeout_secs: 60,
            llm_idle_timeout_secs: 90,
            llm_wait_heartbeat_secs: 30,
            tool_result_max_bytes: 100_000,
            file_write_max_bytes: 1_048_576,
            max_search_files: 5000,
            max_search_results: 1000,
            output_format: OutputFormat::Human,
            verbose: false,
            tui_mode: false,
            api_key: String::new(),
            base_url: String::new(),
            prompt: String::new(),
            max_turns: 40,
            max_context_tokens: 1_000_000,
            context_compact_pct: 85,
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
            tool_disable: ToolDisableFlags::default(),
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
                ModelTier::parse(&val)?; // validate
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
                cfg.tui_mode = true;
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
            "--disable-bash" => {
                cfg.tool_disable.disable_bash = true;
                i += 1;
            }
            "--disable-sub-agent" => {
                cfg.tool_disable.disable_sub_agent = true;
                i += 1;
            }
            "--disable-web" => {
                cfg.tool_disable.disable_web = true;
                i += 1;
            }
            "--disable-python" => {
                cfg.tool_disable.disable_python = true;
                i += 1;
            }
            "--enable-python-sandbox" => {
                cfg.tool_disable.disable_python_sandbox = false;
                i += 1;
            }
            "--config" => {
                let toml_str = require_value(&args, i)?;
                if let Ok(cc) = toml::from_str::<MinkConfigFile>(&toml_str) {
                    cfg.cli_config = Some(cc);
                }
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

pub fn apply_config_file(cfg: &mut Config) {
    let defaults = Config::default();
    apply_env_defaults(cfg, &defaults);
    // SDK 协议模式：所有配置已通过 --config TOML 传入，跳过文件 I/O
    if cfg.agent_jsonl {
        let cli_cfg = cfg.cli_config.take();
        apply_config_sources(cfg, &defaults, None, None, cli_cfg.as_ref());
        apply_sandbox_config(cfg, None, None, cli_cfg.as_ref());
        cfg.cli_config = cli_cfg;
        return;
    }
    // Priority: CLI > project .minkrc > user ~/.minkrc > env > default.
    // CLI is inferred by comparing the already-parsed config to defaults.
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    let user_cfg = read_config_file(&home.join(".minkrc"));
    let project_cfg = read_config_file(&cwd.join(".minkrc"));
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
}

fn apply_env_defaults(cfg: &mut Config, defaults: &Config) {
    if cfg.log_events == defaults.log_events
        && let Ok(v) = std::env::var("LOG_EVENTS")
    {
        apply_log_events_env_value(cfg, v.as_str());
    }
}

fn apply_log_events_env_value(cfg: &mut Config, value: &str) {
    cfg.log_events = value != "0" && value != "false" && value != "no";
}

fn read_config_file(path: &std::path::Path) -> Option<MinkConfigFile> {
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
            return None;
        }
    };
    match toml::from_str(&data) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!(
                "[mink] Warning: failed to parse config file {}: {}",
                path.display(),
                e
            );
            None
        }
    }
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

    for toml_cfg in [user_cfg, project_cfg, cli_cfg].into_iter().flatten() {
        if !cli_model && let Some(model) = &toml_cfg.model {
            cfg.model = model.clone();
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
            cfg.context_compact_pct = context_compact_pct;
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
        if let Some(ref v) = toml_cfg.enabled_tools {
            cfg.enabled_tools = Some(v.clone());
        }
        if !cli_tool_approval_mode && let Some(ref v) = toml_cfg.approval_mode {
            if let Ok(m) = ToolApprovalMode::parse(v) {
                cfg.tool_approval_mode = m;
            }
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
            if let Some(v) = sb.allow_bash {
                cfg.sandbox.allow_bash = v;
            }
            if let Some(ref v) = sb.bash_allow_commands {
                cfg.sandbox.bash_allow_commands = v.clone();
            }
            if let Some(v) = sb.allow_python {
                cfg.sandbox.allow_python = v;
            }
            if let Some(v) = sb.allow_network {
                cfg.sandbox.allow_network = v;
            }
            if let Some(v) = sb.allow_sub_agent {
                cfg.sandbox.allow_sub_agent = v;
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
            if let Some(v) = sp.enable {
                cfg.tool_disable.disable_python_sandbox = !v;
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

/// Resolve the actual API model name from a Config model string.
/// "flash" → "deepseek-v4-flash", "pro" → "deepseek-v4-pro"
pub fn resolve_model_name(model: &str) -> &'static str {
    ModelTier::parse(model)
        .map(|t| t.model_name())
        .unwrap_or("deepseek-v4-flash")
}

/// Resolve the display label for the title bar.
pub fn resolve_model_label(model: &str) -> &'static str {
    ModelTier::parse(model)
        .map(|t| t.label())
        .unwrap_or("flash")
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
    /// 允许 Bash 工具
    pub allow_bash: bool,
    /// 允许的 Bash 命令白名单（空 = 只用危险命令黑名单）
    pub bash_allow_commands: Vec<String>,
    /// 允许 Python 脚本执行
    pub allow_python: bool,
    /// 允许网络访问（含 LLM API 调用）
    pub allow_network: bool,
    /// 允许 SubAgent
    pub allow_sub_agent: bool,
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
            allow_bash: true,
            bash_allow_commands: Vec::new(),
            allow_python: true,
            allow_network: true,
            allow_sub_agent: true,
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

// ── Tool disable flags (from CLI / Agent JSONL) ─────────────────

/// 工具级别的运行时禁用开关。
/// 来源：CLI `--disable-bash` 等，或 Agent JSONL `options.disable_*`。
/// 注意：与 SandboxConfig 中的 allow_* 是独立的层次 —
/// SandboxConfig 决定沙箱策略，ToolDisableFlags 是运行时覆盖。
#[derive(Debug, Clone)]
pub struct ToolDisableFlags {
    pub disable_bash: bool,
    pub disable_sub_agent: bool,
    pub disable_web: bool,
    pub disable_python: bool,
    /// 默认禁用 PythonSandbox，避免与宿主 Python 混用
    /// 通过 --enable-python-sandbox 或 .minkrc 中的设置启用
    pub disable_python_sandbox: bool,
}

impl Default for ToolDisableFlags {
    fn default() -> Self {
        Self {
            disable_bash: false,
            disable_sub_agent: false,
            disable_web: false,
            disable_python: false,
            disable_python_sandbox: true, // 默认禁用
        }
    }
}

/// Tool name → disable flag check mapping. Shared by Config and ToolConfig for tool filtering.
pub(crate) type ToolDisableCheck = fn(&ToolDisableFlags) -> bool;
pub(crate) const TOOL_DISABLE_MAP: &[(&str, ToolDisableCheck)] = &[
    ("Bash", |f| f.disable_bash),
    ("Python", |f| f.disable_python),
    ("WebSearch", |f| f.disable_web),
    ("WebFetch", |f| f.disable_web),
    ("SubAgent", |f| f.disable_sub_agent),
    ("PythonSandbox", |f| f.disable_python_sandbox),
];

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
    fn parse_args_model_rejects_unknown() {
        assert!(parse_args(vec!["-m".into(), "gpt-4".into()]).is_err());
    }

    #[test]
    fn parse_args_flags() {
        let cfg = parse_args(vec!["-v".into(), "-i".into(), "--print".into()]).unwrap();
        assert!(cfg.verbose);
        assert!(cfg.interactive);
        assert_eq!(cfg.output_format, OutputFormat::StreamJson);
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
        apply_config_file(&mut cfg);
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
    fn config_file_sets_context_compact_pct() {
        let defaults = Config::default();
        let user = MinkConfigFile {
            context_compact_pct: Some(72),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
        assert_eq!(cfg.context_compact_pct, 72);
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
}
