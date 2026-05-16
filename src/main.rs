use dscode::config::{api_url, apply_provider_defaults, parse_args};
use dscode::session::paths;
use dscode::session::stats::StatsTracker;
use dscode::session::compaction::CompactionEngine;
use dscode::session::store::ConversationStore;
use dscode::ui::engine::TerminalDisplay;
use dscode::ui::Display;
use dscode::ui::replay::replay_last_turns;
use dscode::cancel::CancellationToken;
use dscode::context::AgentSharedContext;
use dscode::agent::sub_pool::SubAgentPool;
use dscode::agent::orchestrator::{new_orchestrator, OrchCmd};
use anyhow::Result;
use std::io::IsTerminal;
use std::path::PathBuf;
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
        std::env::var("DSCODE_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );

    if cfg.list_sessions {
        list_sessions(&home, &cwd).await?;
        return Ok(());
    }

    apply_provider_defaults(&mut cfg)?;

    let mut sid = cfg.session_id.clone();
    if sid.is_empty() && cfg.continue_session {
        sid = paths::continue_session(&home, &cwd).await.unwrap_or_default();
    }
    if sid.is_empty() {
        sid = paths::chrono_session_id();
    }
    cfg.session_id = sid.clone();

    let spaths = paths::paths_for(&home, &cwd, &sid);
    paths::ensure_dir(&spaths.base_dir).await?;
    paths::ensure_dir(&spaths.session_dir).await?;

    let new_session = !spaths.events.exists();
    for f in [&spaths.conversation, &spaths.events, &spaths.summary, &spaths.plan, &spaths.plan_draft] {
        if !f.exists() {
            let _ = tokio::fs::File::create(f).await;
        }
    }

    if new_session {
        let initial_stats = r#"{"current_turn_count":0,"agent_request_count":0,"compact_request_count":0,"sub_agent_request_count":0,"total_input_tokens":0,"total_output_tokens":0,"total_cache_read_tokens":0,"total_cache_creation_tokens":0,"current_context_tokens":0,"last_updated":""}"#;
        tokio::fs::write(&spaths.stats, format!("{initial_stats}\n")).await?;
    }

    let store = Arc::new(ConversationStore::new(spaths.conversation.clone()));
    store.ensure().await?;

    let stats = StatsTracker::load(&spaths.stats).await?;
    let api_url_str = api_url(&cfg);

    // Determine interactive mode early, before ctx creation
    let is_interactive = cfg.interactive || (cfg.prompt.is_empty() && std::io::stdin().is_terminal());
    cfg.interactive = is_interactive;

    // Shared HTTP client for both LLM calls and compaction
    let shared_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("dscode/3.0")
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
    let is_stream_json = cfg.output_format == dscode::config::OutputFormat::StreamJson;
    let display: Arc<dyn Display> = Arc::new(TerminalDisplay::new(is_interactive, is_stream_json));

    let ctx = Arc::new(AgentSharedContext {
        config: cfg.clone(),
        cwd: cwd.clone(),
        home: home.clone(),
        api_url: api_url_str.clone(),
        store,
        stats,
        compaction,
        cancel: cancel.clone(),
        display: display.clone(),
        tool_timeout_secs: cfg.tool_timeout_secs,
        tool_result_max_bytes: cfg.tool_result_max_bytes,
        file_write_max_bytes: cfg.file_write_max_bytes,
        events_path: spaths.events.clone(),
        summary_path: spaths.summary.clone(),
        plan_path: spaths.plan.clone(),
        plan_draft_path: spaths.plan_draft.clone(),
        immutable_prefix: Mutex::new(None),
    });

    let (sub_result_tx, mut sub_result_rx) = mpsc::unbounded_channel();
    let sub_pool = Arc::new(SubAgentPool::new(8, sub_result_tx));

    let (orchestrator, cmd_tx) = new_orchestrator(ctx.clone(), sub_pool.clone());

    // Bridge sub_agent results → orchestrator commands
    let cmd_tx_clone = cmd_tx.clone();
    tokio::spawn(async move {
        while let Some(report) = sub_result_rx.recv().await {
            let _ = cmd_tx_clone.send(OrchCmd::SubAgentResult(report));
        }
    });

    let orch_display = display.clone();
    let orch_handle = tokio::spawn(async move {
        if let Err(e) = orchestrator.run().await {
            orch_display.render_error(&format!("Orchestrator: {e}"));
        }
    });

    if new_session {
        ctx.log_event(serde_json::json!({"type":"session_start","session_id":sid}));
    }

    if is_interactive {
        display.render_info("dscode interactive mode (type 'exit' or Ctrl+D to quit)");
        if !new_session {
            replay_last_turns(&spaths.events);
        }
        run_interactive(cmd_tx, cancel.clone(), &home).await?;
    } else if !cfg.prompt.is_empty() {
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput { input: cfg.prompt.clone(), done: done_tx })?;
        let _ = done_rx.await;
        while sub_pool.active_count() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    } else {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput { input, done: done_tx })?;
        let _ = done_rx.await;
        while sub_pool.active_count() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    cancel.cancel();
    let _ = orch_handle.await;

    if !cfg.session_id.is_empty() {
        eprintln!("\x1b[90mResume with: --session {}  or  --continue\x1b[0m", cfg.session_id);
    }

    Ok(())
}

