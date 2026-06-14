use crate::agent::orchestrator::OrchCmd;
use crate::cancel::CancellationToken;
use crate::config::{apply_config_file, apply_provider_defaults, parse_args};
use crate::runtime::{
    AgentRuntimeConfig, SessionInfo, apply_sdk_request_options, exit_code_from_turn,
    final_from_run_result,
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
use std::sync::atomic::AtomicBool;
#[cfg(feature = "repl")]
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};

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
        Err(e) if e.to_string() == "__HELP__" => {
            print_usage();
            return Ok(CliExit { code: 0 });
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
        return Ok(CliExit { code: 0 });
    }
    if cfg.list_skills {
        list_skills();
        return Ok(CliExit { code: 0 });
    }

    apply_config_file(&mut cfg);

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

    let prompt_for_title = sdk_request
        .as_ref()
        .map(|r| r.prompt.clone())
        .or_else(|| (!cfg.prompt.trim().is_empty()).then(|| cfg.prompt.clone()));

    // Determine interactive mode early, before ctx creation
    let is_interactive =
        cfg.interactive || (cfg.prompt.is_empty() && std::io::stdin().is_terminal());
    cfg.interactive = is_interactive;
    let is_stream_json =
        cfg.output_format == crate::config::OutputFormat::StreamJson || cfg.agent_jsonl;

    // TUI channels (if tui_mode). Created before display so signal_tx is available.
    #[cfg(feature = "tui")]
    let tui_tx = if cfg.tui_mode {
        let (signal_tx, signal_rx) = std::sync::mpsc::channel::<crate::tui::TuiSignal>();
        Some((signal_tx, signal_rx))
    } else {
        None
    };
    #[cfg(not(feature = "tui"))]
    {
        if cfg.tui_mode {
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

    let mut runtime_config =
        AgentRuntimeConfig::from_config(cfg.clone(), home.clone(), cwd.clone())
            .with_display(display.clone())
            .with_first_prompt(prompt_for_title);
    if let Some(layout) = sdk_request
        .as_ref()
        .and_then(|request| request.options.session_layout)
    {
        runtime_config = runtime_config.with_session_layout(layout);
    }
    if let Some(sub_stream_tx) = sub_stream_tx {
        runtime_config = runtime_config.with_sub_stream_tx(sub_stream_tx);
    }
    let runtime = crate::runtime::AgentRuntime::start(runtime_config).await?;
    let session = runtime.session_info().clone();
    let cmd_tx = runtime.command_sender();
    let cancel = runtime.cancel_token();
    let interrupt = runtime.interrupt_flag();

    let mut process_exit_code = 0i32;
    if cfg.agent_jsonl {
        // SDK protocol: prompt was already parsed above; execute one turn.
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input: sdk_request.map(|r| r.prompt).unwrap_or_default(),
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            crate::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        crate::sdk_protocol::emit_json_line(&final_from_run_result(&result, &session));
        process_exit_code = exit_code_from_turn(result.status);
    } else if cfg.tui_mode {
        #[cfg(feature = "tui")]
        if let Some((_, signal_rx)) = tui_tx
            && let Err(e) = crate::tui::run_tui(
                signal_rx,
                cmd_tx.clone(),
                &session.events_path,
                Some(interrupt.clone()),
                &cfg.sandbox,
            )
        {
            eprintln!("TUI error: {e}");
        }
        #[cfg(not(feature = "tui"))]
        anyhow::bail!("this mink binary was built without the `tui` feature");
    } else if is_interactive {
        display.render_info("mink interactive mode (type 'exit' or Ctrl+D to quit)");
        if !session.is_new {
            replay_last_turns(&session.events_path);
        }
        run_interactive(cmd_tx, cancel.clone(), interrupt.clone(), &home).await?;
    } else if !cfg.prompt.is_empty() {
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input: cfg.prompt.clone(),
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            crate::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        emit_stream_json_final_if_needed(&cfg, &result, &session);
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
            crate::agent::orchestrator::TurnRunResult::failed(format!(
                "orchestrator dropped turn result: {e}"
            ))
        });
        emit_stream_json_final_if_needed(&cfg, &result, &session);
        process_exit_code = exit_code_from_turn(result.status);
    }

    runtime.shutdown().await?;

    if !session.session_id.is_empty() {
        eprintln!(
            "\x1b[90mResume with: --session {}  or  --continue\x1b[0m",
            session.session_ref
        );
    }

    Ok(CliExit {
        code: process_exit_code,
    })
}

fn reexec_if_sandbox(cfg: &crate::config::Config) {
    if cfg.sandbox.is_active() {
        let current_exe = std::env::current_exe().unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        crate::sandbox::reexec_in_sandbox(&cfg.sandbox, &current_exe, &args);
    }
}

fn emit_stream_json_final_if_needed(
    cfg: &crate::config::Config,
    result: &crate::agent::orchestrator::TurnRunResult,
    session: &SessionInfo,
) {
    if cfg.output_format != crate::config::OutputFormat::StreamJson || cfg.agent_jsonl {
        return;
    }
    crate::sdk_protocol::emit_json_line(&final_from_run_result(result, session));
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
    for skill in crate::skills::list_available_skills(&cwd, &home) {
        let source = match skill.source {
            crate::skills::SkillSource::BuiltIn => "built-in",
            crate::skills::SkillSource::FileSystem => "local",
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
    #[cfg(feature = "repl")]
    let history_path = home.join(".mink/history");
    #[cfg(not(feature = "repl"))]
    let _ = home;
    tokio::task::spawn_blocking(move || {
        #[cfg(feature = "repl")]
        {
            if let Some(parent) = history_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
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
        }
        #[cfg(not(feature = "repl"))]
        {
            let _ = interrupt;
            eprintln!("Readline support is disabled; running in simple stdin mode.");
            simple_stdin_loop(&cmd_tx, &cancel_clone);
        }
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
    let mut rows = crate::session::metadata::list_project_sessions(home, cwd).await?;
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
    let program = std::env::args()
        .next()
        .and_then(|arg| {
            std::path::Path::new(&arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "mink".to_string());
    println!("Usage: {program} [options] [prompt]");
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
    #[cfg(feature = "tui")]
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
