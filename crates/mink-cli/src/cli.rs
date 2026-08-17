use crate::config::{
    CliEarlyExit, apply_config_file, apply_provider_defaults, apply_sdk_request_options,
    parse_args, validate_runtime_config,
};
use crate::runtime::{
    AgentEventKind, AgentOptions, AgentRuntime, AgentRuntimeHandle, ContextPolicy,
    GenerationOptions, PresentedToolResultDisplay, ProviderOptions, RuntimeResult, ToolCallDisplay,
    ToolOptions, ToolResultDisplay, TurnOutcome, exit_code_from_turn, final_from_outcome,
    runtime_skills_from_sdk_request, skill_discovery_policy_from_sdk_request,
};
use crate::sdk_protocol::{
    PROTOCOL_VERSION, SdkRequest, emit_failed_parse, parse_agent_jsonl_request,
    validate_sdk_request,
};
use crate::ui::engine::TerminalDisplay;
use crate::ui::replay::replay_last_turns;
use crate::ui::{Display, SubAgentStreamSink};
use anyhow::Result;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub(crate) enum RuntimeCmd {
    Run {
        input: String,
        done: Option<oneshot::Sender<RuntimeResult<TurnOutcome>>>,
    },
    Compact,
    SetModel(String),
    Interrupt,
    Exit,
}

fn render_agent_event(display: &dyn Display, kind: AgentEventKind) {
    match kind {
        AgentEventKind::Thinking { content } => display.render_thinking(&content),
        AgentEventKind::Text { content } => display.render_text(&content),
        AgentEventKind::ToolCall {
            id,
            name,
            summary,
            input,
        } => display.render_tool_call(&ToolCallDisplay {
            tool_use_id: &id,
            tool_name: &name,
            summary: &summary,
            input: Some(&input),
        }),
        AgentEventKind::ToolResult {
            tool_use_id,
            tool_name,
            content_preview,
            content,
            status,
            exit_code,
            result_kind,
            presentation,
            artifacts,
        } => display.render_tool_result(&PresentedToolResultDisplay {
            base: ToolResultDisplay {
                tool_name: &tool_name,
                content_preview: &content_preview,
                content: &content,
                tool_use_id: tool_use_id.as_deref(),
                exit_code,
            },
            status,
            result_kind,
            presentation: presentation.as_ref(),
            artifacts: &artifacts,
        }),
        AgentEventKind::Signal {
            signal_kind,
            severity,
            message,
        } => display.render_signal(&signal_kind, severity, &message),
        AgentEventKind::Stop { reason } => display.render_stop(&reason),
        AgentEventKind::Retry => display.render_retry(),
        AgentEventKind::Error { message } => display.render_error(&message),
        AgentEventKind::Info { message } => display.render_info(&message),
        AgentEventKind::TitleUpdate { model, stats } => display.render_title_update(&model, &stats),
        AgentEventKind::SubAgentStatus {
            session_id,
            status,
            in_tokens,
            out_tokens,
        } => display.render_sub_agent_status(&session_id, &status, in_tokens, out_tokens),
        AgentEventKind::SubAgentOutput {
            session_id,
            status,
            thinking,
            text,
            in_tokens,
            out_tokens,
        } => display.render_sub_agent_output(
            &session_id,
            &status,
            &thinking,
            &text,
            in_tokens,
            out_tokens,
        ),
        AgentEventKind::Prompt => display.render_prompt(),
        AgentEventKind::ClearLine => display.render_clear_line(),
        AgentEventKind::TurnStarted | AgentEventKind::Final { .. } => {}
    }
}

async fn run_turn_rendered(
    handle: &AgentRuntimeHandle,
    input: String,
    display: &dyn Display,
) -> RuntimeResult<TurnOutcome> {
    let mut stream = handle.stream_turn(input)?;
    while let Some(event) = stream.recv().await {
        render_agent_event(display, event.kind);
    }
    stream.outcome().await
}

