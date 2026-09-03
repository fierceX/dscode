//! Full replica of the CLI's `.minkrc` agent configuration for mink-server.
//!
//! The CLI (`mink-cli/src/config.rs`) owns the canonical `MinkConfigFile`
//! parsing, but mink-server cannot depend on the `publish = false` CLI crate.
//! This module mirrors that file shape field-for-field (grouped TOML sections,
//! `deny_unknown_fields`), resolves the same layered precedence
//! (project `.minkrc` overrides user `~/.minkrc`), applies the server's
//! documented environment overrides (env wins), and converts the result into
//! the same grouped `AgentOptions` the CLI builds.
//!
//! Keep the structs below in sync with `mink-cli::config::MinkConfigFile`.

use mink::runtime::{
    AgentOptions, ContextPolicy, EditMode, GenerationOptions, ProviderOptions, SandboxConfig,
    SandboxPythonConfig, SignalPolicy, TokenParamKind, ToolApprovalMode, ToolApprovalPolicy,
    ToolOptions,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// TOML config file structure — grouped format, same as the CLI.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MinkConfigFile {
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
pub(crate) struct ProviderConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub model_aliases: Option<BTreeMap<String, String>>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: Option<bool>,
    pub openai_token_param: Option<String>,
    pub openai_tool_choice: Option<Value>,
    pub openai_extra_body: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GenerationConfigFile {
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
pub(crate) struct ContextConfigFile {
    pub max_context: Option<String>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EditConfigFile {
    pub mode: Option<EditMode>,
    pub fuzzy_match: Option<bool>,
    pub fuzzy_threshold: Option<f64>,
    pub enforce_seen_lines: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ToolsConfigFile {
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

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SignalPolicyFile {
    pub policy: Option<SignalPolicy>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SandboxConfigFile {
    pub enabled: Option<bool>,
    pub backend: Option<String>,
    pub read_dirs: Option<Vec<String>>,
    pub write_dirs: Option<Vec<String>>,
    pub allow_network: Option<bool>,
    pub max_memory_mb: Option<u64>,
    pub max_pids: Option<u32>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SandboxPythonConfigFile {
    pub wasm_path: Option<String>,
    pub stdlib_dir: Option<String>,
    pub timeout: Option<u64>,
    pub read_dirs: Option<Vec<String>>,
    pub write_dirs: Option<Vec<String>>,
    pub package_dirs: Option<Vec<String>>,
}

/// Resolved agent configuration layer (one file layer or the merged result).
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentConfig {
    pub model: Option<String>,
    pub model_aliases: BTreeMap<String, String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub openai_reasoning_effort: Option<String>,
    pub openai_include_usage: Option<bool>,
    pub openai_token_param: Option<TokenParamKind>,
    pub openai_tool_choice: Option<Value>,
    pub openai_extra_body: BTreeMap<String, Value>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub llm_first_event_timeout_secs: Option<i32>,
    pub llm_idle_timeout_secs: Option<i32>,
    pub llm_wait_heartbeat_secs: Option<i32>,
    pub log_events: Option<bool>,
    pub max_context_tokens: Option<usize>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
    pub tool_timeout_secs: Option<i32>,
    pub tool_timeout_max_secs: Option<i32>,
    pub sub_agent_timeout_secs: Option<i32>,
    pub max_search_files: Option<usize>,
    pub max_search_results: Option<usize>,
    pub enabled_tools: Option<Vec<String>>,
    pub approval_mode: Option<ToolApprovalMode>,
    pub approval: BTreeMap<String, ToolApprovalPolicy>,
    pub skills: Option<Vec<String>>,
    pub edit_mode: Option<EditMode>,
    pub edit_fuzzy_match: Option<bool>,
    pub edit_fuzzy_threshold: Option<f64>,
    pub edit_enforce_seen_lines: Option<bool>,
    pub signal_policy: Option<SignalPolicy>,
    pub sandbox: SandboxConfigFile,
    pub sandbox_python: SandboxPythonConfigFile,
}

/// Parse a `.minkrc` file body into a resolved layer. `None` on read/parse
/// failure (non-blocking, same as the CLI's per-file tolerance for missing
/// files; malformed files warn instead of failing startup).
pub(crate) fn parse_layer(text: &str, origin: &str) -> AgentConfig {
    match toml::from_str::<MinkConfigFile>(text) {
        Ok(file) => from_file(&file),
        Err(error) => {
            eprintln!("[mink-server] warning: failed to parse {origin}: {error}");
            AgentConfig::default()
        }
    }
}

/// Read the user-level layer from `~/.minkrc`（与 TUI/CLI 同一配置文件）。
pub(crate) fn load_user_layer() -> AgentConfig {
    let Ok(home) = std::env::var("HOME") else {
        return AgentConfig::default();
    };
    let path = Path::new(&home).join(".minkrc");
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_layer(&text, &path.display().to_string()),
        Err(_) => AgentConfig::default(),
    }
}

/// Read the project-level layer from `<cwd>/.minkrc`（CLI 同款项目级覆盖）。
pub(crate) fn load_project_layer(cwd: &Path) -> AgentConfig {
    let path = cwd.join(".minkrc");
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_layer(&text, &path.display().to_string()),
        Err(_) => AgentConfig::default(),
    }
}

/// Layer merge: every field of `over` replaces `base` when present
/// (project over user, exactly like the CLI's layered application order).
pub(crate) fn merge(base: AgentConfig, over: AgentConfig) -> AgentConfig {
    fn pick<T>(base: Option<T>, over: Option<T>) -> Option<T> {
        over.or(base)
    }
    AgentConfig {
        model: pick(base.model, over.model),
        model_aliases: {
            let mut aliases = base.model_aliases;
            aliases.extend(over.model_aliases);
            aliases
        },
        api_key: pick(base.api_key, over.api_key),
        base_url: pick(base.base_url, over.base_url),
        openai_reasoning_effort: pick(base.openai_reasoning_effort, over.openai_reasoning_effort),
        openai_include_usage: pick(base.openai_include_usage, over.openai_include_usage),
        openai_token_param: pick(base.openai_token_param, over.openai_token_param),
        openai_tool_choice: pick(base.openai_tool_choice, over.openai_tool_choice),
        openai_extra_body: {
            let mut body = base.openai_extra_body;
            body.extend(over.openai_extra_body);
            body
        },
        max_tokens: pick(base.max_tokens, over.max_tokens),
        max_turns: pick(base.max_turns, over.max_turns),
        llm_first_event_timeout_secs: pick(
            base.llm_first_event_timeout_secs,
            over.llm_first_event_timeout_secs,
        ),
        llm_idle_timeout_secs: pick(base.llm_idle_timeout_secs, over.llm_idle_timeout_secs),
        llm_wait_heartbeat_secs: pick(base.llm_wait_heartbeat_secs, over.llm_wait_heartbeat_secs),
        log_events: pick(base.log_events, over.log_events),
        max_context_tokens: pick(base.max_context_tokens, over.max_context_tokens),
        context_compact_pct: pick(base.context_compact_pct, over.context_compact_pct),
        context_reserve_tokens: pick(base.context_reserve_tokens, over.context_reserve_tokens),
        context_compact_tail_tokens: pick(
            base.context_compact_tail_tokens,
            over.context_compact_tail_tokens,
        ),
        context_compact_max_output_tokens: pick(
            base.context_compact_max_output_tokens,
            over.context_compact_max_output_tokens,
        ),
        context_compact_input_reduction: pick(
            base.context_compact_input_reduction,
            over.context_compact_input_reduction,
        ),
        tool_timeout_secs: pick(base.tool_timeout_secs, over.tool_timeout_secs),
        tool_timeout_max_secs: pick(base.tool_timeout_max_secs, over.tool_timeout_max_secs),
        sub_agent_timeout_secs: pick(base.sub_agent_timeout_secs, over.sub_agent_timeout_secs),
        max_search_files: pick(base.max_search_files, over.max_search_files),
        max_search_results: pick(base.max_search_results, over.max_search_results),
        enabled_tools: pick(base.enabled_tools, over.enabled_tools),
        approval_mode: pick(base.approval_mode, over.approval_mode),
        approval: {
            let mut approval = base.approval;
            approval.extend(over.approval);
            approval
        },
        skills: pick(base.skills, over.skills),
        edit_mode: pick(base.edit_mode, over.edit_mode),
        edit_fuzzy_match: pick(base.edit_fuzzy_match, over.edit_fuzzy_match),
        edit_fuzzy_threshold: pick(base.edit_fuzzy_threshold, over.edit_fuzzy_threshold),
        edit_enforce_seen_lines: pick(base.edit_enforce_seen_lines, over.edit_enforce_seen_lines),
        signal_policy: pick(base.signal_policy, over.signal_policy),
        sandbox: merge_sandbox(base.sandbox, over.sandbox),
        sandbox_python: merge_sandbox_python(base.sandbox_python, over.sandbox_python),
    }
}

fn merge_sandbox(base: SandboxConfigFile, over: SandboxConfigFile) -> SandboxConfigFile {
    fn pick<T>(base: Option<T>, over: Option<T>) -> Option<T> {
        over.or(base)
    }
    SandboxConfigFile {
        enabled: pick(base.enabled, over.enabled),
        backend: pick(base.backend, over.backend),
        read_dirs: pick(base.read_dirs, over.read_dirs),
        write_dirs: pick(base.write_dirs, over.write_dirs),
        allow_network: pick(base.allow_network, over.allow_network),
        max_memory_mb: pick(base.max_memory_mb, over.max_memory_mb),
        max_pids: pick(base.max_pids, over.max_pids),
        timeout_secs: pick(base.timeout_secs, over.timeout_secs),
    }
}

fn merge_sandbox_python(
    base: SandboxPythonConfigFile,
    over: SandboxPythonConfigFile,
) -> SandboxPythonConfigFile {
    fn pick<T>(base: Option<T>, over: Option<T>) -> Option<T> {
        over.or(base)
    }
    SandboxPythonConfigFile {
        wasm_path: pick(base.wasm_path, over.wasm_path),
        stdlib_dir: pick(base.stdlib_dir, over.stdlib_dir),
        timeout: pick(base.timeout, over.timeout),
        read_dirs: pick(base.read_dirs, over.read_dirs),
        write_dirs: pick(base.write_dirs, over.write_dirs),
        package_dirs: pick(base.package_dirs, over.package_dirs),
    }
}

/// Server environment overrides (documented server precedence: env wins).
/// Mirrors the keys the CLI honours, applied on top of the file layers.
pub(crate) fn apply_env_overrides(cfg: &mut AgentConfig) {
    if let Ok(model) = std::env::var("MODEL")
        && !model.trim().is_empty()
    {
        cfg.model = Some(model);
    }
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY")
        && !key.trim().is_empty()
    {
        cfg.api_key = Some(key);
    }
    if let Ok(url) = std::env::var("DEEPSEEK_BASE_URL")
        && !url.trim().is_empty()
    {
        cfg.base_url = Some(url);
    }
    if let Ok(policy) = std::env::var("MINK_SIGNAL_POLICY")
        && let Ok(policy) = SignalPolicy::parse(&policy)
    {
        cfg.signal_policy = Some(policy);
    }
    if let Ok(value) = std::env::var("LOG_EVENTS")
        && let Ok(enabled) = parse_bool_env(&value)
    {
        cfg.log_events = Some(enabled);
    }
}

fn parse_bool_env(value: &str) -> Result<bool, ()> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(()),
    }
}

fn from_file(file: &MinkConfigFile) -> AgentConfig {
    let mut cfg = AgentConfig::default();
    let provider = &file.provider;
    cfg.model = provider.model.clone();
    if let Some(aliases) = &provider.model_aliases {
        cfg.model_aliases = aliases.clone();
    }
    cfg.api_key = provider.api_key.clone();
    cfg.base_url = provider.base_url.clone();
    cfg.openai_reasoning_effort = provider.openai_reasoning_effort.clone();
    cfg.openai_include_usage = provider.openai_include_usage;
    cfg.openai_token_param = provider
        .openai_token_param
        .as_deref()
        .and_then(TokenParamKind::parse);
    cfg.openai_tool_choice = provider.openai_tool_choice.clone();
    if let Some(extra_body) = &provider.openai_extra_body {
        cfg.openai_extra_body = extra_body.clone();
    }
    cfg.max_tokens = positive(file.generation.max_tokens, "max_tokens");
    cfg.max_turns = positive(file.generation.max_turns, "max_turns");
    cfg.llm_first_event_timeout_secs = positive(
        file.generation.llm_first_event_timeout,
        "llm_first_event_timeout",
    );
    cfg.llm_idle_timeout_secs = positive(file.generation.llm_idle_timeout, "llm_idle_timeout");
    cfg.llm_wait_heartbeat_secs =
        nonnegative(file.generation.llm_wait_heartbeat, "llm_wait_heartbeat");
    cfg.log_events = file.generation.log_events;
    if let Some(ref max_context) = file.context.max_context
        && let Ok(v) = parse_size_bytes(max_context)
    {
        cfg.max_context_tokens = Some(v);
    }
    if let Some(pct) = file.context.context_compact_pct {
        if (1..=100).contains(&pct) {
            cfg.context_compact_pct = Some(pct);
        } else {
            eprintln!("[mink-server] Warning: ignoring context_compact_pct={pct}; expected 1-100");
        }
    }
    cfg.context_reserve_tokens = file.context.context_reserve_tokens.filter(|v| *v > 0);
    cfg.context_compact_tail_tokens = file.context.context_compact_tail_tokens.filter(|v| *v > 0);
    cfg.context_compact_max_output_tokens = positive(
        file.context.context_compact_max_output_tokens,
        "context_compact_max_output_tokens",
    );
    cfg.context_compact_input_reduction = file.context.context_compact_input_reduction;

    let tools = &file.tools;
    cfg.tool_timeout_secs = positive(tools.tool_timeout, "tool_timeout");
    cfg.tool_timeout_max_secs = positive(tools.tool_timeout_max, "tool_timeout_max");
    cfg.sub_agent_timeout_secs = positive(tools.sub_agent_timeout, "sub_agent_timeout");
    cfg.max_search_files = tools.max_search_files;
    cfg.max_search_results = tools.max_search_results;
    cfg.enabled_tools = tools.enabled_tools.clone();
    cfg.approval_mode = tools.approval_mode;
    if let Some(approval) = &tools.approval {
        cfg.approval = approval.clone();
    }
    cfg.skills = tools.skills.clone();
    cfg.edit_mode = tools.edit.mode;
    cfg.edit_fuzzy_match = tools.edit.fuzzy_match;
    cfg.edit_fuzzy_threshold = tools.edit.fuzzy_threshold;
    cfg.edit_enforce_seen_lines = tools.edit.enforce_seen_lines;
    cfg.signal_policy = file.signal.policy;
    cfg.sandbox = file.sandbox.clone();
    cfg.sandbox_python = file.sandbox_python.clone();
    cfg
}

fn positive(value: Option<i32>, name: &str) -> Option<i32> {
    value.filter(|v| {
        if *v > 0 {
            true
        } else {
            eprintln!("[mink-server] Warning: ignoring {name}={v}; expected > 0");
            false
        }
    })
}

fn nonnegative(value: Option<i32>, name: &str) -> Option<i32> {
    value.filter(|v| {
        if *v >= 0 {
            true
        } else {
            eprintln!("[mink-server] Warning: ignoring {name}={v}; expected >= 0");
            false
        }
    })
}

/// Same suffix semantics as the CLI: `k`/`m`/`g` = 1e3/1e6/1e9.
fn parse_size_bytes(raw: &str) -> anyhow::Result<usize> {
    if raw.is_empty() {
        anyhow::bail!("empty size");
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

/// Apply the merged agent config to runtime options — mirrors the CLI's
/// `assemble_runtime_options` for the agent-relevant groups. Only fields
/// present in the config layer override the runtime defaults.
pub(crate) fn apply_to(mut options: AgentOptions, cfg: &AgentConfig) -> AgentOptions {
    // NOTE: `with_provider_options` replaces the resolved model/api_key/
    // base_url with `ProviderOptions` values, so those three are re-applied
    // by the caller AFTER this function (see `Registry::build_options`).
    let mut provider = ProviderOptions::default();
    if let Some(v) = &cfg.openai_reasoning_effort {
        provider.reasoning_effort = Some(v.clone());
    }
    if let Some(v) = cfg.openai_include_usage {
        provider.include_usage = v;
    }
    if let Some(v) = cfg.openai_token_param {
        provider.token_param = v;
    }
    if let Some(v) = &cfg.openai_tool_choice {
        provider.tool_choice = Some(v.clone());
    }
    if !cfg.openai_extra_body.is_empty() {
        provider.extra_body = cfg.openai_extra_body.clone();
    }
    options = options.with_provider_options(provider);
    for (alias, model) in &cfg.model_aliases {
        options = options.with_model_alias(alias, model);
    }

    let mut generation = GenerationOptions::default();
    if let Some(v) = cfg.max_tokens {
        generation.max_tokens = v;
    }
    if let Some(v) = cfg.max_turns {
        generation.max_turns = v;
    }
    if let Some(v) = cfg.llm_first_event_timeout_secs {
        generation.first_event_timeout_secs = v;
    }
    if let Some(v) = cfg.llm_idle_timeout_secs {
        generation.idle_timeout_secs = v;
    }
    if let Some(v) = cfg.llm_wait_heartbeat_secs {
        generation.wait_heartbeat_secs = v;
    }
    options = options.with_generation_options(generation);

    let mut context = ContextPolicy::default();
    if let Some(v) = cfg.max_context_tokens {
        context.max_context_tokens = v;
    }
    if let Some(v) = cfg.context_compact_pct {
        context.compact_pct = v;
    }
    if let Some(v) = cfg.context_reserve_tokens {
        context.reserve_tokens = v;
    }
    if let Some(v) = cfg.context_compact_tail_tokens {
        context.compact_tail_tokens = v;
    }
    if let Some(v) = cfg.context_compact_max_output_tokens {
        context.compact_max_output_tokens = v;
    }
    if let Some(v) = cfg.context_compact_input_reduction {
        context.compact_input_reduction = v;
    }
    options = options.with_context_policy(context);

    let mut tools = ToolOptions::default();
    if let Some(v) = cfg.tool_timeout_secs {
        tools.timeout_secs = v;
    }
    if let Some(v) = cfg.tool_timeout_max_secs {
        tools.timeout_max_secs = v;
    }
    if let Some(v) = cfg.sub_agent_timeout_secs {
        tools.sub_agent_timeout_secs = v;
    }
    if let Some(v) = cfg.edit_mode {
        tools.edit_mode = v;
    }
    if let Some(v) = cfg.edit_fuzzy_match {
        tools.edit_fuzzy_match = v;
    }
    if let Some(v) = cfg.edit_fuzzy_threshold {
        tools.edit_fuzzy_threshold = v;
    }
    if let Some(v) = cfg.edit_enforce_seen_lines {
        tools.edit_enforce_seen_lines = v;
    }
    if let Some(v) = cfg.max_search_files {
        tools.max_search_files = v;
    }
    if let Some(v) = cfg.max_search_results {
        tools.max_search_results = v;
    }
    if let Some(v) = &cfg.enabled_tools {
        tools.enabled_tools = Some(v.clone());
    }
    if let Some(v) = cfg.approval_mode {
        tools.approval_mode = v;
    }
    if !cfg.approval.is_empty() {
        tools.approval = cfg.approval.clone();
    }
    options = options.with_tool_options(tools);

    if let Some(policy) = cfg.signal_policy {
        options = options.with_signal_policy(policy);
    }
    if let Some(enabled) = cfg.log_events {
        options = options.with_log_events(enabled);
    }
    if let Some(skills) = &cfg.skills {
        options = options.with_selected_skills(skills.iter().cloned());
    }
    options = apply_sandbox(options, &cfg.sandbox);
    options = apply_sandbox_python(options, &cfg.sandbox_python);
    options
}

fn apply_sandbox(options: AgentOptions, file: &SandboxConfigFile) -> AgentOptions {
    let mut sandbox = SandboxConfig::default();
    if let Some(v) = file.enabled {
        sandbox.enabled = v;
    }
    if let Some(ref v) = file.backend {
        sandbox.backend = v.clone();
    }
    if let Some(ref v) = file.read_dirs {
        sandbox.read_dirs = v.clone();
    }
    if let Some(ref v) = file.write_dirs {
        sandbox.write_dirs = v.clone();
    }
    if let Some(v) = file.allow_network {
        sandbox.allow_network = v;
    }
    if let Some(v) = file.max_memory_mb {
        sandbox.max_memory_mb = v;
    }
    if let Some(v) = file.max_pids {
        sandbox.max_pids = v;
    }
    if let Some(v) = file.timeout_secs {
        sandbox.timeout_secs = v;
    }
    options.with_sandbox(sandbox)
}

fn apply_sandbox_python(options: AgentOptions, file: &SandboxPythonConfigFile) -> AgentOptions {
    let mut sandbox_python = SandboxPythonConfig::default();
    if let Some(ref v) = file.wasm_path {
        sandbox_python.wasm_path = v.clone();
    }
    if let Some(ref v) = file.stdlib_dir {
        sandbox_python.stdlib_dir = v.clone();
    }
    if let Some(v) = file.timeout {
        sandbox_python.timeout = v;
    }
    if let Some(ref v) = file.read_dirs {
        sandbox_python.read_dirs = v.clone();
    }
    if let Some(ref v) = file.write_dirs {
        sandbox_python.write_dirs = v.clone();
    }
    if let Some(ref v) = file.package_dirs {
        sandbox_python.package_dirs = v.clone();
    }
    options.with_sandbox_python(sandbox_python)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_config_parses_all_sections() {
        let layer = parse_layer(
            r#"
[provider]
model = "pro"
api_key = "sk-test"
base_url = "https://example.invalid/v1"
openai_reasoning_effort = "low"
openai_include_usage = false
openai_token_param = "max_completion_tokens"
model_aliases = { fast = "flash" }
openai_extra_body = { temperature = 0.2 }

[generation]
max_tokens = 4096
max_turns = 8
llm_first_event_timeout = 30

[context]
max_context = "120k"
context_compact_pct = 80
context_reserve_tokens = 2048

[tools]
enabled_tools = ["Read", "Edit", "Bash"]
approval_mode = "write"
tool_timeout = 120
skills = ["python"]
max_search_files = 100

[tools.edit]
mode = "replace"
fuzzy_threshold = 0.9

[signal]
policy = "evidence"

[sandbox]
enabled = true
allow_network = false

[sandbox_python]
timeout = 60
"#,
            "test",
        );
        assert_eq!(layer.model.as_deref(), Some("pro"));
        assert_eq!(layer.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            layer.base_url.as_deref(),
            Some("https://example.invalid/v1")
        );
        assert_eq!(layer.openai_reasoning_effort.as_deref(), Some("low"));
        assert_eq!(layer.openai_include_usage, Some(false));
        assert_eq!(
            layer.openai_token_param,
            Some(TokenParamKind::MaxCompletionTokens)
        );
        assert_eq!(
            layer.model_aliases.get("fast").map(String::as_str),
            Some("flash")
        );
        assert_eq!(
            layer
                .openai_extra_body
                .get("temperature")
                .and_then(Value::as_f64),
            Some(0.2)
        );
        assert_eq!(layer.max_tokens, Some(4096));
        assert_eq!(layer.max_turns, Some(8));
        assert_eq!(layer.llm_first_event_timeout_secs, Some(30));
        assert_eq!(layer.max_context_tokens, Some(120_000));
        assert_eq!(layer.context_compact_pct, Some(80));
        assert_eq!(layer.context_reserve_tokens, Some(2048));
        assert_eq!(
            layer.enabled_tools.as_deref(),
            Some(&["Read".to_string(), "Edit".to_string(), "Bash".to_string()][..])
        );
        assert_eq!(layer.approval_mode, Some(ToolApprovalMode::Write));
        assert_eq!(layer.tool_timeout_secs, Some(120));
        assert_eq!(layer.skills.as_deref(), Some(&["python".to_string()][..]));
        assert_eq!(layer.edit_mode, Some(EditMode::Replace));
        assert_eq!(layer.edit_fuzzy_threshold, Some(0.9));
        assert_eq!(layer.signal_policy, Some(SignalPolicy::Evidence));
        assert_eq!(layer.sandbox.enabled, Some(true));
        assert_eq!(layer.sandbox.allow_network, Some(false));
        assert_eq!(layer.sandbox_python.timeout, Some(60));
    }

    #[test]
    fn flat_keys_are_rejected_like_the_cli() {
        let layer = parse_layer("model = \"pro\"\n", "test");
        assert_eq!(layer.model, None, "flat top-level keys must be rejected");
        assert_eq!(layer.api_key, None);
    }

    #[test]
    fn removed_plan_projection_tail_is_rejected() {
        let error = toml::from_str::<MinkConfigFile>("[context]\nplan_projection_tail = false\n")
            .unwrap_err();
        assert!(error.to_string().contains("plan_projection_tail"));
    }

    #[test]
    fn project_layer_overrides_user_layer() {
        let user = parse_layer(
            "[provider]\nmodel = \"user-model\"\napi_key = \"user-key\"\n\n[tools]\ntool_timeout = 10\n",
            "user",
        );
        let project = parse_layer(
            "[provider]\nmodel = \"project-model\"\n\n[tools]\ntool_timeout = 20\n",
            "project",
        );
        let merged = merge(user, project);
        assert_eq!(merged.model.as_deref(), Some("project-model"));
        assert_eq!(
            merged.api_key.as_deref(),
            Some("user-key"),
            "absent project fields keep user values"
        );
        assert_eq!(merged.tool_timeout_secs, Some(20));
        // Section fields merge per-field too.
        let user = parse_layer("[sandbox]\nenabled = true\n", "user");
        let project = parse_layer("[sandbox]\nallow_network = false\n", "project");
        let merged = merge(user, project);
        assert_eq!(merged.sandbox.enabled, Some(true));
        assert_eq!(merged.sandbox.allow_network, Some(false));
    }

    #[test]
    fn env_overrides_are_applied_on_top() {
        let _guard = crate::session::TEST_ENV_LOCK.blocking_lock();
        let mut cfg = parse_layer(
            "[provider]\nmodel = \"file-model\"\napi_key = \"file-key\"\n",
            "test",
        );
        unsafe { std::env::set_var("MODEL", "env-model") };
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "env-key") };
        unsafe { std::env::set_var("DEEPSEEK_BASE_URL", "https://env.invalid/v1") };
        apply_env_overrides(&mut cfg);
        unsafe { std::env::remove_var("MODEL") };
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        unsafe { std::env::remove_var("DEEPSEEK_BASE_URL") };
        assert_eq!(cfg.model.as_deref(), Some("env-model"));
        assert_eq!(cfg.api_key.as_deref(), Some("env-key"));
        assert_eq!(cfg.base_url.as_deref(), Some("https://env.invalid/v1"));
    }

    #[test]
    fn size_suffixes_match_cli_semantics() {
        assert_eq!(parse_size_bytes("120k").unwrap(), 120_000);
        assert_eq!(parse_size_bytes("2m").unwrap(), 2_000_000);
        assert_eq!(parse_size_bytes("1g").unwrap(), 1_000_000_000);
        assert_eq!(parse_size_bytes("4096").unwrap(), 4096);
        assert!(parse_size_bytes("").is_err());
    }
}
