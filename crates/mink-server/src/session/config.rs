//! mink-server configuration: server + per-workspace agent config.

use std::path::PathBuf;

/// Server-level configuration (mink-server.toml / environment).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Mink home root (default `$HOME`, or `MINK_HOME`). Session directories
    /// live at `mink_home/.mink/projects/<project_key>/<session_id>` — the
    /// same layout the CLI/TUI uses.
    pub mink_home: PathBuf,
    /// Default model for new sessions.
    pub model: String,
    /// Maximum concurrently running sessions.
    pub max_running: usize,
    /// Idle sessions are kept open at most this long before auto-close (secs).
    pub idle_close_secs: u64,
}

impl ServerConfig {
    /// Load configuration from `mink-server.toml`（显式参数）+ `~/.minkrc`（默认）+ 环境变量。
    /// 优先级：环境变量 > mink-server.toml > ~/.minkrc > 默认值。
    pub fn load(toml_path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        let file_cfg = toml_path.map(parse_toml).transpose()?.unwrap_or_default();
        // 默认读取用户 home 目录下的 ~/.minkrc（与 TUI/CLI 共享同一配置文件）
        let user_rc = read_user_minkrc();

        let mink_home = env_or("MINK_HOME")
            .map(PathBuf::from)
            .or(file_cfg.mink_home)
            .unwrap_or_else(default_home);
        let model = env_or("MODEL")
            .or(file_cfg.model)
            .or(user_rc.model)
            .unwrap_or_else(|| "flash".to_string());

        // Base mink config: environment keys are the canonical source for a
        // single-user deployment (same variables the TUI/CLI honours).
        Ok(ServerConfig {
            host: env_or("MINK_SERVER_HOST")
                .or(file_cfg.host)
                .unwrap_or_else(|| "0.0.0.0".to_string()),
            port: env_parsed_or("MINK_SERVER_PORT", file_cfg.port).unwrap_or(8765),
            mink_home,
            model,
            max_running: env_parsed_or("MINK_SERVER_MAX_RUNNING", file_cfg.max_running)
                .unwrap_or(4),
            idle_close_secs: file_cfg.idle_close_secs.unwrap_or(1800),
        })
    }
}

/// 读取用户 home 目录下的 `~/.minkrc`（TOML 顶层字段，与 mink-core 的 MinkConfigFile 兼容）。
/// 缺失或解析失败时静默返回默认值（不阻断启动）。
fn read_user_minkrc() -> TomlConfig {
    #[derive(serde::Deserialize)]
    struct UserRc {
        model: Option<String>,
    }
    let home = env_or("HOME").unwrap_or_default();
    let path = std::path::Path::new(&home).join(".minkrc");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return TomlConfig::default();
    };
    let Ok(rc) = toml::from_str::<UserRc>(&text) else {
        eprintln!("[mink-server] warning: failed to parse ~/.minkrc");
        return TomlConfig::default();
    };
    TomlConfig {
        model: rc.model,
        host: None,
        port: None,
        mink_home: None,
        max_running: None,
        idle_close_secs: None,
    }
}

#[derive(Debug, Default, Clone)]
struct TomlConfig {
    host: Option<String>,
    port: Option<u16>,
    mink_home: Option<PathBuf>,
    model: Option<String>,
    max_running: Option<usize>,
    idle_close_secs: Option<u64>,
}

fn parse_toml(path: &std::path::Path) -> anyhow::Result<TomlConfig> {
    let text = std::fs::read_to_string(path)?;
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        server: Server,
        #[serde(default)]
        agent: Agent,
    }
    #[derive(Default, serde::Deserialize)]
    struct Server {
        host: Option<String>,
        port: Option<u16>,
        max_running: Option<usize>,
        idle_close_secs: Option<u64>,
    }
    #[derive(Default, serde::Deserialize)]
    struct Agent {
        mink_home: Option<PathBuf>,
        model: Option<String>,
    }
    let f: File = toml::from_str(&text)?;
    Ok(TomlConfig {
        host: f.server.host,
        port: f.server.port,
        mink_home: f.agent.mink_home,
        model: f.agent.model,
        max_running: f.server.max_running,
        idle_close_secs: f.server.idle_close_secs,
    })
}

fn default_home() -> PathBuf {
    env_or("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_or(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn env_parsed_or<T>(key: &str, fallback: Option<T>) -> Option<T>
where
    T: std::str::FromStr,
{
    env_or(key)
        .and_then(|value| value.parse().ok())
        .or(fallback)
}

/// Validate that the server can start: home exists or can be created.
pub fn validate_runtime_config(cfg: &ServerConfig) -> anyhow::Result<()> {
    if !cfg.mink_home.exists() {
        std::fs::create_dir_all(&cfg.mink_home)?;
    }
    if !cfg.mink_home.is_dir() {
        anyhow::bail!("MINK_HOME is not a directory: {}", cfg.mink_home.display());
    }
    if cfg.max_running == 0 {
        anyhow::bail!("max_running must be >= 1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_env_are_serialized_within_one_test() {
        // env var tests must not run in parallel with other env writes.
        let home = std::env::var_os("MINK_HOME");
        unsafe { std::env::set_var("MINK_HOME", "/tmp/unit-mink-home") };
        let cfg = ServerConfig::load(None).unwrap();
        assert_eq!(cfg.mink_home, PathBuf::from("/tmp/unit-mink-home"));
        unsafe { std::env::remove_var("MINK_HOME") };
        let cfg = ServerConfig::load(None).unwrap();
        assert!(!cfg.model.is_empty());
        assert!(cfg.port > 0);
        if let Some(home) = home {
            unsafe { std::env::set_var("MINK_HOME", home) };
        }
    }
}

#[test]
fn user_minkrc_model_is_default_when_no_env() {
    // read_user_minkrc 读 $HOME/.minkrc——临时 HOME + 临时 .minkrc
    let home = std::env::var_os("HOME");
    let rc_path = std::path::Path::new("/tmp/mink-rc-test").join(".minkrc");
    std::fs::create_dir_all("/tmp/mink-rc-test").unwrap();
    std::fs::write(&rc_path, "model = \"deepseek-v4\"\n").unwrap();
    unsafe { std::env::set_var("HOME", "/tmp/mink-rc-test") };
    unsafe { std::env::remove_var("MODEL") };
    let cfg = ServerConfig::load(None).unwrap();
    assert_eq!(cfg.model, "deepseek-v4");
    // 环境变量优先级高于 .minkrc
    unsafe { std::env::set_var("MODEL", "env-model") };
    let cfg = ServerConfig::load(None).unwrap();
    assert_eq!(cfg.model, "env-model");
    unsafe { std::env::remove_var("MODEL") };
    if let Some(home) = home {
        unsafe { std::env::set_var("HOME", home) };
    }
    let _ = std::fs::remove_dir_all("/tmp/mink-rc-test");
}