fn start_runtime_broker(
    handle: AgentRuntimeHandle,
    display: Arc<dyn Display>,
) -> mpsc::UnboundedSender<RuntimeCmd> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(command) = work_rx.recv().await {
            let result = match command {
                RuntimeCmd::Run { input, done } => {
                    let result = run_turn_rendered(&work_handle, input, display.as_ref()).await;
                    if let Err(error) = &result {
                        display.render_error(&error.to_string());
                    }
                    if let Some(done) = done {
                        let _ = done.send(result);
                    }
                    continue;
                }
                RuntimeCmd::Compact => work_handle.compact().await.map(|_| ()),
                RuntimeCmd::SetModel(model) => work_handle.set_model(model).await,
                RuntimeCmd::Interrupt => unreachable!("interrupts bypass the work queue"),
                RuntimeCmd::Exit => unreachable!("exit bypasses the work queue"),
            };
            if let Err(error) = result {
                display.render_error(&error.to_string());
            }
        }
    });
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            match command {
                RuntimeCmd::Exit => break,
                RuntimeCmd::Interrupt => handle.interrupt_current_turn(),
                command => {
                    if work_tx.send(command).is_err() {
                        break;
                    }
                }
            }
        }
    });
    tx
}

pub struct CliExit {
    pub code: i32,
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        #[cfg(feature = "tui")]
        let _ = crossterm::terminal::disable_raw_mode();
        eprintln!("{info}");
    }));
}

