use crate::config::{
    apply_config_file, apply_provider_defaults, parse_args, validate_runtime_config,
};
use crate::runtime::{
    AgentOptions, AgentRuntime, AgentRuntimeHandle, RuntimeResult, TurnOutcome,
    apply_sdk_request_options, exit_code_from_turn, final_from_outcome,
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
                    let result = work_handle.run_turn(input).await;
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
        Err(e) if e.to_string() == "__HELP__" => {
            print_usage();
            return Ok(CliExit { code: 0 });
        }
        Err(e) if e.to_string() == "__VERSION__" => {
            println!("{}", version_line());
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
    if let Err(error) =
        mink::tools::catalog::validate_tool_config(&mink::context::ToolConfig::from_config(&cfg))
    {
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

    // Determine interactive mode early, before ctx creation
    let is_interactive =
        cfg.interactive || (cfg.prompt.is_empty() && std::io::stdin().is_terminal());
    cfg.interactive = is_interactive;
    let is_stream_json =
        cfg.output_format == crate::config::OutputFormat::StreamJson || cfg.agent_jsonl;

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

    let mut runtime_options = AgentOptions::from_config(cfg.clone(), home.clone(), cwd.clone())
        .with_project_scoped_sessions()
        .with_display(display.clone());
    if let Some(prompt) = prompt_for_title {
        runtime_options = runtime_options.with_first_prompt(prompt);
    }
    if let Some(layout) = sdk_request
        .as_ref()
        .and_then(|request| request.options.session_layout)
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
    if cfg.agent_jsonl {
        let outcome = runtime
            .run_turn(sdk_request.map(|r| r.prompt).unwrap_or_default())
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
        let outcome = runtime.run_turn(cfg.prompt.clone()).await?;
        emit_stream_json_final_if_needed(&cfg, &outcome);
        process_exit_code = exit_code_from_turn(outcome.status);
    } else {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let outcome = runtime.run_turn(input).await?;
        emit_stream_json_final_if_needed(&cfg, &outcome);
        process_exit_code = exit_code_from_turn(outcome.status);
    }

    // Auto-generate session title from first user input if missing
    auto_set_session_title(&session).await;

    runtime.shutdown().await?;

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
    let metadata_text = match tokio::fs::read_to_string(&metadata_path).await {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut metadata: serde_json::Value = match serde_json::from_str(&metadata_text) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Already has a non-empty title
    if metadata
        .get("title")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return;
    }
    let conversation_path = &session.conversation_path;
    let conv_text = match tokio::fs::read_to_string(conversation_path).await {
        Ok(t) => t,
        Err(_) => return,
    };
    for line in conv_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if msg.get("role").and_then(|v| v.as_str()) == Some("user") {
            let content = match msg.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => continue,
            };
            let title = crate::session::metadata::title_from_prompt(content);
            if let Some(title) = title {
                metadata["title"] = serde_json::Value::String(title);
                let now = time::OffsetDateTime::now_utc();
                let fmt = time::format_description::well_known::Rfc3339;
                let updated_at = now.format(&fmt).unwrap_or_default();
                metadata["updated_at"] = serde_json::Value::String(updated_at);
                if let Ok(text) = serde_json::to_string_pretty(&metadata) {
                    let _ = tokio::fs::write(&metadata_path, format!("{text}\n")).await;
                }
            }
            break;
        }
    }
}

fn reexec_if_sandbox(cfg: &crate::config::Config) {
    if cfg.sandbox.is_active() {
        let current_exe = std::env::current_exe().unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        crate::sandbox::reexec_in_sandbox(&cfg.sandbox, &current_exe, &args);
    }
}

fn emit_stream_json_final_if_needed(cfg: &crate::config::Config, outcome: &TurnOutcome) {
    if cfg.output_format != crate::config::OutputFormat::StreamJson || cfg.agent_jsonl {
        return;
    }
    crate::sdk_protocol::emit_json_line(&final_from_outcome(outcome));
}

