//! Example: run Mink with the router backend, optionally combined with Prefab.
//!
//! Environment variables:
//! - `MOCK_BASE_URL` (required): base URL of the LLM API/mock server.
//! - `MINK_HOME`: session home (default `./target/router-e2e-home`).
//! - `CWD`: working directory (default `./`).
//! - `SESSION`: session name (default `router-e2e`).
//! - `PROMPT`: user prompt (default `continue`).
//! - `PREFAB`: optional prefab template name/path (e.g. `router-flash-weak`).
//! - `NARROW_TOOLS`: `1` to enable first-turn tool narrowing.

use std::sync::Arc;

use mink::prelude::{
    AgentOptions, AgentRuntime, OpenAiCompatibleBackend, OpenAiCompatibleOptions, SessionPolicy,
};
use mink_router::{RouterConfig, RouterLlmBackend};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_url = std::env::var("MOCK_BASE_URL").unwrap_or_else(|_| {
        eprintln!("MOCK_BASE_URL is required");
        std::process::exit(2);
    });
    let home = std::env::var("MINK_HOME").unwrap_or_else(|_| "target/router-e2e-home".into());
    let cwd = std::env::var("CWD").unwrap_or_else(|_| ".".into());
    let session = std::env::var("SESSION").unwrap_or_else(|_| "router-e2e".into());
    let prompt = std::env::var("PROMPT").unwrap_or_else(|_| "continue".into());
    let narrow = std::env::var("NARROW_TOOLS").as_deref() == Ok("1");

    let inner = Arc::new(OpenAiCompatibleBackend::new(
        OpenAiCompatibleOptions::default(),
    ));
    let router = RouterLlmBackend::new(
        inner,
        RouterConfig::flash_only()
            .with_prefab_aware(true)
            .with_narrow_first_turn_tools(narrow),
    );

    let mut options = AgentOptions::new(home.clone(), cwd.clone())
        .with_llm_backend(Arc::new(router))
        .with_api_key("test-key")
        .with_base_url(&base_url)
        .with_model("deepseek-v4-flash")
        .with_project_scoped_sessions()
        .with_session(SessionPolicy::UseOrCreate(session.clone()));

    if let Ok(prefab) = std::env::var("PREFAB") {
        options = options.with_prefab_spec(&prefab)?;
    }

    let runtime = AgentRuntime::start(options).await?;
    let outcome = runtime.run_turn(&prompt).await?;
    println!("status={:?} text={}", outcome.status, outcome.text);
    runtime.shutdown().await?;
    Ok(())
}