pub async fn main_entry(args: Vec<String>) -> Result<CliExit> {
    let mut cfg = match parse_args(args) {
        Ok(v) => v,
        Err(e) => match e.downcast_ref::<CliEarlyExit>() {
            Some(CliEarlyExit::Help) => {
                print_usage();
                return Ok(CliExit { code: 0 });
            }
            Some(CliEarlyExit::Version) => {
                println!("{}", version_line());
                return Ok(CliExit { code: 0 });
            }
            None => return Err(e),
        },
    };

    let cwd = std::env::current_dir()?;
    let home = crate::config::default_home();

    if cfg.list_sessions {
        list_sessions(&home, &cwd).await?;
        return Ok(CliExit { code: 0 });
    }
    if cfg.list_skills {
        list_skills();
        return Ok(CliExit { code: 0 });
    }

    apply_config_file(&mut cfg)?;

    if !cfg.agent_jsonl {
        apply_provider_defaults(&mut cfg)?;
        reexec_if_sandbox(&cfg);
    }

    // ═══ SDK protocol mode: early stdin parsing (before context creation) ═══
    let sdk_request: Option<SdkRequest> = if cfg.agent_jsonl {
        // Self-sandboxing must happen before reading SDK stdin so the
        // sandboxed child process inherits the original pipe with data intact.
        reexec_if_sandbox(&cfg);
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let req = match parse_agent_jsonl_request(&input) {
            Ok(req) => req,
            Err(e) => {
                emit_failed_parse(&e);
                return Ok(CliExit { code: 1 });
            }
        };
        if let Some(version) = req.version
            && version != PROTOCOL_VERSION
        {
            emit_failed_parse(&format!(
                "unsupported SDK protocol version: {version}, expected {PROTOCOL_VERSION}"
            ));
            return Ok(CliExit { code: 1 });
        }
        if let Err(e) = validate_sdk_request(&req) {
            emit_failed_parse(&e);
            return Ok(CliExit { code: 1 });
        }
        apply_sdk_request_options(&mut cfg, &req);
        Some(req)
    } else {
        None
    };

    if cfg.agent_jsonl {
        apply_provider_defaults(&mut cfg)?;
    }

    if let Err(error) = validate_runtime_config(&cfg) {
        if cfg.agent_jsonl {
            emit_failed_parse(&format!("invalid SDK request: {error}"));
            return Ok(CliExit { code: 1 });
        }
        return Err(error);
    }
    let prompt_for_title = sdk_request
        .as_ref()
        .map(|r| r.prompt.clone())
        .or_else(|| (!cfg.prompt.trim().is_empty()).then(|| cfg.prompt.clone()));

    // Determine interactive mode early, before ctx creation.
    // StreamJson 输出（--print/--agent-jsonl）没有人类可读输出，
    // 不得落入 REPL（否则 REPL 渲染全部被抑制、只剩 stderr 提示符）。
    let is_stream_json_output =
        cfg.output_format == crate::config::OutputFormat::StreamJson || cfg.agent_jsonl;
    let is_interactive = cfg.interactive
        || (cfg.prompt.is_empty() && std::io::stdin().is_terminal() && !is_stream_json_output);
    cfg.interactive = is_interactive;
    let is_stream_json = is_stream_json_output;

    // TUI channels (if tui_mode). Created before display so signal_tx is available.
    #[cfg(feature = "tui")]
    let tui_tx = if cfg.tui_mode.enabled() {
        let (signal_tx, signal_rx) = std::sync::mpsc::channel::<crate::tui::TuiSignal>();
        Some((signal_tx, signal_rx))
    } else {
        None
    };
    #[cfg(not(feature = "tui"))]
    {
        if cfg.tui_mode.enabled() {
            anyhow::bail!("this mink binary was built without the `tui` feature");
        }
    }

    #[cfg(feature = "tui")]
    let display: Arc<dyn Display> = if let Some((ref tx, ..)) = tui_tx {
        Arc::new(crate::tui::TuiDisplay::new(tx.clone())) as Arc<dyn Display>
    } else {
        Arc::new(TerminalDisplay::new(is_interactive, is_stream_json))
    };
    #[cfg(not(feature = "tui"))]
    let display: Arc<dyn Display> = Arc::new(TerminalDisplay::new(is_interactive, is_stream_json));

    // TUI mode: store mpsc sender for sub-agent streaming
    #[cfg(feature = "tui")]
    let sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>> = tui_tx.as_ref().map(|(tx, ..)| {
        Arc::new(crate::tui::TuiSubAgentStreamSink::new(tx.clone())) as Arc<dyn SubAgentStreamSink>
    });
    #[cfg(not(feature = "tui"))]
    let sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>> = None;

    let mut runtime_options =
        assemble_runtime_options(&cfg, home.clone(), cwd.clone()).with_project_scoped_sessions();
    if let Some(prompt) = prompt_for_title {
        runtime_options = runtime_options.with_first_prompt(prompt);
    }
    if let Some(layout) = sdk_request
        .as_ref()
        .and_then(|request| request.options.session.session_layout)
    {
        runtime_options = runtime_options.with_session_layout(layout);
    }
    if let Some(request) = sdk_request.as_ref() {
        for skill in runtime_skills_from_sdk_request(request) {
            runtime_options = runtime_options.with_runtime_skill(skill);
        }
        if let Some(policy) = skill_discovery_policy_from_sdk_request(request) {
            runtime_options = runtime_options.with_skill_discovery_policy(policy);
        }
    }
    if let Some(sub_stream_tx) = sub_stream_tx {
        runtime_options = runtime_options.with_sub_stream_tx(sub_stream_tx);
    }
    let runtime = AgentRuntime::start(runtime_options).await?;
    let runtime_handle = runtime.handle();
    let session = runtime.session_info().clone();

    let mut process_exit_code = 0i32;
    let turn_result: anyhow::Result<()> = async {
        if cfg.agent_jsonl {
            let outcome = run_turn_rendered(
                &runtime_handle,
                sdk_request
                    .map(|request| request.prompt)
                    .unwrap_or_default(),
                display.as_ref(),
            )
            .await?;
            crate::sdk_protocol::emit_json_line(&final_from_outcome(&outcome));
            process_exit_code = exit_code_from_turn(outcome.status);
        } else if cfg.tui_mode.enabled() {
            #[cfg(feature = "tui")]
            {
                let cmd_tx = start_runtime_broker(runtime_handle.clone(), display.clone());
                if let Some((_, signal_rx)) = tui_tx {
                    let model_label = crate::config::resolve_model_label(&cfg.model);
                    if let Err(e) = crate::tui::run_tui(
                        cfg.tui_mode,
                        signal_rx,
                        cmd_tx.clone(),
                        &session,
                        &model_label,
                        &cfg.sandbox,
                    ) {
                        eprintln!("TUI error: {e}");
                    }
                }
            }
            #[cfg(not(feature = "tui"))]
            anyhow::bail!("this mink binary was built without the `tui` feature");
        } else if is_interactive {
            let cmd_tx = start_runtime_broker(runtime_handle.clone(), display.clone());
            display.render_info("mink interactive mode (type 'exit' or Ctrl+D to quit)");
            if !session.is_new {
                replay_last_turns(&session.events_path);
            }
            run_interactive(cmd_tx, &home).await?;
        } else if !cfg.prompt.is_empty() {
            let outcome =
                run_turn_rendered(&runtime_handle, cfg.prompt.clone(), display.as_ref()).await?;
            emit_stream_json_final_if_needed(&cfg, &outcome);
            process_exit_code = exit_code_from_turn(outcome.status);
        } else {
            let mut input = String::new();
            tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
            let outcome = run_turn_rendered(&runtime_handle, input, display.as_ref()).await?;
            emit_stream_json_final_if_needed(&cfg, &outcome);
            process_exit_code = exit_code_from_turn(outcome.status);
        }

        // Auto-generate session title from first user input if missing
        auto_set_session_title(&session).await;
        Ok(())
    }
    .await;

    let shutdown_result = runtime.shutdown().await;
    turn_result?;
    shutdown_result?;

    if !session.session_id.is_empty() {
        let alias_label = read_session_alias(&session).await;
        let label = alias_label.as_deref().unwrap_or(&session.session_ref);
        eprintln!(
            "\x1b[90mResume with: --session {}  or  --continue\x1b[0m",
            label
        );
    }

    Ok(CliExit {
        code: process_exit_code,
    })
}

