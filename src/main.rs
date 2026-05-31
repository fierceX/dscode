use anyhow::Result;
use dscode::agent::orchestrator::{OrchCmd, new_orchestrator};
use dscode::cancel::CancellationToken;
use dscode::config::{api_url, apply_config_file, apply_provider_defaults, parse_args};
use dscode::context::AgentSharedContext;
use dscode::context::ToolConfig;
use dscode::session::compaction::CompactionEngine;
use dscode::session::paths;
use dscode::ui::Display;
use dscode::ui::engine::TerminalDisplay;
use dscode::ui::replay::replay_last_turns;
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
        std::env::var("DSCODE_HOME")
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

    // ═══ JSON-RPC mode: early stdin parsing (before context creation) ═══
    // We parse the request here so that tool_disable flags take effect
    // when AgentSharedContext and ToolConfig are constructed.
    let json_rpc_prompt: Option<String> = if cfg.json_rpc {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let req: serde_json::Value =
            serde_json::from_str(&input).unwrap_or_else(|_| serde_json::json!({"prompt": input}));
        // Apply optional tool disable flags and config overrides
        if let Some(opts) = req.get("options") {
            if opts
                .get("disable_bash")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                cfg.tool_disable.disable_bash = true;
            }
            if opts
                .get("disable_sub_agent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                cfg.tool_disable.disable_sub_agent = true;
            }
            if opts
                .get("disable_web")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                cfg.tool_disable.disable_web = true;
            }
            if opts
                .get("disable_python")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                cfg.tool_disable.disable_python = true;
            }
            // Config overrides from JSON-RPC
            if let Some(v) = opts
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            {
                cfg.model = v;
                cfg.cli_overrides.model = true;
            }
            if let Some(v) = opts
                .get("max_tokens")
                .and_then(|v| v.as_i64())
                .map(|i| i as i32)
            {
                cfg.max_tokens = v;
                cfg.cli_overrides.max_tokens = true;
            }
            if let Some(v) = opts
                .get("max_turns")
                .and_then(|v| v.as_i64())
                .map(|i| i as i32)
            {
                cfg.max_turns = v.max(1);
                cfg.cli_overrides.max_turns = true;
            }
            if let Some(v) = opts
                .get("tool_timeout")
                .and_then(|v| v.as_i64())
                .map(|i| i as i32)
            {
                cfg.tool_timeout_secs = v.max(5);
                cfg.cli_overrides.tool_timeout_secs = true;
            }
            if let Some(v) = opts
                .get("sub_agent_timeout")
                .and_then(|v| v.as_i64())
                .map(|i| i as i32)
            {
                cfg.sub_agent_timeout_secs = v.max(5);
                cfg.cli_overrides.sub_agent_timeout_secs = true;
            }
            if let Some(v) = opts.get("verbose").and_then(|v| v.as_bool()) {
                cfg.verbose = v;
            }
        }
        // session_id at top level (or in options)
        if let Some(v) = req.get("session_id").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                cfg.session_id = v.to_string();
            }
        } else if let Some(v) = req
            .get("options")
            .and_then(|o| o.get("session_id"))
            .and_then(|v| v.as_str())
        {
            if !v.is_empty() {
                cfg.session_id = v.to_string();
            }
        }
        Some(
            req.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&input)
                .to_string(),
        )
    } else {
        None
    };

    // ═══ Self-sandboxing: re-exec into nsjail/bwrap/sandbox-exec ═══
    // If successful, the process is replaced and we never reach the next line.
    // If it fails, we log a warning and continue without sandbox.
    if cfg.sandbox.is_active() {
        let current_exe = std::env::current_exe().unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        // ⚠ Blocking call — ok here because we haven't entered the async runtime yet
        dscode::sandbox::reexec_in_sandbox(&cfg.sandbox, &current_exe, &args);
    }

    let mut sid = cfg.session_id.clone();
    if sid.is_empty() && cfg.continue_session {
        sid = paths::continue_session(&home, &cwd)
            .await
            .unwrap_or_default();
    }
    if sid.is_empty() {
        sid = paths::chrono_session_id();
    }
    cfg.session_id = sid.clone();

    let spaths = paths::paths_for(&home, &cwd, &sid);

    let new_session = !spaths.events.exists();

    // 共享会话初始化
    let (store, stats) = dscode::session::init::init_session_base(&home, &cwd, &sid).await?;
    let api_url_str = api_url(&cfg);

    // Determine interactive mode early, before ctx creation
    let is_interactive =
        cfg.interactive || (cfg.prompt.is_empty() && std::io::stdin().is_terminal());
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

    // TUI channels (if tui_mode). Created before display so signal_tx is available.
    let tui_tx = if cfg.tui_mode {
        let (signal_tx, signal_rx) = std::sync::mpsc::channel::<dscode::tui::TuiSignal>();
        Some((signal_tx, signal_rx))
    } else {
        None
    };

    let display: Arc<dyn Display> = if let Some((ref tx, ..)) = tui_tx {
        Arc::new(dscode::tui::TuiDisplay::new(tx.clone())) as Arc<dyn Display>
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

    if cfg.json_rpc {
        // JSON-RPC: prompt was already parsed above; just execute and emit turn-end
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input: json_rpc_prompt.unwrap_or_default(),
            done: done_tx,
        })?;
        let _ = done_rx.await;
        // End marker so caller knows processing is complete
        println!(r#"{{"type":"turn_end","status":"ok"}}"#);
    } else if cfg.tui_mode {
        if let Some((_, signal_rx)) = tui_tx
            && let Err(e) = dscode::tui::run_tui(
                signal_rx,
                cmd_tx.clone(),
                &spaths.events,
                Some(ctx.interrupt.clone()),
            )
        {
            eprintln!("TUI error: {e}");
        }
    } else if is_interactive {
        display.render_info("dscode interactive mode (type 'exit' or Ctrl+D to quit)");
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
        let _ = done_rx.await;
    } else {
        let mut input = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input).await?;
        let (done_tx, done_rx) = oneshot::channel();
        cmd_tx.send(OrchCmd::UserInput {
            input,
            done: done_tx,
        })?;
        let _ = done_rx.await;
    }

    cancel.cancel();
    let _ = orch_handle.await;

    if !cfg.session_id.is_empty() {
        eprintln!(
            "\x1b[90mResume with: --session {}  or  --continue\x1b[0m",
            cfg.session_id
        );
    }

    Ok(())
}

