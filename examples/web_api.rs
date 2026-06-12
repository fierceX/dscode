//! mink hidden-worker web API demo.
//!
//! Demonstrates the private-deployment pattern from the design doc: a single
//! binary acts as an HTTP API **server** in normal mode and as a sandboxed
//! **hidden worker** when spawned with `--internal-mink-worker`.
//!
//! ## Data flow (same as mink CLI)
//!
//! ```text
//! Server                               Worker
//!   │                                    │
//!   ├─ spawn with argv:                  │
//!   │    --internal-mink-worker          │  1. parse argv → SandboxConfig
//!   │    --home ...   --cwd ...          │     (home, cwd, read_dirs, write_dirs)
//!   │    --read-dir ... --write-dir ...  │
//!   │    --api-key ... --model flash     │
//!   │                                    │  2. sandbox::reexec_in_sandbox()
//!   │                                    │     stdin 未读，管道数据完好
//!   │                                    │
//!   │  write(stdin, {"prompt":"..."})    │  3. 沙箱子进程: read stdin → prompt
//!   │  close(stdin)                      │
//!   │                                    │  4. AgentRuntime::start → run_turn
//!   │                                    │
//!   │  ◄── read stdout ──────────────── │  5. println!(result JSON)
//! ```
//!
//! ## Usage
//!
//! ```bash
//! DEEPSEEK_API_KEY=sk-xxx cargo run --example web_api --features web-api
//!
//! curl -X POST localhost:3000/task -H 'Content-Type: application/json' \
//!   -d '{"prompt":"Explain Rust ownership"}'
//! curl localhost:3000/task/<task_id>
//! ```

use axum::{
    Json, Router,
    extract::State,
    response::IntoResponse,
    routing::{get, post},
};
use mink::config::SandboxConfig;
use mink::runtime::{AgentOptions, AgentRuntime, SessionPolicy, TurnStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── IPC types ─────────────────────────────────────────────────────────

/// Task payload written to worker's stdin (re-exec safe — read after sandbox).
#[derive(Serialize, Deserialize, Debug)]
struct TaskRequest {
    prompt: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WorkerResult {
    status: String,
    text: String,
    thinking: String,
    tool_calls: u32,
    tool_errors: u32,
    error: Option<String>,
    session_id: String,
    home: String,
    cwd: String,
}

// ── Public API types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateTask {
    prompt: String,
}

#[derive(Serialize)]
struct CreateResponse {
    task_id: String,
    status: String,
}

#[derive(Serialize, Clone)]
struct TaskStatus {
    status: String,
    text: Option<String>,
    thinking: Option<String>,
    tool_calls: Option<u32>,
    tool_errors: Option<u32>,
    error: Option<String>,
    session_id: Option<String>,
    home: Option<String>,
    cwd: Option<String>,
}

impl TaskStatus {
    fn queued() -> Self {
        Self {
            status: "queued".into(),
            text: None, thinking: None, tool_calls: None, tool_errors: None,
            error: None, session_id: None, home: None, cwd: None,
        }
    }
    fn running() -> Self {
        Self {
            status: "running".into(),
            text: None, thinking: None, tool_calls: None, tool_errors: None,
            error: None, session_id: None, home: None, cwd: None,
        }
    }
}

// ── Server state ──────────────────────────────────────────────────────

struct AppState {
    tasks: RwLock<HashMap<String, TaskStatus>>,
}

// ── Workspace helpers ─────────────────────────────────────────────────

const WORK_BASE: &str = "/tmp/mink-demo-tasks";

fn workspace_for(task_id: &str) -> PathBuf {
    PathBuf::from(WORK_BASE).join(task_id)
}

fn create_workspace(task_id: &str) -> Result<(PathBuf, PathBuf), String> {
    let cwd = workspace_for(task_id);
    let home = cwd.join(".mink_home");
    std::fs::create_dir_all(&home).map_err(|e| format!("mkdir home: {e}"))?;
    std::fs::create_dir_all(&cwd).map_err(|e| format!("mkdir cwd: {e}"))?;
    Ok((home, cwd))
}

// ── Helpers ───────────────────────────────────────────────────────────

fn random_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:016x}", nanos)
}

fn api_key() -> String {
    std::env::var("DEEPSEEK_API_KEY").unwrap_or_default()
}

// ═══ Worker argv parser ═══════════════════════════════════════════════
// Parse sandbox & runtime config from argv (survives execve).
// Mirrors the cli.rs parse_args → Config flow inside the worker.