fn assemble_runtime_options(
    cfg: &crate::config::CliConfig,
    home: PathBuf,
    cwd: PathBuf,
) -> AgentOptions {
    let session = if !cfg.session_id.trim().is_empty() {
        crate::runtime::SessionPolicy::UseOrCreate(cfg.session_id.trim().to_string())
    } else if cfg.continue_session {
        crate::runtime::SessionPolicy::ContinueLatest
    } else {
        crate::runtime::SessionPolicy::New
    };
    let mut options = AgentOptions::new(home, cwd)
        .with_provider_options(ProviderOptions {
            model: cfg.model.clone(),
            model_aliases: cfg.model_aliases.clone(),
            api_key: cfg.api_key.clone(),
            base_url: cfg.base_url.clone(),
            reasoning_effort: cfg.openai_reasoning_effort.clone(),
            include_usage: cfg.openai_include_usage,
            token_param: cfg.openai_token_param,
            tool_choice: cfg.openai_tool_choice.clone(),
            extra_body: cfg.openai_extra_body.clone(),
        })
        .with_generation_options(GenerationOptions {
            max_tokens: cfg.max_tokens,
            max_turns: cfg.max_turns,
            first_event_timeout_secs: cfg.llm_first_event_timeout_secs,
            idle_timeout_secs: cfg.llm_idle_timeout_secs,
            wait_heartbeat_secs: cfg.llm_wait_heartbeat_secs,
        })
        .with_context_policy(ContextPolicy {
            max_context_tokens: cfg.max_context_tokens,
            compact_pct: cfg.context_compact_pct,
            reserve_tokens: cfg.context_reserve_tokens,
            compact_tail_tokens: cfg.context_compact_tail_tokens,
            compact_max_output_tokens: cfg.context_compact_max_output_tokens,
            compact_input_reduction: cfg.context_compact_input_reduction,
            plan_projection_tail: cfg.plan_projection_tail,
        })
        .with_tool_options(ToolOptions {
            timeout_secs: cfg.tool_timeout_secs,
            timeout_max_secs: cfg.tool_timeout_max_secs,
            sub_agent_timeout_secs: cfg.sub_agent_timeout_secs,
            result_max_bytes: cfg.tool_result_max_bytes,
            file_write_max_bytes: cfg.file_write_max_bytes,
            edit_mode: cfg.edit_mode,
            edit_fuzzy_match: cfg.edit_fuzzy_match,
            edit_fuzzy_threshold: cfg.edit_fuzzy_threshold,
            edit_enforce_seen_lines: cfg.edit_enforce_seen_lines,
            max_search_files: cfg.max_search_files,
            max_search_results: cfg.max_search_results,
            enabled_tools: cfg.enabled_tools.clone(),
            approval_mode: cfg.tool_approval_mode,
            approval: cfg.tool_approval.clone(),
        })
        .with_signal_policy(cfg.signal_policy)
        .with_session(session)
        .with_output_format(cfg.output_format)
        .with_verbose(cfg.verbose)
        .with_log_events(cfg.log_events)
        .with_selected_skills(cfg.skills.clone())
        .with_sandbox(cfg.sandbox.clone())
        .with_sandbox_python(cfg.sandbox_python.clone())
        .with_interactive(cfg.interactive);
    if !cfg.prompt.trim().is_empty() {
        options = options.with_first_prompt(cfg.prompt.clone());
    }
    if let Some(path) = &cfg.mission_file {
        options = options.with_mission_file(path.clone());
    }
    if let Some(content) = &cfg.mission_content {
        options = options.with_mission_content(content.clone());
    }
    options
}

