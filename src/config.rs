use anyhow::{Result, anyhow, bail};
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

/// TOML config file structure (optional, loaded from ~/.dscoderc or <project>/.dscoderc).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct DscodeConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub max_context: Option<String>, // supports K/M suffix
    pub tool_timeout: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub context_compact_pct: Option<u8>,
    pub log_events: Option<bool>,
    /// `[sandbox]` section — when enabled, dscode re-execs itself inside a sandbox.
    #[serde(default)]
    pub sandbox: Option<SandboxConfigFile>,
}

/// The `[sandbox]` section in .dscoderc (all fields optional, inherits defaults).
#[derive(Debug, Clone, serde::Deserialize)]
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

impl Default for SandboxConfigFile {
    fn default() -> Self {
        Self {
            enabled: None,
            backend: None,
            read_dirs: None,
            write_dirs: None,
            allow_bash: None,
            bash_allow_commands: None,
            allow_python: None,
            allow_network: None,
            allow_sub_agent: None,
            max_memory_mb: None,
            max_pids: None,
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub max_tokens: i32,
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
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
    /// JSON-RPC mode: read request from stdin, emit events to stdout.
    pub json_rpc: bool,
    /// 沙箱配置（从 .dscoderc 加载）
    pub sandbox: SandboxConfig,
    /// 自定义系统提示词文件（MISSION.md）
    pub mission_file: Option<PathBuf>,
    /// 工具禁用开关（从 CLI 或 JSON-RPC 加载）
    pub tool_disable: ToolDisableFlags,
}

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub model: bool,
    pub max_tokens: bool,
    pub tool_timeout_secs: bool,
    pub sub_agent_timeout_secs: bool,
    pub api_key: bool,
    pub base_url: bool,
    pub max_turns: bool,
    pub max_context_tokens: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 81920,
            tool_timeout_secs: 600,
            sub_agent_timeout_secs: 300,
            tool_result_max_bytes: 100_000,
            file_write_max_bytes: 1_048_576,
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
            json_rpc: false,
            sandbox: SandboxConfig::default(),
            mission_file: None,
            tool_disable: ToolDisableFlags::default(),
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
            "--max-tokens" => {
                cfg.max_tokens = parse_size_bytes(&require_value(&args, i)?)? as i32;
                cfg.cli_overrides.max_tokens = true;
                i += 2;
            }
            "--tool-timeout" => {
                cfg.tool_timeout_secs = require_value(&args, i)?.parse()?;
                cfg.cli_overrides.tool_timeout_secs = true;
                i += 2;
            }
            "--sub-agent-timeout" => {
                cfg.sub_agent_timeout_secs = require_value(&args, i)?.parse()?;
                cfg.cli_overrides.sub_agent_timeout_secs = true;
                i += 2;
            }
            "--skill" => {
                cfg.skills.push(require_value(&args, i)?);
                i += 2;
            }
            "--mission" => {
                cfg.mission_file = Some(require_value(&args, i)?.into());
                i += 2;
            }
            "--max-turns" => {
                cfg.max_turns = require_value(&args, i)?.parse()?;
                cfg.cli_overrides.max_turns = true;
                i += 2;
            }
            "--max-context" => {
                let val = require_value(&args, i)?;
                cfg.max_context_tokens = parse_size_bytes(&val)
                    .map_err(|_| anyhow!("Invalid --max-context: {}", val))?;
                cfg.cli_overrides.max_context_tokens = true;
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
            "--output-format" => {
                let v = require_value(&args, i)?;
                cfg.output_format = match v.as_str() {
                    "human" => OutputFormat::Human,
                    "stream-json" => OutputFormat::StreamJson,
                    _ => bail!("unknown output format: {v}"),
                };
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
            "--json-rpc" => {
                cfg.json_rpc = true;
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
    // Priority: CLI > project .dscoderc > user ~/.dscoderc > env > default.
    // CLI is inferred by comparing the already-parsed config to defaults.
    let defaults = Config::default();
    apply_env_defaults(cfg, &defaults);
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("DSCODE_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    let user_cfg = read_config_file(&home.join(".dscoderc"));
    let project_cfg = read_config_file(&cwd.join(".dscoderc"));

    apply_config_sources(cfg, &defaults, user_cfg.as_ref(), project_cfg.as_ref());

    // Apply sandbox config: project overrides user, user overrides default
    apply_sandbox_config(cfg, user_cfg.as_ref(), project_cfg.as_ref());
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

fn read_config_file(path: &std::path::Path) -> Option<DscodeConfigFile> {
    let data = std::fs::read_to_string(path).ok()?;
    toml::from_str(&data).ok()
}

fn apply_config_sources(
    cfg: &mut Config,
    defaults: &Config,
    user_cfg: Option<&DscodeConfigFile>,
    project_cfg: Option<&DscodeConfigFile>,
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

    for toml_cfg in [user_cfg, project_cfg].into_iter().flatten() {
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
        if !cli_tool_timeout && let Some(tool_timeout) = toml_cfg.tool_timeout {
            cfg.tool_timeout_secs = tool_timeout;
        }
        if !cli_sub_agent_timeout && let Some(sub_agent_timeout) = toml_cfg.sub_agent_timeout {
            cfg.sub_agent_timeout_secs = sub_agent_timeout;
        }
        if let Some(context_compact_pct) = toml_cfg.context_compact_pct {
            cfg.context_compact_pct = context_compact_pct;
        }
        if let Some(log_events) = toml_cfg.log_events {
            cfg.log_events = log_events;
        }
    }
}

/// Apply sandbox config from TOML `[sandbox]` sections.
/// Project-level overrides user-level; both override defaults.
/// Only active when `sandbox.enabled = true` in the highest-priority config.
fn apply_sandbox_config(
    cfg: &mut Config,
    user_cfg: Option<&DscodeConfigFile>,
    project_cfg: Option<&DscodeConfigFile>,
) {
    for toml_cfg in [user_cfg, project_cfg].into_iter().flatten() {
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
    }

    // Also check DSCODE_LIMITS env var (JSON format) — highest priority after CLI
    if let Ok(json) = std::env::var("DSCODE_LIMITS") {
        if let Ok(sb) = serde_json::from_str::<SandboxConfig>(&json) {
            if sb.enabled {
                cfg.sandbox = sb;
            }
        }
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

/// 沙箱限制配置 — 从 `.dscoderc` 的 `[sandbox]` 段或环境变量 `DSCODE_LIMITS` 加载。
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

// ── Tool disable flags (from CLI / JSON-RPC) ─────────────────

/// 工具级别的运行时禁用开关。
/// 来源：CLI `--disable-bash` 等，或 JSON-RPC `options.disable_*`。
/// 注意：与 SandboxConfig 中的 allow_* 是独立的层次 —
/// SandboxConfig 决定沙箱策略，ToolDisableFlags 是运行时覆盖。
#[derive(Debug, Clone, Default)]
pub struct ToolDisableFlags {
    pub disable_bash: bool,
    pub disable_sub_agent: bool,
    pub disable_web: bool,
    pub disable_python: bool,
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
    fn parse_args_prompt() {
        let cfg = parse_args(vec!["hello world".into()]).unwrap();
        assert_eq!(cfg.prompt, "hello world");
    }

    #[test]
    fn parse_args_unknown_flag_error() {
        assert!(parse_args(vec!["--unknown".into()]).is_err());
    }

    #[test]
    fn parse_config_file_overrides_model() {
        let toml_str = r#"
model = "pro"
max_tokens = 163840
max_context = "500K"
tool_timeout = 120
"#;
        let parsed: DscodeConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.model.unwrap(), "pro");
        assert_eq!(parsed.max_tokens.unwrap(), 163840);
        assert_eq!(parsed.max_context.unwrap(), "500K");
        assert_eq!(parsed.tool_timeout.unwrap(), 120);
    }

    #[test]
    fn parse_config_file_partial_fields() {
        // Only setting one field should not require others
        let toml_str = r#"log_events = false"#;
        let parsed: DscodeConfigFile = toml::from_str(toml_str).unwrap();
        assert!(!parsed.log_events.unwrap());
        assert!(parsed.model.is_none());
        assert!(parsed.api_key.is_none());
    }

    #[test]
    fn config_cli_overrides_project_config() {
        let defaults = Config::default();
        let project = DscodeConfigFile {
            model: Some("pro".into()),
            max_turns: Some(99),
            ..Default::default()
        };
        let mut cfg = Config {
            model: "flash".into(),
            max_turns: 12,
            ..Default::default()
        };
        apply_config_sources(&mut cfg, &defaults, None, Some(&project));
        assert_eq!(cfg.model, "flash");
        assert_eq!(cfg.max_turns, 12);
    }

    #[test]
    fn config_project_overrides_user_config() {
        let defaults = Config::default();
        let user = DscodeConfigFile {
            api_key: Some("user-key".into()),
            model: Some("flash".into()),
            max_turns: Some(10),
            ..Default::default()
        };
        let project = DscodeConfigFile {
            api_key: Some("project-key".into()),
            model: Some("pro".into()),
            max_turns: Some(20),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), Some(&project));
        assert_eq!(cfg.api_key, "project-key");
        assert_eq!(cfg.model, "pro");
        assert_eq!(cfg.max_turns, 20);
    }

    #[test]
    fn config_user_overrides_default() {
        let defaults = Config::default();
        let user = DscodeConfigFile {
            api_key: Some("user-key".into()),
            base_url: Some("https://user.example".into()),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None);
        assert_eq!(cfg.api_key, "user-key");
        assert_eq!(cfg.base_url, "https://user.example");
    }

    #[test]
    fn config_file_sets_context_compact_pct() {
        let defaults = Config::default();
        let user = DscodeConfigFile {
            context_compact_pct: Some(72),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_config_sources(&mut cfg, &defaults, Some(&user), None);
        assert_eq!(cfg.context_compact_pct, 72);
    }

    #[test]
    fn config_file_log_events_overrides_env_default() {
        let defaults = Config::default();
        let project = DscodeConfigFile {
            log_events: Some(true),
            ..Default::default()
        };
        let mut cfg = Config::default();
        apply_log_events_env_value(&mut cfg, "0");
        apply_config_sources(&mut cfg, &defaults, None, Some(&project));
        assert!(cfg.log_events);
    }

    #[test]
    fn config_explicit_cli_default_value_overrides_project_config() {
        let defaults = Config::default();
        let project = DscodeConfigFile {
            max_turns: Some(99),
            tool_timeout: Some(120),
            ..Default::default()
        };
        let mut cfg = parse_args(vec![
            "--max-turns".into(),
            "40".into(),
            "--tool-timeout".into(),
            "600".into(),
        ])
        .unwrap();
        apply_config_sources(&mut cfg, &defaults, None, Some(&project));
        assert_eq!(cfg.max_turns, 40);
        assert_eq!(cfg.tool_timeout_secs, 600);
    }
}
