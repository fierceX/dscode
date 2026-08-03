//! mink-server: web workspace server for remote interactive mink sessions.
//!
//! Single binary: REST + SSE API plus the embedded web UI. Sessions live in
//! the default mink home (`~/.mink/projects/...`) and share `events.jsonl`
//! with the TUI, enabling seamless hand-off between terminal and browser.

use crate::session::config::{validate_runtime_config, ServerConfig};
use crate::session::registry::Registry;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

mod api;
mod bridge;
mod session;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .filter(|a| a != "--help" && a != "-h")
        .map(PathBuf::from);
    let cfg = ServerConfig::load(config_path.as_deref())?;
    validate_runtime_config(&cfg)?;

    let cwd = std::env::current_dir()?;
    let registry = Arc::new(Registry::new(
        cfg.mink_home.clone(),
        cfg.model.clone(),
        cfg.max_running,
    ));
    let state = Arc::new(api::ApiState {
        registry: registry.clone(),
        cwd: cwd.clone(),
    });
    // Web UI：Svelte 构建产物（web/dist/），由 `npm run build` 生成。
    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join("dist");
    let app = api::router(state).fallback_service(ServeDir::new(web_dir));

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = TcpListener::bind(&addr).await?;
    println!(
        "mink-server listening on http://{addr} (home: {})",
        cfg.mink_home.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}