async fn read_session_alias(session: &crate::runtime::SessionInfo) -> Option<String> {
    let metadata_path = session.events_path.with_file_name("session.json");
    let text = tokio::fs::read_to_string(&metadata_path).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    value.get("alias")?.as_str().map(|s| s.to_string())
}

/// After all turns complete, auto-generate a session title from the first user
/// input if the session.json doesn't already have a title. This ensures
/// interactive/TUI sessions show a meaningful name in `--list-sessions`.
async fn auto_set_session_title(session: &crate::runtime::SessionInfo) {
    let metadata_path = session.events_path.with_file_name("session.json");
    derive_and_persist_title(&metadata_path, &session.conversation_path).await;
}

/// 从 conversation.jsonl 首条真实 user 消息生成标题并原子写回 session.json。
/// 已有非空标题时不动。此前该逻辑在 auto_set_session_title 与
/// resolve_session_title 各实现一份且都是非原子写。
async fn derive_and_persist_title(
    metadata_path: &Path,
    conversation_path: &Path,
) -> Option<String> {
    let metadata_text = tokio::fs::read_to_string(metadata_path).await.ok()?;
    let mut metadata: serde_json::Value = serde_json::from_str(&metadata_text).ok()?;
    if metadata
        .get("title")
        .and_then(|v| v.as_str())
        .is_some_and(|t| !t.is_empty())
    {
        return None;
    }
    let conv_text = tokio::fs::read_to_string(conversation_path).await.ok()?;
    let title = conv_text.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let msg: serde_json::Value = serde_json::from_str(line).ok()?;
        if msg.get("role")?.as_str()? != "user" {
            return None;
        }
        let content = msg.get("content")?.as_str()?;
        crate::session::metadata::title_from_prompt(content)
    })?;
    metadata["title"] = serde_json::Value::String(title.clone());
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::format_description::well_known::Rfc3339;
    metadata["updated_at"] = serde_json::Value::String(now.format(&fmt).unwrap_or_default());
    if let Ok(text) = serde_json::to_string_pretty(&metadata) {
        let _ = mink::runtime::atomic_replace(metadata_path, format!("{text}\n").as_bytes());
    }
    Some(title)
}

fn reexec_if_sandbox(cfg: &crate::config::CliConfig) {
    if cfg.sandbox.is_active() {
        let current_exe = std::env::current_exe().unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        let sandbox = crate::runtime::SandboxConfig {
            enabled: cfg.sandbox.enabled,
            backend: cfg.sandbox.backend.clone(),
            read_dirs: cfg.sandbox.read_dirs.clone(),
            write_dirs: cfg.sandbox.write_dirs.clone(),
            allow_network: cfg.sandbox.allow_network,
            max_memory_mb: cfg.sandbox.max_memory_mb,
            max_pids: cfg.sandbox.max_pids,
            timeout_secs: cfg.sandbox.timeout_secs,
        };
        crate::sandbox::reexec_in_sandbox(&sandbox, &current_exe, &args);
    }
}

fn emit_stream_json_final_if_needed(cfg: &crate::config::CliConfig, outcome: &TurnOutcome) {
    if cfg.output_format != crate::config::OutputFormat::StreamJson || cfg.agent_jsonl {
        return;
    }
    crate::sdk_protocol::emit_json_line(&final_from_outcome(outcome));
}

fn list_skills() {
    crate::local::print_skills();
}