fn list_skills() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("MINK_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );
    let snapshot = crate::capabilities::CapabilitySnapshot::load_default(
        &cwd,
        &home,
        "skills-list",
        "skills-list",
        &[],
    );

    println!("SKILLS");
    println!("{}", "-".repeat(60));
    match snapshot {
        Ok(snapshot) => {
            for skill in &snapshot.skills.discoverable {
                println!("   {} [{}]", skill.skill.name, skill.source_label());
                println!("      {}", skill.skill.description);
                println!();
            }
        }
        Err(e) => {
            println!("Error loading skills: {e}");
        }
    }

    println!("Load with --skill NAME or Read skill://NAME.");
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
            println!("Commands:");
            println!("  /flash        Switch to flash alias");
            println!("  /pro          Switch to pro alias");
            println!("  /model NAME   Switch to a model name or alias");
            println!("  /compact      Force context compaction");
            println!("  /skills       List available skills");
            println!("  /help         Show this help");
            println!("  Ctrl+C        Interrupt current task");
            println!("  Ctrl+C again  Exit REPL");
            println!("  exit / quit   Exit REPL");
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
            if cmd_tx.send(RuntimeCmd::SetModel(model.to_string())).is_err() {
                LocalCommandOutcome::Shutdown
            } else {
                LocalCommandOutcome::Handled
            }
        }
        _ if line.starts_with("/model ") => {
            let model = line["/model ".len()..].trim();
            if model.is_empty() {
                LocalCommandOutcome::Handled
            } else if cmd_tx.send(RuntimeCmd::SetModel(model.to_string())).is_err() {
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
        let alias = row.metadata.alias.as_deref().unwrap_or("-");
        let title = resolve_session_title(&row.path, &row.metadata).await;
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
    meta: &crate::session::metadata::SessionMetadata,
) -> String {
    // Fast path: title already in metadata
    if let Some(title) = meta.title.as_deref().filter(|t| !t.is_empty()) {
        return title.to_string();
    }
    if let Some(summary) = meta.summary.as_deref().filter(|s| !s.is_empty()) {
        return summary.to_string();
    }
    // Lazy path: generate title from first user input
    let metadata_path = session_dir.join("session.json");
    let metadata_text = match tokio::fs::read_to_string(&metadata_path).await {
        Ok(t) => t,
        Err(_) => return "-".to_string(),
    };
    let mut value: serde_json::Value = match serde_json::from_str(&metadata_text) {
        Ok(v) => v,
        Err(_) => return "-".to_string(),
    };
    // Double-check title in the raw JSON (may differ from cached struct)
    if let Some(t) = value.get("title").and_then(|v| v.as_str())
        && !t.is_empty()
    {
        return t.to_string();
    }
    let conv_path = session_dir.join("conversation.jsonl");
    let conv_text = match tokio::fs::read_to_string(&conv_path).await {
        Ok(t) => t,
        Err(_) => return "-".to_string(),
    };
    let title = conv_text
        .lines()
        .filter_map(|line| {
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
        })
        .next();
    let title = match title {
        Some(t) => t,
        None => return "-".to_string(),
    };
    // Write only the title field back, preserving all others
    value["title"] = serde_json::Value::String(title.clone());
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = tokio::fs::write(&metadata_path, format!("{text}\n")).await;
    }
    title
}

fn truncate_display(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    let keep = max_width.saturating_sub(3);
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str());
        if w + cw > keep {
            out.push_str("...");
            break;
        }
        out.push(c);
        w += cw;
    }
    out
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
    println!("  --session [NAME]        Use named session");
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
    println!("  --config <toml>         Set config via TOML string");
    println!("                          Example: --config \"max_tokens=4096\\ntool_timeout=300\"");
    println!("                          Supports: model, max_tokens, max_turns, max_context,");
    println!("                          context_compact_*, context_reserve_tokens,");
    println!("                          tool_timeout, sub_agent_timeout, llm_*_timeout,");
    println!("                          output_format, approval_mode, edit_*, skills, verbose,");
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