struct WorkerArgs {
    home: String,
    cwd: String,
    read_dirs: Vec<String>,
    write_dirs: Vec<String>,
    api_key: String,
    model: String,
}

fn parse_worker_args(args: &[String]) -> Option<WorkerArgs> {
    fn next_arg(args: &[String], i: usize) -> Option<&str> {
        args.get(i + 1).map(|s| s.as_str())
    }
    let mut home = None;
    let mut cwd = None;
    let mut read_dirs = Vec::new();
    let mut write_dirs = Vec::new();
    let mut api_key = None;
    let mut model = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--home" => { home = next_arg(args, i).map(String::from); i += 1; }
            "--cwd" => { cwd = next_arg(args, i).map(String::from); i += 1; }
            "--read-dir" => { if let Some(d) = next_arg(args, i) { read_dirs.push(d.into()); } i += 1; }
            "--write-dir" => { if let Some(d) = next_arg(args, i) { write_dirs.push(d.into()); } i += 1; }
            "--api-key" => { api_key = next_arg(args, i).map(String::from); i += 1; }
            "--model" => { model = next_arg(args, i).map(String::from); i += 1; }
            _ => {}
        }
        i += 1;
    }
    Some(WorkerArgs {
        home: home?,
        cwd: cwd?,
        read_dirs,
        write_dirs,
        api_key: api_key?,
        model: model.unwrap_or_else(|| "flash".into()),
    })
}

// ── HTTP handlers ─────────────────────────────────────────────────────

async fn create_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateTask>,
) -> impl IntoResponse {
    let token = random_token();
    state.tasks.write().await.insert(token.clone(), TaskStatus::queued());

    let state = state.clone();
    let tid = token.clone();
    let prompt = req.prompt.clone();

    tokio::spawn(async move {
        state.tasks.write().await.insert(tid.clone(), TaskStatus::running());
        let result = spawn_worker_and_wait(&tid, &prompt).await;

        let status = match result {
            Ok(r) => TaskStatus {
                status: r.status,
                text: Some(r.text), thinking: Some(r.thinking),
                tool_calls: Some(r.tool_calls), tool_errors: Some(r.tool_errors),
                error: r.error,
                session_id: Some(r.session_id), home: Some(r.home), cwd: Some(r.cwd),
            },
            Err(e) => TaskStatus {
                status: "failed".into(),
                text: None, thinking: None, tool_calls: None, tool_errors: None,
                error: Some(e), session_id: None, home: None, cwd: None,
            },
        };
        state.tasks.write().await.insert(tid, status);
    });

    Json(CreateResponse { task_id: token, status: "queued".into() })
}

async fn get_task(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let task_id = params.get("id").cloned().unwrap_or_default();
    let tasks = state.tasks.read().await;
    eprintln!("[server] GET /task?id={task_id}  keys={:?}", tasks.keys().collect::<Vec<_>>());
    match tasks.get(&task_id) {
        Some(s) => Json(s.clone()).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "task not found", "known_tasks": tasks.keys().collect::<Vec<_>>()})),
        ).into_response(),
    }
}

async fn list_tasks(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let tasks = state.tasks.read().await;
    let keys: Vec<_> = tasks.keys().cloned().collect();
    eprintln!("[server] GET /tasks  keys={:?}", keys);
    Json(serde_json::json!({
        "count": tasks.len(),
        "tasks": keys,
    }))
}

async fn fallback(uri: axum::http::Uri) -> impl IntoResponse {
    eprintln!("[server] 404 not found: {uri}");
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "not found", "uri": uri.to_string()})),
    )
}

// ── Worker spawn (server side) ────────────────────────────────────────