async fn run_interactive(cmd_tx: mpsc::UnboundedSender<RuntimeCmd>, home: &Path) -> Result<()> {
    #[cfg(feature = "repl")]
    let history_path = home.join(".mink/history");
    #[cfg(not(feature = "repl"))]
    let _ = home;
    let (exit_tx, exit_rx) = tokio::sync::mpsc::channel::<()>(1);
    {
        let sig_tx = cmd_tx.clone();
        let sig_exit_tx = exit_tx;
        tokio::spawn(async move {
            let mut last: Option<Instant> = None;
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                let now = Instant::now();
                if let Some(prev) = last
                    && now.duration_since(prev) < Duration::from_secs(2)
                {
                    let _ = sig_tx.send(RuntimeCmd::Exit);
                    let _ = sig_exit_tx.send(()).await;
                    return;
                }
                last = Some(now);
                let _ = sig_tx.send(RuntimeCmd::Interrupt);
            }
        });
    }
    tokio::task::spawn_blocking(move || {
        #[cfg_attr(not(feature = "repl"), allow(unused_mut))]
        let mut exit_rx = exit_rx;
        #[cfg(feature = "repl")]
        {
            if let Some(parent) = history_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut rl = match rustyline::DefaultEditor::new() {
                Ok(r) => r,
                Err(_) => {
                    eprintln!("Failed to initialize readline. Running in simple mode.");
                    simple_stdin_loop(&cmd_tx, exit_rx);
                    return;
                }
            };

            let _ = rl.load_history(&history_path);
            let history_file = history_path.clone();
            let mut last_interrupt: Option<Instant> = None;

            loop {
                let line = match rl.readline("> ") {
                    Ok(s) => s.trim_end().to_string(),
                    Err(rustyline::error::ReadlineError::Eof) => break,
                    Err(rustyline::error::ReadlineError::Interrupted) => {
                        if last_interrupt
                            .is_some_and(|prev| prev.elapsed() < Duration::from_secs(2))
                        {
                            let _ = cmd_tx.send(RuntimeCmd::Exit);
                            break;
                        }
                        last_interrupt = Some(Instant::now());
                        let _ = cmd_tx.send(RuntimeCmd::Interrupt);
                        continue;
                    }
                    Err(_) => break,
                };
                if line.is_empty() {
                    continue;
                }
                match dispatch_local_command(&line, &cmd_tx) {
                    LocalCommandOutcome::Shutdown => break,
                    LocalCommandOutcome::Handled => continue,
                    LocalCommandOutcome::PassThrough => {}
                }
                if line == "exit" || line == "quit" {
                    break;
                }
                let _ = rl.add_history_entry(&line);
                let _ = rl.save_history(&history_file);
                let (done_tx, done_rx) = tokio::sync::oneshot::channel();
                if cmd_tx
                    .send(RuntimeCmd::Run {
                        input: line,
                        done: Some(done_tx),
                    })
                    .is_err()
                {
                    break;
                }
                // 轮询等待完成，同时检查中断与退出信号
                let mut done_rx = done_rx;
                loop {
                    match done_rx.try_recv() {
                        Ok(_) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            if exit_rx.try_recv().is_ok() {
                                let _ = cmd_tx.send(RuntimeCmd::Exit);
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }
            }
            let _ = rl.save_history(&history_file);
        }
        #[cfg(not(feature = "repl"))]
        {
            eprintln!("Readline support is disabled; running in simple stdin mode.");
            simple_stdin_loop(&cmd_tx, exit_rx);
        }
    })
    .await?;

    Ok(())
}

fn simple_stdin_loop(
    cmd_tx: &mpsc::UnboundedSender<RuntimeCmd>,
    mut exit_rx: tokio::sync::mpsc::Receiver<()>,
) {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        if exit_rx.try_recv().is_ok() {
            break;
        }

        let line = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            match dispatch_local_command(&line, cmd_tx) {
                LocalCommandOutcome::Shutdown => break,
                LocalCommandOutcome::Handled | LocalCommandOutcome::PassThrough => {}
            }
            continue;
        }
        if line == "exit" || line == "quit" {
            break;
        }
        let (done_tx, done_rx) = oneshot::channel();
        if cmd_tx
            .send(RuntimeCmd::Run {
                input: line,
                done: Some(done_tx),
            })
            .is_err()
        {
            break;
        }
        let _ = done_rx.blocking_recv();
        if exit_rx.try_recv().is_ok() {
            break;
        }
    }
}
enum LocalCommandOutcome {
    /// The line was consumed by a local slash command.
    Handled,
    /// Not a local command; the caller decides how to treat it as input.
    PassThrough,
    /// A command-channel send failed; the caller should stop the loop.
    Shutdown,
}