fn list_skills() {
    let embedded = dscode::assets::embedded_skills::all();
    let mut seen_fs = std::collections::HashSet::new();
    let mut fs_skills: Vec<String> = Vec::new();

    // Scan file-system skill dirs
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(
        std::env::var("DSCODE_HOME")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| String::from(".")),
    );
    let bases = find_list_skill_dirs(&cwd, &home);
    for base in &bases {
        if !base.is_dir() {
            continue;
        }
        for entry in (std::fs::read_dir(base).into_iter().flatten()).flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with('.') || !seen_fs.insert(name.clone()) {
                continue;
            }
            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&skill_file).unwrap_or_default();
            let desc = extract_frontmatter_field_for_list(&content, "description:")
                .unwrap_or_else(|| String::from("(no description)"));
            fs_skills.push(format!("{}: {}", name, desc));
        }
    }

    // Show embedded skills
    println!("BUILT-IN SKILLS");
    println!("{}", "-".repeat(60));
    for skill in &embedded {
        let marker = if seen_fs.contains(skill.name) {
            "▶"
        } else {
            " "
        };
        println!("{}  {}", marker, skill.name);
        println!("{}     {}", marker, skill.description);
        println!();
    }

    // Show filesystem-only skills
    let fs_only: Vec<_> = fs_skills
        .iter()
        .filter(|s| {
            !embedded
                .iter()
                .any(|e| s.starts_with(&format!("{}:", e.name)))
        })
        .collect();
    if !fs_only.is_empty() {
        println!("FILESYSTEM SKILLS");
        println!("{}", "-".repeat(60));
        for s in &fs_only {
            println!("   {}", s);
        }
        println!();
    }

    println!("Load with --skill NAME or Skill(name).");
}

