# Mink Router（Flash 路由外挂）

`mink-router` 将 `pi-deepseek-route` 的 Flash 路由策略移植为 Mink 的
`LlmBackend` 装饰器。对于 Flash 模型，**推荐直接使用 Router，不需要叠加 Prefab 轨迹**；
Router 已包含 Flash persona、近场引导和工具面渐进暴露。

## 设计

- 纯逻辑：`crates/mink-router/src/core.rs`
- Prefab 感知：`crates/mink-router/src/prefab.rs`
- LLM 装饰器：`crates/mink-router/src/backend.rs`
- 配置：`crates/mink-router/src/config.rs`

## CLI / TUI 使用

```bash
# 仅路由（推荐）
mink --router "修复这个 bug"

# TUI
mink --tui --router
```

> 不推荐：`--router --prefab=router-flash-weak` 组合仅用于实验/兼容验证。
> Prefab 预热轨迹会额外占用上下文和 TUI transcript，Router 已覆盖其核心 persona 效果。

`full-cli` 默认包含 `router` feature。

## Rust API 使用

```rust
let inner = Arc::new(OpenAiCompatibleBackend::new(OpenAiCompatibleOptions::default()));
let router = RouterLlmBackend::new(inner, RouterConfig::flash_only().with_prefab_aware(true));

AgentOptions::new(home, cwd)
    .with_llm_backend(Arc::new(router))
    ...
```

> `with_prefab_named("router-flash-weak")` 仅用于实验/兼容验证，不是推荐用法。

## 测试

### 单元测试

```bash
cargo test -p mink-router
```

### Mock e2e（无需密钥）

```bash
python3 scripts/e2e_router_mock.py
```

验证内容：

- Prefab 预热消息不参与路由
- Flash persona 不重复注入
- weak 模式近场引导注入
- 首轮工具面收窄
- 多轮请求捕获与 session 目录分析

### 真实密钥 e2e（可选）

```bash
MINK_ROUTER_E2E_REAL=1 DEEPSEEK_API_KEY=sk-... python3 scripts/e2e_router_real.py
```

通过本地转发代理抓包并转发到真实 DeepSeek API，验证真实模型行为。