/// Handle local slash commands shared by the readline and simple stdin
/// loops. Unknown `/...` lines are reported as `PassThrough`; the readline
/// loop forwards them as input while the simple loop consumes them.
fn dispatch_local_command(
    line: &str,
    cmd_tx: &mpsc::UnboundedSender<RuntimeCmd>,
) -> LocalCommandOutcome {
    match line {
        "/help" => {
            for line in crate::local::COMMON_COMMAND_HELP
                .iter()
                .chain(crate::local::REPL_EXIT_HELP)
            {
                println!("{line}");
            }
            LocalCommandOutcome::Handled
        }
        "/skills" => {
            list_skills();
            LocalCommandOutcome::Handled
        }
        "/compact" => {
            if cmd_tx.send(RuntimeCmd::Compact).is_err() {
                LocalCommandOutcome::Shutdown
            } else {
                LocalCommandOutcome::Handled
            }
        }
        "/flash" | "/pro" => {
            let model = line.trim_start_matches('/');
            if cmd_tx
                .send(RuntimeCmd::SetModel(model.to_string()))
                .is_err()
            {
                LocalCommandOutcome::Shutdown
            } else {
                LocalCommandOutcome::Handled
            }
        }
        _ if line.starts_with("/model ") => {
            let model = line["/model ".len()..].trim();
            if model.is_empty() {
                LocalCommandOutcome::Handled
            } else if cmd_tx
                .send(RuntimeCmd::SetModel(model.to_string()))
                .is_err()
            {
                LocalCommandOutcome::Shutdown
            } else {
                LocalCommandOutcome::Handled
            }
        }
        _ => LocalCommandOutcome::PassThrough,
    }
}

async fn list_sessions(home: &Path, cwd: &Path) -> Result<()> {
    let mut rows = crate::session::metadata::list_project_sessions(home, cwd).await?;
    if rows.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.modified));
    println!(
        "{} {} {} ID",
        pad_display("ALIAS", 20),
        pad_display("TITLE", 32),
        pad_display("UPDATED", 16),
    );
    let alias_width = 20;
    let title_width = 32;
    let updated_width = 16;
    for row in rows {
        let alias = row.alias.as_deref().unwrap_or("-");
        let title = resolve_session_title(&row.path, &row).await;
        let alias = truncate_display(alias, alias_width);
        let title = truncate_display(&title, title_width);
        let dt: time::OffsetDateTime = row.modified.into();
        let formatted = {
            use time::macros::format_description;
            static FMT: &[time::format_description::FormatItem<'_>] =
                format_description!("[year]-[month]-[day] [hour]:[minute]");
            dt.format(FMT)
                .unwrap_or_else(|_| format!("{:?}", row.modified))
        };
        println!(
            "{} {} {} {}",
            pad_display(&alias, alias_width),
            pad_display(&title, title_width),
            pad_display(&formatted, updated_width),
            row.id,
        );
    }
    Ok(())
}

/// Return the session title, lazily generating it from the first user
/// conversation turn when session.json has no title. If generated, only the
/// `title` field is written back to session.json — all other fields are
/// preserved untouched.
async fn resolve_session_title(
    session_dir: &Path,
    meta: &crate::session::metadata::SessionRecord,
) -> String {
    if let Some(title) = meta.title.as_deref().filter(|t| !t.is_empty()) {
        return title.to_string();
    }
    if let Some(summary) = meta.summary.as_deref().filter(|s| !s.is_empty()) {
        return summary.to_string();
    }
    // Lazy path: 生成并原子写回（与交互路径共用同一实现）。
    let metadata_path = session_dir.join("session.json");
    let conversation_path = session_dir.join("conversation.jsonl");
    derive_and_persist_title(&metadata_path, &conversation_path)
        .await
        .unwrap_or_else(|| "-".to_string())
}

