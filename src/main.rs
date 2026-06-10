use anyhow::Result;
use mink::agent::orchestrator::{OrchCmd, TurnStatus, new_orchestrator};
use mink::cancel::CancellationToken;
use mink::config::{api_url, apply_config_file, apply_provider_defaults, parse_args};
use mink::context::AgentSharedContext;
use mink::context::ToolConfig;
use mink::sdk_protocol::{
    PROTOCOL_VERSION, SdkFinal, SdkRequest, SdkStatus, emit_failed_parse,
    parse_agent_jsonl_request, path_string, validate_sdk_request,
};
use mink::session::compaction::CompactionEngine;
use mink::session::paths;
use mink::ui::Display;
use mink::ui::engine::TerminalDisplay;
use mink::ui::replay::replay_last_turns;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

#[tokio::main]
async fn main() {
    std::panic::set_hook(Box::new(|info| {
        let _ = crossterm::terminal::disable_raw_mode();
        eprintln!("{info}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(args).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(args: Vec<String>) -> Result<()> {
    let mut cfg = match parse_args(args) {
        Ok(v) => v,
        Err(e) if e.to_string() == "__HELP__" => {
            print_usage();
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    let cwd = std::env::current_dir()?;
    let home = PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    if cfg.list_sessions {
        list_sessions(&home, &cwd).await?;
        return Ok(());
    }
    if cfg.list_skills {
        list_skills();
        return Ok(());
    }

    apply_config_file(&mut cfg);
    apply_provider_defaults(&mut cfg)?;

    // ═══ Self-sandboxing: re-exec into nsjail/bwrap/sandbox-exec ═══
    // This must happen BEFORE any stdin reading (Agent JSONL) so that
    // the sandboxed child process inherits the original stdin pipe
    // with its data still intact.
    // If successful, the process is replaced and we never reach the next line.
    // If it fails, we log a warning and continue without sandbox.
    if cfg.sandbox.is_active() {
        let current_exe = std::env::current_exe().unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        // ⚠ Blocking call — ok here because we haven't entered the async runtime yet
        mink::sandbox::reexec_in_sandbox(&cfg.sandbox, &current_exe, &args);
    }

    // ═══ SDK protocol mode: early stdin parsing (before context creation) ═══
    // We parse the request here so that tool_disable flags take effect when
    // AgentSharedContext and ToolConfig are constructed.
    let sdk_request: Option<SdkRequest> = if cfg.agent_jsonl {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let req = match parse_agent_jsonl_request(&input) {
            Ok(req) => req,
            Err(e) => {
                emit_failed_parse(&e);
                std::process::exit(1);
            }
        };
        if let Some(version) = req.version
            && version != PROTOCOL_VERSION
        {
            emit_failed_parse(&format!(
                "unsupported SDK protocol version: {version}, expected {PROTOCOL_VERSION}"
            ));
            std::process::exit(1);
        }
        if let Err(e) = validate_sdk_request(&req) {
            emit_failed_parse(&e);
            std::process::exit(1);
        }
        apply_sdk_request_options(&mut cfg, &req);
        Some(req)
    } else {
        None
    };

    let requested_session = cfg.session_id.trim().to_string();
    let mut session_alias = None;
    let mut sid = String::new();
    if !requested_session.is_empty() {
        if let Some(resolved) =
            mink::session::metadata::resolve_session_reference(&home, &cwd, &requested_session)
                .await?
        {
            sid = resolved;
        } else {
            session_alias = mink::session::metadata::sanitize_alias(&requested_session);
            if session_alias.is_none() {
                anyhow::bail!("invalid session name: {requested_session}");
            }
            sid = paths::chrono_session_id();
        }
    }
    if sid.is_empty() && cfg.continue_session {
        sid = paths::continue_session(&home, &cwd)
            .await
            .unwrap_or_default();
    }
    if sid.is_empty() {
        sid = paths::chrono_session_id();
    }
    cfg.session_id = sid.clone();
    let resume_session_ref = if requested_session.is_empty() {
        sid.clone()
    } else {
        requested_session.clone()
    };

    let spaths = paths::paths_for(&home, &cwd, &sid);

    let new_session = !spaths.events.exists();

    // 共享会话初始化
    let (store, stats, artifacts) =
        mink::session::init::init_session_base(&home, &cwd, &sid).await?;
    let prompt_for_title = sdk_request
        .as_ref()
        .map(|r| r.prompt.as_str())
        .or_else(|| (!cfg.prompt.trim().is_empty()).then_some(cfg.prompt.as_str()));
    mink::session::metadata::ensure_metadata(
        &spaths,
        &cwd,
        mink::session::metadata::SessionSeed {
            alias: session_alias,
            title: prompt_for_title.and_then(mink::session::metadata::title_from_prompt),
            first_prompt: prompt_for_title.map(ToString::to_string),
        },
    )
    .await?;
    let api_url_str = api_url(&cfg);

    // Determine interactive mode early, before ctx creation
    let is_interactive =
        cfg.interactive || (cfg.prompt.is_empty() && std::io::stdin().is_terminal());
    cfg.interactive = is_interactive;

    // Shared HTTP client for both LLM calls and compaction
    let shared_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("mink/3.0")
        .build()?;

    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        spaths.plan.clone(),
        spaths.plan_draft.clone(),
        cwd.clone(),
        home.clone(),
        cfg.skills.clone(),
        api_url_str.clone(),
        &cfg,
        stats.clone(),
        shared_client,
    ));

    let cancel = CancellationToken::new();
    let is_stream_json = cfg.output_format == mink::config::OutputFormat::StreamJson;

    // TUI channels (if tui_mode). Created before display so signal_tx is available.
    let tui_tx = if cfg.tui_mode {
        let (signal_tx, signal_rx) = std::sync::mpsc::channel::<mink::tui::TuiSignal>();
        Some((signal_tx, signal_rx))
    } else {
        None
    };

    let display: Arc<dyn Display> = if let Some((ref tx, ..)) = tui_tx {
        Arc::new(mink::tui::TuiDisplay::new(tx.clone())) as Arc<dyn Display>
    } else {
        Arc::new(TerminalDisplay::new(is_interactive, is_stream_json))
    };

    // TUI mode: store mpsc sender for sub-agent streaming
    let sub_stream_tx: Option<Arc<dyn std::any::Any + Send + Sync>> = tui_tx
        .as_ref()
        .map(|(tx, ..)| Arc::new(tx.clone()) as Arc<dyn std::any::Any + Send + Sync>);

    let ctx = Arc::new(AgentSharedContext {
        config: cfg.clone(),
        cwd: cwd.clone(),
        home: home.clone(),
        api_url: api_url_str.clone(),
        store,
        artifacts,
        snapshots: Arc::new(Mutex::new(
            mink::tools::snapshot::FileSnapshotStore::default(),
        )),
        stats,
        compaction,
        cancel: cancel.clone(),
        display: display.clone(),
        sub_stream_tx,
        tool_config: ToolConfig::from_config(&cfg),
        events_path: spaths.events.clone(),
        summary_path: spaths.summary.clone(),
        plan_path: spaths.plan.clone(),
        plan_draft_path: spaths.plan_draft.clone(),
        immutable_prefix: Mutex::new(None),
        is_sub_agent: false,
        interrupt: Arc::new(AtomicBool::new(false)),
        event_log_warned: AtomicBool::new(false),
    });

    let (orchestrator, cmd_tx) = new_orchestrator(ctx.clone());

    let orch_display = display.clone();
    let orch_handle = tokio::spawn(async move {
        if let Err(e) = orchestrator.run().await {
            orch_display.render_error(&format!("Orchestrator: {e}"));
        }
    });

    if new_session {
        ctx.log_event(serde_json::json!({"type":"session_start","session_id":sid}));
    }

    let mut process_exit_code = 0i32;
    if cfg.agent_jsonl {
        // SDK protocol: prompt was already parsed above; execute one turn.
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input: sdk_request.map(|r| r.prompt).unwrap_or_default(),
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            mink::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        mink::sdk_protocol::emit_json_line(&SdkFinal {
            event_type: "final",
            version: PROTOCOL_VERSION,
            status: sdk_status_from_turn(result.status),
            session_id: sid.clone(),
            session_ref: resume_session_ref.clone(),
            home: path_string(&home),
            cwd: path_string(&cwd),
            events_path: path_string(&spaths.events),
            conversation_path: path_string(&spaths.conversation),
            artifacts_dir: path_string(&spaths.artifacts),
            summary_path: path_string(&spaths.summary),
            tool_call_count: result.tool_call_count,
            tool_error_count: result.tool_error_count,
            error: result.error.clone(),
        });
        process_exit_code = exit_code_from_turn(result.status);
    } else if cfg.tui_mode {
        if let Some((_, signal_rx)) = tui_tx
            && let Err(e) = mink::tui::run_tui(
                signal_rx,
                cmd_tx.clone(),
                &spaths.events,
                Some(ctx.interrupt.clone()),
                &cfg.sandbox,
            )
        {
            eprintln!("TUI error: {e}");
        }
    } else if is_interactive {
        display.render_info("mink interactive mode (type 'exit' or Ctrl+D to quit)");
        if !new_session {
            replay_last_turns(&spaths.events);
        }
        run_interactive(cmd_tx, cancel.clone(), ctx.interrupt.clone(), &home).await?;
    } else if !cfg.prompt.is_empty() {
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input: cfg.prompt.clone(),
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            mink::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        emit_stream_json_final_if_needed(
            &cfg,
            &result,
            &sid,
            &resume_session_ref,
            &home,
            &cwd,
            &spaths,
        );
        process_exit_code = exit_code_from_turn(result.status);
    } else {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input,
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            mink::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        emit_stream_json_final_if_needed(
            &cfg,
            &result,
            &sid,
            &resume_session_ref,
            &home,
            &cwd,
            &spaths,
        );
        process_exit_code = exit_code_from_turn(result.status);
    }

    cancel.cancel();
    let _ = orch_handle.await;

    if !cfg.session_id.is_empty() {
        eprintln!(
            "\x1b[90mResume with: --session {}  or  --continue\x1b[0m",
            resume_session_ref
        );
    }

    if process_exit_code != 0 {
        std::process::exit(process_exit_code);
    }

    Ok(())
}

fn sdk_status_from_turn(status: TurnStatus) -> SdkStatus {
    match status {
        TurnStatus::Ok => SdkStatus::Ok,
        TurnStatus::Failed => SdkStatus::Failed,
        TurnStatus::Interrupted => SdkStatus::Interrupted,
        TurnStatus::MaxTurnsExceeded => SdkStatus::MaxTurnsExceeded,
    }
}

fn exit_code_from_turn(status: TurnStatus) -> i32 {
    match status {
        TurnStatus::Ok => 0,
        TurnStatus::Failed => 1,
        TurnStatus::Interrupted => 130,
        TurnStatus::MaxTurnsExceeded => 2,
    }
}

fn emit_stream_json_final_if_needed(
    cfg: &mink::config::Config,
    result: &mink::agent::orchestrator::TurnRunResult,
    sid: &str,
    resume_session_ref: &str,
    home: &Path,
    cwd: &Path,
    spaths: &mink::session::paths::Paths,
) {
    if cfg.output_format != mink::config::OutputFormat::StreamJson || cfg.agent_jsonl {
        return;
    }
    mink::sdk_protocol::emit_json_line(&SdkFinal {
        event_type: "final",
        version: PROTOCOL_VERSION,
        status: sdk_status_from_turn(result.status),
        session_id: sid.to_string(),
        session_ref: resume_session_ref.to_string(),
        home: path_string(home),
        cwd: path_string(cwd),
        events_path: path_string(&spaths.events),
        conversation_path: path_string(&spaths.conversation),
        artifacts_dir: path_string(&spaths.artifacts),
        summary_path: path_string(&spaths.summary),
        tool_call_count: result.tool_call_count,
        tool_error_count: result.tool_error_count,
        error: result.error.clone(),
    });
}

fn apply_sdk_request_options(cfg: &mut mink::config::Config, req: &SdkRequest) {
    let opts = &req.options;
    if opts.disable_bash {
        cfg.tool_disable.disable_bash = true;
    }
    if opts.disable_sub_agent {
        cfg.tool_disable.disable_sub_agent = true;
    }
    if opts.disable_web {
        cfg.tool_disable.disable_web = true;
    }
    if opts.disable_python {
        cfg.tool_disable.disable_python = true;
    }
    if let Some(model) = &opts.model {
        cfg.model = model.clone();
        cfg.cli_overrides.model = true;
    }
    if let Some(max_tokens) = opts.max_tokens {
        cfg.max_tokens = max_tokens;
        cfg.cli_overrides.max_tokens = true;
    }
    if let Some(max_turns) = opts.max_turns {
        cfg.max_turns = max_turns;
        cfg.cli_overrides.max_turns = true;
    }
    if let Some(tool_timeout) = opts.tool_timeout {
        cfg.tool_timeout_secs = tool_timeout;
        cfg.cli_overrides.tool_timeout_secs = true;
    }
    if let Some(sub_agent_timeout) = opts.sub_agent_timeout {
        cfg.sub_agent_timeout_secs = sub_agent_timeout;
        cfg.cli_overrides.sub_agent_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_first_event_timeout {
        cfg.llm_first_event_timeout_secs = timeout;
        cfg.cli_overrides.llm_first_event_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_idle_timeout {
        cfg.llm_idle_timeout_secs = timeout;
        cfg.cli_overrides.llm_idle_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_wait_heartbeat {
        cfg.llm_wait_heartbeat_secs = timeout;
        cfg.cli_overrides.llm_wait_heartbeat_secs = true;
    }
    if let Some(verbose) = opts.verbose {
        cfg.verbose = verbose;
    }
    if let Some(session_id) = req
        .session_id
        .as_deref()
        .or(opts.session_id.as_deref())
        .filter(|s| !s.is_empty())
    {
        cfg.session_id = session_id.to_string();
    }
    if let Some(mission) = &req.mission {
        cfg.mission_content = Some(mission.clone());
    }
    if let Some(tools) = &opts.enabled_tools {
        cfg.enabled_tools = Some(tools.clone());
    }
}

fn list_skills() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    println!("SKILLS");
    println!("{}", "-".repeat(60));
    for skill in mink::skills::list_available_skills(&cwd, &home) {
        let source = match skill.source {
            mink::skills::SkillSource::BuiltIn => "built-in",
            mink::skills::SkillSource::FileSystem => "local",
        };
        println!("   {} [{}]", skill.name, source);
        println!("      {}", skill.description);
        println!();
    }

    println!("Load with --skill NAME or Read skill://NAME.");
}

async fn run_interactive(
    cmd_tx: mpsc::UnboundedSender<OrchCmd>,
    cancel: CancellationToken,
    interrupt: Arc<AtomicBool>,
    home: &Path,
) -> Result<()> {
    let cancel_clone = cancel.clone();
    let history_path = home.join(".mink/history");
    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    tokio::task::spawn_blocking(move || {
        let mut rl = match rustyline::DefaultEditor::new() {
            Ok(r) => r,
            Err(_) => {
                eprintln!("Failed to initialize readline. Running in simple mode.");
                simple_stdin_loop(&cmd_tx, &cancel_clone);
                return;
            }
        };

        let _ = rl.load_history(&history_path);
        let history_file = history_path.clone();

        loop {
            let line = match rl.readline("> ") {
                Ok(s) => s.trim_end().to_string(),
                Err(rustyline::error::ReadlineError::Eof) => break,
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    interrupt.store(true, Ordering::SeqCst);
                    continue;
                }
                Err(_) => break,
            };
            if line.is_empty() {
                continue;
            }
            if line == "/help" {
                println!("Commands:");
                println!("  /flash        Switch to flash tier");
                println!("  /pro          Switch to pro tier");
                println!("  /compact      Force context compaction");
                println!("  /skills       List available skills");
                println!("  /help         Show this help");
                println!("  Ctrl+C        Interrupt current task");
                println!("  Ctrl+C again  Exit REPL");
                println!("  exit / quit   Exit REPL");
                continue;
            }
            if line == "/skills" {
                list_skills();
                continue;
            }
            if line == "/compact" {
                let (done_tx, done_rx) = oneshot::channel();
                if cmd_tx.send(OrchCmd::Compact { done: done_tx }).is_err() {
                    break;
                }
                let _ = done_rx.blocking_recv();
                continue;
            }
            if line == "/flash" || line == "/pro" {
                let model = line.trim_start_matches('/');
                if cmd_tx.send(OrchCmd::SetModel(model.to_string())).is_err() {
                    break;
                }
                continue;
            }
            if line == "exit" || line == "quit" {
                break;
            }
            if cancel_clone.is_cancelled() {
                break;
            }
            let _ = rl.add_history_entry(&line);
            let _ = rl.save_history(&history_file);
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            if cmd_tx
                .send(OrchCmd::UserInput {
                    input: line,
                    done: done_tx,
                })
                .is_err()
            {
                break;
            }
            // 轮询等待完成，同时检查中断标志
            let mut done_rx = done_rx;
            loop {
                if interrupt.load(Ordering::SeqCst) {
                    break;
                }
                match done_rx.try_recv() {
                    Ok(_) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        }
        let _ = rl.save_history(&history_file);
    })
    .await?;

    Ok(())
}

fn simple_stdin_loop(cmd_tx: &mpsc::UnboundedSender<OrchCmd>, cancel: &CancellationToken) {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            if line == "/help" {
                println!("Commands: /flash, /pro, /compact, /skills, /help");
                println!("Ctrl+C = interrupt, Ctrl+C again = exit");
            } else if line == "/skills" {
                list_skills();
            } else if line == "/compact" {
                let (done_tx, done_rx) = oneshot::channel();
                let _ = cmd_tx.send(OrchCmd::Compact { done: done_tx });
                let _ = done_rx.blocking_recv();
            } else if line == "/flash" || line == "/pro" {
                let model = line.trim_start_matches('/');
                let _ = cmd_tx.send(OrchCmd::SetModel(model.to_string()));
            }
            continue;
        }
        if line == "exit" || line == "quit" {
            break;
        }
        if cancel.is_cancelled() {
            break;
        }
        let (done_tx, done_rx) = oneshot::channel();
        if cmd_tx
            .send(OrchCmd::UserInput {
                input: line,
                done: done_tx,
            })
            .is_err()
        {
            break;
        }
        let _ = done_rx.blocking_recv();
    }
}

async fn list_sessions(home: &Path, cwd: &Path) -> Result<()> {
    let mut rows = mink::session::metadata::list_project_sessions(home, cwd).await?;
    if rows.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    rows.sort_by(|a, b| b.modified.cmp(&a.modified));
    println!("{:<20} {:<32} {:<16} ID", "ALIAS", "TITLE", "UPDATED");
    for row in rows {
        let alias = row.metadata.alias.as_deref().unwrap_or("-");
        let title = row
            .metadata
            .title
            .as_deref()
            .or(row.metadata.summary.as_deref())
            .unwrap_or("-");
        let alias = truncate_chars(alias, 20);
        let title = truncate_chars(title, 32);
        let dt: time::OffsetDateTime = row.modified.into();
        let formatted = {
            use time::macros::format_description;
            static FMT: &[time::format_description::FormatItem<'_>] =
                format_description!("[year]-[month]-[day] [hour]:[minute]");
            dt.format(FMT)
                .unwrap_or_else(|_| format!("{:?}", row.modified))
        };
        println!("{:<20} {:<32} {:<16} {}", alias, title, formatted, row.id);
    }
    Ok(())
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = value.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn print_usage() {
    println!("Usage: mink [options] [prompt]");
    println!();
    println!("Options:");
    println!("  -m, --model TIER        Model tier: flash | pro (default: flash)");
    println!("  --api-key KEY           API key (default from env)");
    println!("  --base-url URL          Override API base URL");
    println!("  --mission PATH          Load custom system prompt from MISSION.md file");
    println!("  --session [NAME]        Use named session");
    println!("  --continue              Continue most recent session");
    println!("  --list-sessions         List saved sessions");
    println!("  --list-skills           List available skills");
    println!("  -v, --verbose           Verbose mode");
    println!("  -i, --interactive       Interactive mode (REPL)");
    println!("  --tui                   TUI mode");
    println!("  --print                 Stream JSON events to stdout");
    println!("  --agent-jsonl           Agent JSONL protocol");
    println!("  --disable-bash          Disable Bash tool");
    println!("  --disable-python        Disable Python tool");
    println!("  --disable-sub-agent     Disable SubAgent tool");
    println!("  --disable-web           Disable WebSearch/WebFetch tools");
    println!("  --enable-python-sandbox Enable PythonSandbox tool (default: disabled)");
    println!("  --config <toml>         Set config via TOML string");
    println!("                          Example: --config \"max_tokens=4096\\ntool_timeout=300\"");
    println!("                          Supports: model, max_tokens, max_turns, max_context,");
    println!("                          tool_timeout, sub_agent_timeout, llm_*_timeout,");
    println!("                          output_format, approval_mode, skills, verbose,");
    println!("                          and [sandbox_python] section");
    println!("  -h, --help              Show this help");
    println!();
    println!("Config via TOML (--config or .minkrc):");
    println!("  See .minkrc.example for a complete reference.");
    println!();
    println!("Environment:");
    println!("  DEEPSEEK_API_KEY        DeepSeek API key");
    println!("  DEEPSEEK_BASE_URL       DeepSeek base URL");
    println!("  LOG_EVENTS              Enable event logging (default: true)");
    println!("  MINK_SIGNAL_MODE        Signal system mode: off | full (default: full)");
}
