use anyhow::{Result, anyhow, bail};

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
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DscodeConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub max_context: Option<String>,   // supports K/M suffix
    pub tool_timeout: Option<i32>,
    pub context_compact_pct: Option<u8>,
    pub log_events: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub max_tokens: i32,
    pub tool_timeout_secs: i32,
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
    pub skills: Vec<String>,
    pub interactive: bool,
    pub session_id: String,
    pub continue_session: bool,
    pub list_sessions: bool,
    pub list_skills: bool,
    pub log_events: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 81920,
            tool_timeout_secs: 600,
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
            skills: Vec::new(),
            interactive: false,
            session_id: String::new(),
            continue_session: false,
            list_sessions: false,
            list_skills: false,
            log_events: true,
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
                ModelTier::parse(&val)?;  // validate
                cfg.model = val;
                i += 2;
            }
            "--max-tokens" => {
                cfg.max_tokens = parse_size_bytes(&require_value(&args, i)?)? as i32;
                i += 2;
            }
            "--tool-timeout" => {
                cfg.tool_timeout_secs = require_value(&args, i)?.parse()?;
                i += 2;
            }
            "--skill" => {
                cfg.skills.push(require_value(&args, i)?);
                i += 2;
            }
            "--max-turns" => {
                cfg.max_turns = require_value(&args, i)?.parse()?;
                i += 2;
            }
            "--max-context" => {
                let val = require_value(&args, i)?;
                cfg.max_context_tokens = parse_size_bytes(&val)
                    .map_err(|_| anyhow!("Invalid --max-context: {}", val))?;
                i += 2;
            }
            "--api-key" => {
                cfg.api_key = require_value(&args, i)?;
                i += 2;
            }
            "--base-url" => {
                cfg.base_url = require_value(&args, i)?;
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
    // Priority: project .dscoderc > user ~/.dscoderc
    // Each field only fills if cfg equivalent is empty/still default
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("DSCODE_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    let paths = [
        home.join(".dscoderc"),             // user-level
        cwd.join(".dscoderc"),              // project-level
    ];

    for path in &paths {
        let Ok(data) = std::fs::read_to_string(path) else { continue };
        let Ok(toml_cfg): Result<DscodeConfigFile, _> = toml::from_str(&data) else { continue };
        // Apply only if cfg field is still at default/empty
        if cfg.model.is_empty() && toml_cfg.model.is_some() {
            cfg.model = toml_cfg.model.unwrap();
        }
        if cfg.api_key.is_empty() && toml_cfg.api_key.is_some() {
            cfg.api_key = toml_cfg.api_key.unwrap();
        }
        if cfg.base_url.is_empty() && toml_cfg.base_url.is_some() {
            cfg.base_url = toml_cfg.base_url.unwrap();
        }
        if cfg.max_tokens == 81920 && toml_cfg.max_tokens.is_some() {
            cfg.max_tokens = toml_cfg.max_tokens.unwrap();
        }
        if cfg.max_turns == 40 && toml_cfg.max_turns.is_some() {
            cfg.max_turns = toml_cfg.max_turns.unwrap();
        }
        if cfg.max_context_tokens == 1_000_000 && toml_cfg.max_context.is_some() {
            if let Ok(v) = parse_size_bytes(&toml_cfg.max_context.unwrap()) {
                cfg.max_context_tokens = v;
            }
        }
        if cfg.tool_timeout_secs == 600 && toml_cfg.tool_timeout.is_some() {
            cfg.tool_timeout_secs = toml_cfg.tool_timeout.unwrap();
        }
        if toml_cfg.context_compact_pct.is_some() {
            unsafe { std::env::set_var("CONTEXT_COMPACT_PCT", &toml_cfg.context_compact_pct.unwrap().to_string()); }
        }
        if toml_cfg.log_events.is_some() {
            cfg.log_events = toml_cfg.log_events.unwrap();
        }
    }
}

    pub fn apply_provider_defaults(cfg: &mut Config) -> Result<()> {
    // Env var overrides for size limits
    if let Ok(v) = std::env::var("TOOL_RESULT_MAX_BYTES")
        && let Ok(n) = v.parse::<usize>() {
            cfg.tool_result_max_bytes = n;
        }
    if let Ok(v) = std::env::var("FILE_WRITE_MAX_BYTES")
        && let Ok(n) = v.parse::<usize>() {
            cfg.file_write_max_bytes = n;
        }
    if let Ok(v) = std::env::var("LOG_EVENTS") {
        cfg.log_events = v != "0" && v != "false" && v != "no";
    }

    // API key: DEEPSEEK_API_KEY > CLI flag
    if cfg.api_key.is_empty() {
        cfg.api_key = std::env::var("DEEPSEEK_API_KEY")
            .unwrap_or_default();
    }
    // Base URL: DEEPSEEK_BASE_URL > CLI flag > default
    if cfg.base_url.is_empty() {
        cfg.base_url = std::env::var("DEEPSEEK_BASE_URL")
            .unwrap_or_default();
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