fn truncate_display(s: &str, max_width: usize) -> String {
    crate::util::truncate_display(s, max_width)
}

fn pad_display(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        let mut result = s.to_string();
        result.push_str(&" ".repeat(width - w));
        result
    }
}

fn print_usage() {
    let program = std::env::args()
        .next()
        .and_then(|arg| {
            std::path::Path::new(&arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "mink".to_string());
    println!("{}", version_line());
    println!();
    println!("Usage: {program} [options] [prompt]");
    println!("Options:");
    println!("  -m, --model MODEL       Model name or alias: flash | pro | any backend model");
    println!("  --api-key KEY           API key (default from env)");
    println!("  --base-url URL          Override API base URL");
    println!("  --mission PATH          Load custom system prompt from MISSION.md file");
    println!("  --session NAME          Use named session");
    println!("  --continue              Continue most recent session");
    println!("  --list-sessions         List saved sessions");
    println!("  --list-skills           List available skills");
    println!("  -v, --verbose           Verbose mode");
    println!("  -i, --interactive       Interactive mode (REPL)");
    #[cfg(feature = "tui")]
    println!("  --tui[=full|inline]     Full TUI (default) or inline TUI");
    println!("  --print                 Stream JSON events to stdout");
    println!("  --agent-jsonl           Agent JSONL protocol");
    println!("  --enabled-tools <list>  Comma-separated tools to enable, or 'none'");
    println!("  --edit-mode MODE        Edit protocol: hashline (default) or replace");
    println!("  --edit-fuzzy-match BOOL Enable Replace progressive/fuzzy matching");
    println!("  --edit-fuzzy-threshold N Replace fuzzy threshold, 0.0..=1.0 (default: 0.95)");
    println!("  --edit-enforce-seen-lines BOOL Require displayed Hashline anchors");
    println!("  --skill NAME            Select a skill (repeatable); same as tools.skills");
    println!("  --config <toml>         Set config via TOML string (grouped sections)");
    println!(
        "                          Example: --config \"[generation]\\nmax_tokens=4096\\n[tools]\\ntool_timeout=300\""
    );
    println!("                          Sections: [provider] [generation] [context] [tools]");
    println!("                          [tools.edit] [signal] [sandbox] [sandbox_python]");
    println!("  -h, --help              Show this help");
    println!();
    println!("Config via TOML (--config or .minkrc):");
    println!("  See .minkrc.example for a complete reference.");
    println!(
        "  Priority: CLI flags > --config > project .minkrc > user ~/.minkrc > env > defaults."
    );
    println!();
    println!("Environment:");
    println!("  DEEPSEEK_API_KEY        DeepSeek API key");
    println!("  DEEPSEEK_BASE_URL       DeepSeek base URL");
    println!("  LOG_EVENTS              Enable event logging (default: true)");
    println!("  TOOL_RESULT_MAX_BYTES   Max tool result bytes");
    println!("  FILE_WRITE_MAX_BYTES    Max file write bytes");
    println!("  MAX_SEARCH_FILES        Max files scanned by Glob/Grep");
    println!("  MAX_SEARCH_RESULTS      Max Glob/Grep results");
    println!("  MINK_LIMITS             Sandbox limits as JSON [sandbox] override");
    println!(
        "  MINK_SIGNAL_POLICY      Signal policy: off | evidence | state_ops | restart | full"
    );
    println!("  MINK_EDIT_MODE          Edit protocol: hashline | replace");
    println!("  MINK_EDIT_FUZZY_MATCH   Replace fuzzy matching: true | false");
    println!("  MINK_EDIT_FUZZY_THRESHOLD Replace fuzzy threshold, 0.0..=1.0");
    println!("  MINK_EDIT_ENFORCE_SEEN_LINES Hashline seen-line guard: true | false");
}
fn version_line() -> String {
    let git_hash = env!("MINK_GIT_HASH");
    if git_hash.is_empty() {
        format!("mink {}", env!("CARGO_PKG_VERSION"))
    } else {
        format!("mink {} ({git_hash})", env!("CARGO_PKG_VERSION"))
    }
}