fn find_list_skill_dirs(cwd: &std::path::Path, home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let project = cwd.join(".claude/skills");
    if project.is_dir() {
        out.push(project);
    }
    let project_dev = cwd.join("skills");
    if project_dev.is_dir() {
        out.push(project_dev);
    }
    let global = home.join(".claude/skills");
    if global.is_dir() {
        out.push(global);
    }
    out
}

fn extract_frontmatter_field_for_list(content: &str, field: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.first()?.trim() != "---" {
        return None;
    }
    for line in &lines[1..] {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix(field) {
            let val = value.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

async fn run_interactive(
    cmd_tx: mpsc::UnboundedSender<OrchCmd>,
    cancel: CancellationToken,
    interrupt: Arc<AtomicBool>,
    home: &Path,
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
                    Ok(()) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
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
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let summary_path = e.path().join("summary.txt");
        let mut summary = String::new();
        if let Ok(data) = tokio::fs::read_to_string(&summary_path).await {
            for line in data.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    summary = trimmed.to_string();
                    break;
                }
            }
        }
        rows.push(Row {
            name,
            ts: e.metadata().await?.modified().unwrap_or(UNIX_EPOCH),
            summary,
        });
    }
    if rows.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    rows.sort_by(|a, b| b.ts.cmp(&a.ts));
    println!("{:<40} {:<16} PREVIEW", "NAME", "MODIFIED");
    for row in rows {
        let mut preview = row.summary;
        if preview.len() > 60 {
            preview.truncate(57);
            preview.push_str("...");
        }
        let dt: time::OffsetDateTime = row.ts.into();
        let formatted = {
            use time::macros::format_description;
            static FMT: &[time::format_description::FormatItem<'_>] =
                format_description!("[year]-[month]-[day] [hour]:[minute]");
            dt.format(FMT).unwrap_or_else(|_| format!("{:?}", row.ts))
        };
        println!("{:<40} {:<16} {}", row.name, formatted, preview);
    }
    Ok(())
}

fn print_usage() {
    println!("Usage: dscode [options] [prompt]");
    println!();
    println!("Options:");
    println!("  -m, --model MODEL       Model tier: flash | pro (default: flash)");
    println!("  --max-tokens N          Max output tokens (default: 81920)");
    println!("  --tool-timeout N        Tool execution timeout in seconds (default: 600)");
    println!("  --sub-agent-timeout N   Sub-agent execution timeout in seconds (default: 300)");
    println!("  --skill NAME            Load skill from .claude/skills/NAME/SKILL.md");
    println!("  --mission PATH          Load custom system prompt from MISSION.md file");
    println!("  --max-turns N           Max agent turns (default: 40)");
    println!("  --max-context N         Max stored context tokens (default: 1M)");
    println!("  --api-key KEY           API key (default from env)");
    println!("  --base-url URL          Override API base URL (default: api.deepseek.com)");
    println!("  --output-format FMT     Output format: human | stream-json");
    println!("  --print                 Alias for --output-format stream-json");
    println!("  --session [NAME]        Use named session");
    println!("  --continue              Continue most recent session");
    println!("  --list-sessions         List saved sessions");
    println!("  --list-skills           List built-in skills");
    println!("  -v, --verbose           Verbose mode");
    println!("  -i, --interactive       Interactive mode (REPL)");
    println!("  --tui                   TUI mode (alternate screen with status bar)");
    println!(
        "  --json-rpc              JSON-RPC mode (read request from stdin, emit events to stdout)"
    );
    println!("  --disable-bash          Disable Bash tool");
    println!("  --disable-python        Disable Python tool");
    println!("  --disable-sub-agent     Disable SubAgent tool");
    println!("  --disable-web           Disable WebSearch/WebFetch tools");
    println!("  -h, --help              Show this help");
    println!();
    println!("Environment:");
    println!("  DEEPSEEK_API_KEY        DeepSeek API key");
    println!("  DEEPSEEK_BASE_URL       DeepSeek base URL (default: https://api.deepseek.com/v1)");
    println!("  LOG_EVENTS              Enable event logging (default: true, 0/false/no disables)");
    println!("  DSCODE_SIGNAL_MODE      Signal system mode: off | full (default: full)");
}
