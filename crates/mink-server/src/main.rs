//! mink-server: web workspace server for remote interactive mink sessions.
//!
//! Single binary: REST + SSE API plus the embedded web UI. Sessions live in
//! the default mink home (`~/.mink/projects/...`) and share `events.jsonl`
//! with the TUI, enabling seamless hand-off between terminal and browser.

use crate::session::config::{ServerConfig, validate_runtime_config};
use crate::session::registry::Registry;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

mod api;
mod bridge;
mod session;
mod web_assets;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("-V") | Some("--version") => {
            println!("{}", version_line());
            return Ok(());
        }
        Some("-h") | Some("--help") => {
            print_usage();
            return Ok(());
        }
        _ => {}
    }
    let config_path = args.next().map(PathBuf::from);
    let cfg = ServerConfig::load(config_path.as_deref())?;
    validate_runtime_config(&cfg)?;

    let cwd = std::env::current_dir()?;
    let user_layer = session::agent_config::load_user_layer();
    let registry = Arc::new(
        Registry::new(cfg.mink_home.clone(), cfg.model.clone(), cfg.max_running)
            .with_agent_layer(user_layer),
    );
    let reaper_registry = registry.clone();
    let idle_close = std::time::Duration::from_secs(cfg.idle_close_secs);
    let reaper = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            for (id, project) in reaper_registry.idle_session_ids(idle_close) {
                if let Err(error) = reaper_registry.close(&id, Some(&project)).await {
                    eprintln!("[mink-server] idle close {id} failed: {error:#}");
                }
            }
        }
    });
    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let state = Arc::new(api::ApiState {
        registry: registry.clone(),
        cwd: cwd.clone(),
        shutdown: shutdown_tx.clone(),
    });
    // Web UI：默认服务嵌入二进制的前端产物（build.rs 自动构建并 include_str! 嵌入）。
    // MINK_SERVER_DEV_WEB=1 时回退磁盘 ServeDir（web/dist）——前端开发热迭代场景。
    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("web")
        .join("dist");
    let use_embedded =
        std::env::var("MINK_SERVER_DEV_WEB").is_err() && !crate::web_assets::FILES.is_empty();
    let app = if use_embedded {
        use axum::handler::HandlerWithoutStateExt;
        api::router(state).fallback_service(crate::web_assets::embedded_serve.into_service())
    } else {
        api::router(state).fallback_service(ServeDir::new(web_dir))
    };

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = TcpListener::bind(&addr).await?;
    println!(
        "mink-server listening on http://{addr} (home: {})",
        cfg.mink_home.display()
    );
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        // Tell open SSE streams to close before waiting for in-flight requests
        // to drain. Without this, axum waits for every SSE connection to end,
        // but those streams only end after shutdown_all() drops the broadcast
        // senders — which is unreachable until server.await returns.
        let _ = shutdown_tx.send(true);
    });
    let serve_result = server.await;
    reaper.abort();
    let shutdown_result = registry.shutdown_all().await;
    serve_result?;
    shutdown_result
}

fn version_line() -> String {
    let git_hash = env!("MINK_GIT_HASH");
    if git_hash.is_empty() {
        format!("mink-server {}", env!("CARGO_PKG_VERSION"))
    } else {
        format!("mink-server {} ({git_hash})", env!("CARGO_PKG_VERSION"))
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
        .unwrap_or_else(|| "mink-server".to_string());
    println!("{}", version_line());
    println!();
    println!("Usage: {program} [CONFIG_PATH]");
    println!();
    println!("Options:");
    println!("  -V, --version       Show version");
    println!("  -h, --help          Show this help");
    println!();
    println!("Config: mink-server.toml path (optional); falls back to ~/.minkrc and environment.");
    println!("Environment: MINK_HOME, MINK_SERVER_HOST, MINK_SERVER_PORT, MINK_SERVER_MAX_RUNNING");
}
