# mink-router

Flash reasoning-mode router for Mink, ported from
[pi-deepseek-route](https://github.com/hisence999/pi-deepseek-route).

It implements the task-aware routing strategy as an external `LlmBackend`
decorator. For Flash models, **use it directly**; Prefab trajectory mode is
not required and is only kept compatible for experimental validation.

## Features

- Flash-only gate: non-Flash models pass through untouched.
- Task classification: build → react, fix → spec, ambiguous → weak.
- Flash weak persona (`WEAK_FLASH`).
- Near-field routing guidance for weak mode.
- Prefab-aware real-user detection: ignores seeded warm-up messages.
- Persona dedup: does not double-inject when Prefab already provided it.
- Optional first-turn tool narrowing; full tools are restored after the first
  real tool call.

## Usage

```rust,no_run
use std::sync::Arc;
use mink::prelude::{
    AgentOptions, AgentRuntime, OpenAiCompatibleBackend, OpenAiCompatibleOptions,
};
use mink_router::{RouterConfig, RouterLlmBackend};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let inner = Arc::new(OpenAiCompatibleBackend::new(
        OpenAiCompatibleOptions::default(),
    ));
    let router = RouterLlmBackend::new(
        inner,
        RouterConfig::flash_only()
            .with_prefab_aware(true)
            .with_narrow_first_turn_tools(true),
    );

    let options = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_llm_backend(Arc::new(router))
        .with_api_key("sk-...")
        .with_base_url("https://api.deepseek.com/v1")
        .with_model("deepseek-v4-flash");

    let runtime = AgentRuntime::start(options).await?;
    let outcome = runtime.run_turn("修复这个 bug").await?;
    runtime.shutdown().await?;
    Ok(())
}
```

## E2E

Mock e2e (no API key):

```bash
python3 scripts/e2e_router_mock.py
```

Real-key e2e (optional, uses a local forwarding proxy):

```bash
MINK_ROUTER_E2E_REAL=1 DEEPSEEK_API_KEY=sk-... python3 scripts/e2e_router_real.py
```

## License

MIT.