async fn spawn_worker_and_wait(token: &str, prompt: &str) -> Result<WorkerResult, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

    let (home, cwd) = create_workspace(token)?;
    let home_s = home.to_string_lossy().into_owned();
    let cwd_s = cwd.to_string_lossy().into_owned();

    eprintln!("[server] task={token} workspace={cwd_s}");

    // Sandbox + runtime config → argv (same pattern as mink CLI).
    // Only the prompt goes to stdin, read after re-exec inside the sandbox.
    let mut child = std::process::Command::new(&exe)
        .args([
            "--internal-mink-worker",
            "--home", &home_s,
            "--cwd", &cwd_s,
            "--read-dir", &cwd_s,
            "--write-dir", &cwd_s,
            "--api-key", &api_key(),
            "--model", "flash",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn worker: {e}"))?;

    // Write the prompt to stdin — worker reads it after re-exec.
    let task_req = serde_json::to_string(&TaskRequest { prompt: prompt.to_string() })
        .map_err(|e| format!("json: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(task_req.as_bytes()).map_err(|e| format!("write stdin: {e}"))?;
        drop(stdin); // close → EOF
    }

    let stdout = child.stdout.take().ok_or_else(|| "no stdout".to_string())?;
    let (last_line, exit_status) = tokio::task::spawn_blocking(move || {
        let reader = BufReader::new(stdout);
        let mut last = String::new();
        for line in reader.lines() {
            if let Ok(l) = line { if !l.trim().is_empty() { last = l; } }
        }
        let status = child.wait().map_err(|e| format!("wait: {e}"))?;
        Ok::<_, String>((last, status))
    })
    .await
    .map_err(|e| format!("join: {e}"))??;

    if !exit_status.success() {
        return Err(format!("worker exited with {exit_status}"));
    }
    if last_line.is_empty() {
        return Err("worker produced no output".into());
    }
    serde_json::from_str(&last_line).map_err(|e| format!("parse worker result: {e}"))
}

// ── Hidden worker entry point ─────────────────────────────────────────

async fn run_hidden_worker() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();

    // 1. Parse sandbox & runtime config from argv.
    let wa = parse_worker_args(&args).ok_or_else(|| "missing worker args".to_string())?;

    eprintln!(
        "[worker] home={} cwd={} read_dirs={:?} write_dirs={:?}",
        wa.home, wa.cwd, wa.read_dirs, wa.write_dirs,
    );

    // 2. Sandbox re-exec — stdin not yet read, pipe data intact.
    let sandbox = SandboxConfig {
        enabled: true,
        backend: "auto".into(),
        read_dirs: wa.read_dirs.clone(),
        write_dirs: wa.write_dirs.clone(),
        allow_bash: true,
        allow_network: true,
        ..Default::default()
    };
    mink::sandbox::reexec_in_sandbox(
        &sandbox,
        &std::env::current_exe().unwrap_or_default(),
        &args,
    );

    // 3. Read task prompt from stdin (inside sandbox, after re-exec).
    let mut input = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input)
        .await
        .map_err(|e| format!("read stdin: {e}"))?;
    let task: TaskRequest =
        serde_json::from_str(&input).map_err(|e| format!("parse task: {e}"))?;

    // 4. Build and run mink runtime.
    let opts = AgentOptions::new(wa.home.clone(), wa.cwd.clone())
        .with_api_key(wa.api_key)
        .with_model(wa.model)
        .with_max_turns(25)
        .with_session(SessionPolicy::New)
        .with_sandbox(sandbox);

    let rt = AgentRuntime::start_with_options(opts)
        .await
        .map_err(|e| format!("runtime start: {e}"))?;

    let session = rt.session_info().clone();
    eprintln!(
        "[worker] session_id={} events={}",
        session.session_id,
        session.events_path.display()
    );

    let outcome = rt.run_turn(&task.prompt).await.map_err(|e| format!("run_turn: {e}"))?;
    rt.shutdown().await.map_err(|e| format!("shutdown: {e}"))?;

    // 5. Emit result JSON.
    let result = WorkerResult {
        status: match outcome.status {
            TurnStatus::Ok => "ok".into(),
            _ => "failed".into(),
        },
        text: outcome.text,
        thinking: outcome.thinking,
        tool_calls: outcome.tool_call_count,
        tool_errors: outcome.tool_error_count,
        error: outcome.error,
        session_id: session.session_id,
        home: session.home.to_string_lossy().into_owned(),
        cwd: session.cwd.to_string_lossy().into_owned(),
    };
    println!("{}", serde_json::to_string(&result).map_err(|e| format!("json: {e}"))?);
    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--internal-mink-worker") {
        if let Err(e) = run_hidden_worker().await {
            eprintln!("worker error: {e}");
            std::process::exit(1);
        }
        return;
    }

    if api_key().is_empty() {
        eprintln!("Set DEEPSEEK_API_KEY environment variable");
        std::process::exit(1);
    }

    std::fs::create_dir_all(WORK_BASE).ok();

    let state = Arc::new(AppState { tasks: RwLock::new(HashMap::new()) });
    let app = Router::new()
        .route("/task", post(create_task))
        .route("/task", get(get_task))
        .route("/tasks", get(list_tasks))
        .fallback(fallback)
        .with_state(state);

    println!("mink web API demo — http://localhost:3000");
    println!("  POST /task       submit a task");
    println!("  GET  /task?id=<id>  poll for result");
    println!("  GET  /tasks      list all tasks");
    println!("[server] listening on :3000");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.expect("bind :3000");
    axum::serve(listener, app).await.expect("server");
}