async fn run_interactive(
    cmd_tx: mpsc::UnboundedSender<OrchCmd>,
    cancel: CancellationToken,
    home: &PathBuf,
) -> Result<()> {
    let cancel_clone = cancel.clone();
    let history_path = home.join(".dscode/history");
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
                    cancel_clone.cancel();
                    break;
                }
                Err(_) => break,
            };
            if line.is_empty() {
                continue;
            }
            if line == "/help" {
                println!("Commands:");
                println!("  /flash        Switch to flash model (deepseek-v4-flash)");
                println!("  /pro          Switch to pro model (deepseek-v4-pro)");
                println!("  /help         Show this help");
                println!("  exit / quit   Exit REPL");
                continue;
            }
            if line == "/flash" || line == "/pro" {
                let model = line.trim_start_matches('/');
                if cmd_tx
                    .send(OrchCmd::SetModel(model.to_string()))
                    .is_err()
                {
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
            let (done_tx, done_rx) = oneshot::channel();
            if cmd_tx.send(OrchCmd::UserInput { input: line, done: done_tx }).is_err() {
                break;
            }
            let _ = done_rx.blocking_recv();
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
        if line.is_empty() { continue; }
        if line.starts_with('/') {
            if line == "/help" {
                println!("Commands: /flash, /pro, /help");
            } else if line == "/flash" || line == "/pro" {
                let model = line.trim_start_matches('/');
                let _ = cmd_tx.send(OrchCmd::SetModel(model.to_string()));
            }
            continue;
        }
        if line == "exit" || line == "quit" { break; }
        if cancel.is_cancelled() { break; }
        let (done_tx, done_rx) = oneshot::channel();
        if cmd_tx.send(OrchCmd::UserInput { input: line, done: done_tx }).is_err() { break; }
        let _ = done_rx.blocking_recv();
    }
}

async fn list_sessions(home: &PathBuf, cwd: &PathBuf) -> Result<()> {
    use std::time::UNIX_EPOCH;
    let dir = home.join(".dscode/projects").join(paths::project_key(cwd));
    if !dir.exists() {
        println!("No sessions found.");
        return Ok(());
    }
    struct Row {
        name: String,
        ts: std::time::SystemTime,
        summary: String,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(e) = entries.next_entry().await? {
        if !e.path().is_dir() { continue; }
        let name = e.file_name().to_string_lossy().to_string();
        let summary_path = e.path().join("summary.txt");
        let mut summary = String::new();
        if let Ok(data) = tokio::fs::read_to_string(&summary_path).await {
            for line in data.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() { summary = trimmed.to_string(); break; }
            }
        }
        rows.push(Row { name, ts: e.metadata().await?.modified().unwrap_or(UNIX_EPOCH), summary });
    }
    if rows.is_empty() { println!("No sessions found."); return Ok(()); }
    rows.sort_by(|a, b| b.ts.cmp(&a.ts));
    println!("{:<40} {:<16} PREVIEW", "NAME", "MODIFIED");
    for row in rows {
        let mut preview = row.summary;
        if preview.len() > 60 { preview.truncate(57); preview.push_str("..."); }
        let dt: Result<time::OffsetDateTime, _> = row.ts.try_into();
        let formatted = match dt {
            Ok(dt) => {
                use time::macros::format_description;
                static FMT: &[time::format_description::FormatItem<'_>] = format_description!("[year]-[month]-[day] [hour]:[minute]");
                dt.format(FMT).unwrap_or_else(|_| format!("{:?}", row.ts))
            }
            Err(_) => format!("{:?}", row.ts),
        };
        println!("{:<40} {:<16} {}", row.name, formatted, preview);
    }
    Ok(())
}

fn print_usage() {
    println!("Usage: dscode [options] [prompt]");
    println!();
    println!("Options:");
    println!("  -m, --model MODEL       Model name (default: deepseek-v4-flash)");
    println!("  --max-tokens N          Max output tokens (default: 4096)");
    println!("  --tool-timeout N        Tool execution timeout in seconds (default: 600)");
    println!("  --skill NAME            Load skill from .claude/skills/NAME/SKILL.md");
    println!("  --max-turns N           Max agent turns (default: 40)");
    println!("  --max-context N         Max stored context tokens (default: 200000)");
    println!("  --api-key KEY           API key (default from env)");
    println!("  --base-url URL          Override API base URL (default: api.deepseek.com)");
    println!("  --output-format FMT     Output format: human | stream-json");
    println!("  --print                 Alias for --output-format stream-json");
    println!("  --session [NAME]        Use named session");
    println!("  --continue              Continue most recent session");
    println!("  --list-sessions         List saved sessions");
    println!("  -v, --verbose           Verbose mode");
    println!("  -i, --interactive       Interactive mode (REPL)");
    println!("  -h, --help              Show this help");
    println!();
    println!("Environment:");
    println!("  DEEPSEEK_API_KEY        DeepSeek API key (also reads OPENAI_API_KEY)");
    println!("  DEEPSEEK_BASE_URL       DeepSeek base URL (default: https://api.deepseek.com/v1)");
    println!("  AUTO_MODEL              Enable auto-model upgrade (true/1)");
    println!("  SECONDARY_MODEL         Auto-model upgrade target model");
    println!("  AUTO_SELF_REPORT        Enable NEEDS_PRO self-report upgrade (true/1)");
    println!("  LOG_EVENTS              Enable event logging (default: true, 0/false/no disables)");
}
