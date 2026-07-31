# Changelog

## v0.2.0 (2026-07-30)

### 工具选择统一与 Web 工具移除（Breaking）

- **删除** `WebSearch`、`WebFetch`、`Read http(s)://`、URL artifact cache 及全部 Web 配置和提示词。Agent JSONL 协议升至 v2，旧工具策略字段作为未知字段直接拒绝，不兼容 v1。
- **`enabled_tools` 成为唯一工具选择入口**：删除工具级 `disable_*`、sandbox `allow_bash` / `allow_python` / `allow_sub_agent`、`sandbox_python.enable` 等字段。CLI、TOML、Rust API、Agent JSONL 与 Python SDK 共用同一 `enabled_tools` 合同。`PythonSandbox` 必须在该列表中显式列出才进入 surface。
- **Plan 类型化命令**：新增 `PlanDraft` 和独立 `PlanStore`。草稿/确认/清理由类型化命令驱动，状态转换使用同目录原子替换。已确认计划拒绝新的草稿写入。`<current-plan>` 从 immutable prefix 移入逐请求动态 system state，确认后立即生效。PlanConfirm/PlanClear 的压缩请求经过 `TurnCompactor` 同轮守卫，失败不再被静默吞掉。
- **Todo 持久化工作流**：从上下文内 checklist 改为 `todos.json` 持久化。`TodoRead` / `TodoWrite` / `TodoAdvance` 三个独立工具各使用类型化参数和 revision + 稳定 ID 防止 stale write。新增 `atomic_file.rs` 提供同目录原子写入。成功变更后向 conversation 尾部追加增量事件和紧凑 active 投影；恢复或压缩丢失 revision 时自动同步。新增轻量进度守卫（同一 active batch 长时间无转换或模型准备结束时最多提醒一次）。新增 `reconcile_todo_state()` 在每次 LLM 请求前校验 todo revision 一致性。`ToolOutcome` / `ToolRunResult` 扩展 `plan_command`、`state_metadata`、`presentation`、`success`、`result_kind`、`needs_finalization` 等字段。Todo prompt 按 inspect / structure / progress 语义能力组合加载。

### TUI — 结构化 Full / Inline 双模式

- 原单 TUI projection 拆分为共享结构化 transcript reducer + 两种终端 surface：**Full 模式**（交互式全屏应用内 transcript，鼠标滚轮、可逆折叠、卡片点击）和 **Inline 模式**（渐进写入原生 scrollback，scrolling region 避免 viewport 重绘，自动折叠后不可展开）。
- 工具调用/结果按 ID 合并，携带 `ToolResultKind`、成功状态、Plan/Todo presentation 和 artifact 元数据。
- 新增结构化 Plan / Todo / Artifact / SubAgent 详情页，按实际内容宽度折行。新增 `/plan`、`/todos`、`/artifact ID`、`/sub-agent ID` 四个 slash 命令。
- Inline 模式下详情页临时切换 alternate screen，返回时恢复同一个 terminal 对象。
- `ToolResultDisplay` / `ToolCallDisplay` 扩展 `tool_use_id` 和 presentation 字段；`Display` trait 新增 `render_tool_result_presented()` / `render_tool_call_detail()`。`AgentEvent` 扩展 `billing_turn_id`、`usage`、`presentation`、`artifacts`、`plan_presentation` 字段；事件日志序列化同步更新。
- TUI 实时 signal 与 session replay 经过同一个 reducer；Plan/Todo 详情从 `plan.draft` / `plan.md` / `todos.json` 恢复。
- 启用 ratatui scrolling-regions feature。Inline 只提交连续且 sealed 的 transcript。

### 其他变更

- 内置技能 `pre-code-check` 和 `verification` 的 prompt 调整为 provider 无关描述。
- CLI 参数解析增加泛型 TOML 列表值处理。
- Sub-coordinator 允许 `needs_finalization` 跳过已 finalize 的结果。
- `ArtifactManager` 的 `index` 路径改为公有方法；`PlanStore` 增加 `pending_confirm()` 查询。
- 更新架构、使用、设计、TUI 和工具参考文档。

## v0.1.15 (2026-07-25)

### Tool capabilities

- 新增 `ToolCatalog` 和 `ModelToolSurface`，统一解析工具 schema、白名单、禁用开关、approval、agent 角色、文件系统后端和编译 feature 可用性。
- 新增与具体工具名解耦的语义能力模型，支持 provider 优先级、替代 provider、参数级使用范围、硬依赖和受约束的前向求值。
- 工具组合工作流根据解析后的能力集合确定性加载；不可用工具不再出现在模型 schema、工具提示词或组合工作流中。
- `ToolRunner` 在真正执行前再次校验 resolved model tool surface，阻止模型或修复流程调用因角色、后端等原因未暴露的工具。

### Prompt architecture

- 将 system prompt 重构为结构化 `PromptDocument`，分离 core、工具片段、能力工作流、外部 instructions、MISSION 覆盖和 session state。
- 工具说明和组合约束改为按需加载，覆盖 anchored edit、search-then-inspect、Python 路由、专用写入路由、计划、Todo 和 SubAgent。
- 生成阶段校验 prompt 中的工具引用和工作流依赖，避免提示词引用当前 session 不可调用的工具。
- MISSION 使用明确的 section contract：只允许覆盖白名单 core section，runtime 保留 section 必须 fail closed，不保留旧格式兼容分支。

### Recovery and skills

- Signal Recovery 根据 resolved capabilities 渲染当前可用的检查 provider，并将 focused verification 与普通 Bash 执行、安全检查及误用拦截解耦。
- selected skill 正文不再依赖 `Read` 工具；只有解析出 `ResourceRead` 能力时才展示 skill/rule 索引和 `skill://` 子资源提示。
- 集中 approval 判定和 runtime guidance，清理旧 prompt 拼装逻辑及不再使用的 skill helper。

### Documentation and tests

- 更新架构、设计、使用、工具、信号系统和 agent 文档，新增工具能力、提示词解耦与自由组合的正式设计文档。
- 新增工具面组合不变式、能力解析、prompt 引用、MISSION 边界、恢复策略、执行门禁和 skill 资源提示测试。

## v0.1.14 (2026-07-16)

### Context and sessions

- 上下文压缩改为非破坏式投影：`conversation.jsonl` 完整追加保留，`context-state.json` 持久化活跃边界和摘要。
- 压缩统一使用 LLM 摘要，阈值、响应预留、热尾部和摘要输出预算改为显式配置；摘要作为动态消息保持 immutable prefix。
- 新增可开关的摘要输入降噪：保留用户与 assistant 文本，删除 thinking，压缩工具参数和结果并提取错误证据。
- Agent JSONL 和 Python SDK 补齐上下文窗口及压缩参数，并在 runtime 启动时校验参数组合。
- 正常 turn 只缓存活跃历史后缀；压缩提交后裁剪冷历史缓存，恢复时流式解析 JSONL 并只保留活跃消息。
- Provider context overflow 在无可见输出且本轮尚未压缩时，最多执行一次 LLM 压缩和一次重试。
- 新增从 `conversation.jsonl` 生成的有损 `session://current/history` transcript，并支持通过 registered-resource `Grep` 定位；thinking 和完整工具输出仍需读取原始 JSONL 或 artifact。
- SubAgent fork 在 runtime 初始化前克隆完整 session 状态；artifact 从已有索引恢复序号并禁止覆盖继承正文。

### Resources

- `skill://<name>/<relative-path>` 支持读取文件系统技能的子资源文件（如参考文档），路径经过规范化、真实路径检查和遍历防护。

### Tools

- 本地搜索改进：Glob/Grep 在工作区中排除无关目录的行为优化，减少误报和不必要的结果截断。
- `tool_result_max_bytes` 限制在注册资源搜索中也生效，避免大型 resource 内容溢出。

### Refactor

- 移除过度抽象的 infrastructure trait：`SafetyApprover`、`ContextFileProvider`、`RuleProvider`。
- 内联小文件：`agent/signal_mode.rs` → `config.rs`，`runtime/llm.rs` → `runtime/mod.rs`。
- `plan_actions.rs` 保持独立文件。
- 更新文档中 `agent/signal_mode.rs` 的文件路径引用。

## v0.1.13 (2026-07-07)

### Features

- **OpenAI-compatible 请求扩展参数** — 默认 OpenAI-compatible backend 支持 `openai_tool_choice` 和 `[openai_extra_body]`。
  - `openai_tool_choice` 可配置 `auto` / `none` / `required` 或 JSON 对象，用于兼容标准 Chat Completions 工具选择策略。
  - `[openai_extra_body]` 可透传兼容端点的模型参数、采样参数、结构化输出参数和嵌套扩展字段。
  - `openai_extra_body` 不允许覆盖 `model`、`messages`、`stream`、`tools`、`tool_choice`、`max_tokens` 和 `max_completion_tokens`，避免破坏 agent 协议核心字段。

### Rust API

- `AgentOptions` 新增 OpenAI-compatible backend 便捷配置方法：
  - `with_openai_reasoning_effort()`
  - `without_openai_reasoning_effort()`
  - `with_openai_include_usage()`
  - `with_openai_token_param()`
  - `with_openai_tool_choice()`
  - `with_openai_extra_body()`
- `OpenAiCompatibleBackend` 新增 `with_tool_choice()` 和 `with_extra_body()`，用于直接配置内置 backend。

### Fixes

- `tool_choice` 现在只会在本次请求实际包含 `tools` 时发送，避免压缩、禁用工具或纯文本请求携带孤立 `tool_choice` 被兼容端点拒绝。
- OpenAI-compatible SSE reasoning 字段会过滤模型额外输出的 `<think>` / `</think>` 标签，避免私有化部署或兼容端点把 reasoning 闭合标签直接渲染到终端。
- 保持 `OpenAiCompatibleOptions` 公开结构体字段不变，避免破坏外部使用 struct literal 构造 options 的代码。

### Docs

- 更新 `.minkrc.example`、README、`crates/mink-core/README.md`、`docs/USAGE.md`、`docs/DESIGN.md` 和 `docs/ARCHITECTURE.md`，补充 OpenAI-compatible 参数配置、嵌套 extra body 示例和 Rust 嵌入式 builder 用法。

### Tests

- 新增测试覆盖 OpenAI-compatible extra body 合并、reserved key 保护、无工具请求不发送 `tool_choice`、runtime builder 配置转换和 reasoning effort 禁用。

## v0.1.12 (2026-07-01)

### Features

- **可注入 LLM backend** (`mink::runtime::LlmBackend`) — Rust 嵌入式场景可替换默认 OpenAI-compatible HTTP client，接入私有化部署、内网网关、厂商 SDK 或非 HTTP transport。
  - `AgentOptions::with_llm_backend()` 将自定义 backend 注入现有 agent 循环，不复制 turn 执行逻辑。
  - `LlmRequest` 暴露解析后的真实模型名、请求别名、system prompt、messages、tools、timeout、OpenAI-compatible options 和取消状态。
  - `LlmEvent` 保留 text、thinking/reasoning、tool call、stop 和 usage 事件，供自定义 backend 流式返回。
  - `examples/custom_llm_backend.rs` 提供无网络 backend 示例。
- **自定义模型名与别名** — `flash` / `pro` 继续作为 CLI 别名；任意模型名未命中别名时原样传给 backend。`model_aliases` 可覆盖私有模型等级。
- **OpenAI-compatible client 防护增强** — 请求失败保留 attempt count 以便 usage accounting，失败且 provider 未返回 usage 时记录 unreported usage，提交 provider 前清理孤立 tool call。
- **Runtime event / stream 收敛** — runtime streaming 和子代理协调在嵌入式与 CLI 路径中保持 final outcome、状态映射和子代理 usage 传播一致。

### Tools

- **本地搜索对齐 ripgrep 语义** (`tools/search.rs`)
  - `Glob` 改用 ripgrep 的 `ignore::WalkBuilder` 和 override glob 语义，对齐 `rg --files -g <pattern>`，不依赖外部 `rg` 二进制。
  - `Grep` 改用 `grep-regex`、`grep-searcher` 和 `grep-printer`，对齐 `rg -n/-C -g` 风格输出和 regex 行为。
  - 本地搜索无结果时返回空输出，和 rg 保持一致；结果数和字节上限仍会追加明确截断诊断。
- **VFS 搜索边界清理** (`tools/vfs.rs`)
  - 移除 core 内重复的虚拟搜索 helper 实现。
  - 注入的 `ReadOnlyFileSystem` 后端自行实现 `glob` / `grep` 搜索逻辑，并负责遵守 `max_files` / `max_results`。
  - core 保留 VFS trait、请求/结果结构、请求校验、路径规范化、结果格式化和 100KB 输出保护。
  - `examples/redb_vfs.rs` 将后端搜索逻辑移动到示例 adapter 内部。

### Dependencies

- Workspace 保持 Rust edition 2024；升级后的依赖集合要求 Rust 1.94+。
- 升级主要依赖：`reqwest 0.13`、`wasmtime 46`、`wasmtime-wasi 46`、`axum 0.8`、`rustyline 18`、`sha2 0.11`、`similar 3`、`toml 1.1`、`redb 4`。
- 新增 ripgrep 组件依赖：`grep-printer`、`grep-regex`、`grep-searcher`。
- 适配 `reqwest 0.13` feature：`rustls`、`query`、`form`；WASI preview1 import 迁移到 `wasmtime_wasi::p1` / `p2::pipe`。
- 为 `sha2 0.11` digest 输出新增显式 SHA-256 小写十六进制格式化 helper。

### Docs

- 更新 README、`crates/mink-core/README.md`、`docs/USAGE.md`、`docs/DESIGN.md`、`docs/ARCHITECTURE.md` 和 `docs/tools.md`，同步自定义 LLM backend 注入、模型名直传、VFS 后端职责、rg 风格搜索行为、依赖要求和构建用法。

### Tests

- 新增和更新测试覆盖：模型别名解析、自定义 backend 流式/失败行为、runtime SDK 映射、子代理协调、rg 风格 Glob/Grep 行为和 redb VFS 后端职责。
- 已通过 `cargo fmt --check`、`cargo check --workspace --all-targets --all-features`、`cargo test -q`、`cargo test -q --all-features` 和 Python WASI sandbox 慢测。

## v0.1.11 (2026-06-26)

### Features

- **Read-only VFS hooks** (`tools/vfs.rs`) — 可注入的同步只读文件系统后端，替换 Read/Glob/Grep 的本地文件访问
  - `VfsScope` 携带 `resource_session_id` 和 `agent_session_id` 双 scope
  - 虚拟 Read 不生成 snapshot，Edit/Write 始终操作本地文件
  - `redb_vfs.rs` 示例：嵌入式 redb 数据库作为 VFS 后端
- **Registered resource & capability views** (`resources/router.rs`)
  - `ResourceRouter` 统一分发 `artifact://`、`skill://`、`rule://`、`session://` 资源
  - `artifact://<id>` 读取被截断的工具输出全文
  - `session://current` 查看当前 session 摘要、stats、messages、artifacts
- **Capability snapshot 系统** (`capabilities/`) — 统一的 skills/rules/context files 快照
  - SkillProvider trait，支持 runtime/filesystem/built-in 三级 skill 发现
  - `skill_discovery_policy`：Defaults / RuntimeOnly / ExplicitOnly
  - 内联 skill、SDK 协议 `inline_skills` 字段
  - Context files（AGENTS.md / CLAUDE.md）和 Rule 资源读取
- **Session title 自动生成** — 交互/TUI 模式下自动从首条用户输入生成 title
  - 退出时 (`auto_set_session_title`) 从首条用户输入生成 title 并写回 session.json
  - `--list-sessions` 时惰性补全 (`resolve_session_title`)：无 title 时从 `conversation.jsonl` 取首条 user 消息
  - 只写 `title` 字段，其他字段不变
- **Exit 消息显示 alias** — 退出提示优先读取 `session.json` 的 `alias` 字段
- **CJK 对齐修复** — `--list-sessions` 的 ALIAS/TITLE/UPDATED 列用 `unicode-width` 计算显示宽度
  - `unicode-width` 从 TUI 选装升级为非可选依赖
- **Python SDK 增强** (`mink_agent/`) — `inline_skills`、`skill_discovery_policy`、`resource_handlers`、`session_layout` 选项
- **SubAgent VFS scope 继承** — 子代理继承父 session 的 `resource_session_id`

### TUI

- **文件选择器优化** — 扫描深度统一为 1（直接子目录），隐藏文件过滤，评分算法重写
  - 隐藏文件：查询不以 `.` 开头时不显示 `.git/` 等条目
  - 精确匹配 (1200) > 前缀匹配 > basename 包含，目录优先排序
  - `./` 和 `./.` 前缀正确处理隐藏文件发现
  - Enter 确认选择并关闭、Tab 确认选择并保持打开（便于逐级进入目录）

### API Changes

- `AgentOptions` 新增 `with_session_layout`、`with_skill_providers`、`with_resource_handler`、`with_runtime_skill`、`with_skill_discovery_policy`、`with_selected_skills`、`with_first_prompt` 等方法
- `SessionPolicy` 新增 `UseOrCreate` 变体
- `SessionInfo` 新增 `session_ref`、`artifacts_dir`、`summary_path`、`usage_path`、`plan_path`、`plan_draft_path` 字段
- `TurnOutcome` 新增 `session`、`usage`、`usage_records` 字段

### Docs

- 新增 `crates/mink-core/README.md`（库 API 文档）
- `docs/ARCHITECTURE.md` 重写运行时分层、模块索引、执行流程
- `docs/DESIGN.md` 新增 Session、VFS、Capability、SDK Protocol 章节
- `docs/USAGE.md` 更新 session 布局、资源读取、SDK 协议、工具参考
- `docs/tools.md` 更新 Read/Glob/Grep 参数说明、VFS 行为

### Refactor

- `src/skills.rs` → `src/capabilities/` 模块化拆分
- `src/tools/file.rs` 重构：Read path selector 抽取为 `resources/selector.rs`

## v0.1.10 (2026-06-20)

### Features

- **Token 用量与费用统计** (`session/usage.rs`) — 统一的 `UsageJournal`/`MeteredStream` 采集 LLM 请求级 Token 和费用
  - 新增 `billing_turn_id` 生命周期，覆盖 Agent 调用、自动压缩和子代理
  - `TurnOutcome.usage`（`UsageSummary` 汇总）和 `usage_records`（`Vec<UsageRecord>` 明细）对外暴露
  - `SessionInfo.usage_path` 指向 `usage.jsonl`，可自行读取完整历史
  - `price_usage()` 通过 `ModelTier::price_input/output/cache_read_per_m()` 纳元整数运算，消除双重定价
  - `OpenAIParser` 解析 `cache_creation_input_tokens`（兼容直接字段和 `prompt_tokens_details.cache_creation`）
  - `Event::UsageUnavailable` 协议变体，区分 provider 未返回 usage 与 usage=0
  - SubAgent 共享父 session 的 `UsageJournal`，无需额外 rollup

### TUI

- **移除内容区边框与 Border 切换** (`Ctrl+T` 快捷键删除)
  - 改用稳定水平/底部 padding，便于终端文本选中
  - 删除 `theme::border()` / `streaming_border()` 和 `show_borders` 状态
- **模型标签同步** — TUI 启动、模型切换和 turn title 刷新时保持活跃模型标签一致
- **修改 diff 格式化方式** — 引入 `is_diff_eligible()` 工具白名单，仅在 Edit/Bash/Python/PythonSandbox 的输出上按 diff 语法高亮渲染
  - Read 等结构化输出工具排除，避免 YAML front matter 等文件内容误染 diff 颜色
  - `is_diff_like` 改为全文扫描（白名单已确保安全），不再依赖前 5 行启发式

### Editor Workflow

- **Write 工具失效旧 snapshot** — 写入文件后自动 invalidate 对应 snapshot，后续 Edit 用旧 tag 时提示未知 tag
- **Edit 错误上下文** — snapshot 未知、未覆盖或行 hash 不匹配时在错误信息中输出 `Current context:` 锚点行
- **Edit 后返回新 anchor** — 成功的 Edit 返回 `@path#TAG` header 和修改区域附近行号，方便立即跟进
- **`old_string/new_string` 显式拒绝** — 使用旧参数时提示改用 Read+patch 流程

### Session & Compaction

- **Compaction 复用 AsyncLlClient** — `run_summary_call()` 改用 `AsyncLlClient::stream_request()`，移除独立 HTTP client 和 `send_summary_request()` 方法
- **Compaction 采集 usage** — 压缩生成的 LLM 请求也经过 `MeteredStream`，usage 记入同一 `usage.jsonl`
- **移除 `CompactionEngine` 死代码** — 删除 `_client` 参数和 `is_retryable_compaction_status()`

### Docs

- `USAGE.md` 新增 Token 用量与费用章节，覆盖 `UsageSummary`、`TokenUsage`、定价模型和 Rust/Python/CLI 多端代码示例
- `ARCHITECTURE.md` 新增 `session/usage.rs` 模块，数据流增加 MeteredStream 采集和 `OrchActor::finish_usage()` 汇总
- `crates/mink-core/README.md` 补充 `billing_turn_id`/`usage_records` Rust 嵌入示例
- `docs/tools.md` 更新 Edit 工具描述，对齐 Read+patch 流程
- `docs/DESIGN.md` 更新 anchored Edit 设计说明

### Tests

- 446 tests passed (+3 新: cache_creation 解析、+ 回归测试覆盖) — 相比 v0.1.9 的 443 passed

## v0.1.9 (2026-06-14)

### Features

- **Rust 库 API** — 新增 `mink::runtime` 公共模块，暴露 `AgentRuntime`、`AgentEventStream`、`AgentOptions` 等核心类型
  - `run_turn()` / `stream_turn()` / `shutdown()` 完整生命周期
  - 8 个 mock LLM 集成测试、14 种 Display→AgentEvent 覆盖测试
- **Hidden-worker Web API 示例** — `examples/web_api.rs`，axum + 进程沙箱 + 异步任务队列

### SDK & Protocol

- **Session 布局支持** — 引入显式 `SessionLayout`，支持 project-scoped / home-scoped / isolated 三种路径策略
  - CLI 和 bare `mink-core` 保持原 project-scoped 行为
  - Python SDK 默认为 home-scoped
  - Rust `AgentOptions` 默认 isolation session root
- **工具过滤覆盖率提升** — `ToolConfig::filter_tools_json` 单一路径
- **SDK JSONL success 兼容性测试** — `final_from_outcome` 字段契约

### Refactor

- **Workspace 拆分** — 将 Rust runtime 移入可发布的 `mink-core` crate，UI 层移入 `mink-cli` crate
  - 根 manifest 转为 workspace，二进制名和 CLI 行为不变
  - `mink-core` 可通过 crates.io 独立发布（无终端 UI 依赖）
  - `crates/mink-cli` 拥有 `mink` 和 `mink-core` 二进制目标
- **运行时边界精炼** — Runtime 上下文构造在主 runtime 和子代理间共享，单次 CLI 执行走 `AgentRuntime::run_turn`
- **清理过时代码** — 删除 `ensure_workspace_write`（沙箱已处理文件限制）、移除 3 个关联测试、删除遗留辅助代码

### Fixes

- macOS 沙箱写入白名单修复：支持自定义 `MINK_HOME`，无需放通整个 `HOME`
- 沙箱路径规范化修正（sandbox-exec subpath rules）
- Session 恢复/继续时路径解析修复

### Docs

- `AGENTS.md`、`ARCHITECTURE.md`、`DESIGN.md`、`README.md` 更新
- 更新 Python SDK 文档，记录 SessionLayout 语义和包发布说明

### CI & Build

- 新增 workspace 构建矩阵（`make feature-matrix`）
- Python wheel 构建器更新为 `-p mink-cli --bin mink-core`
- 新增 `sdk-bin` feature 控制 SDK 二进制目标
## v0.1.8 (2026-06-12)

### Features

- **搜索参数可配置** — Glob/Grep 最大文件/结果数通过 `max_search_files`/`max_search_results` 参数控制
- **Agent 启动路径优化** — `--agent-jsonl` 跳过 `.minkrc` 文件 I/O；Mission 内容通过 stdin JSONL 直接传递，消除临时文件
- **按需裁剪** — `wasmtime`/`wasmtime-wasi` 通过 `python-sandbox` feature-gate 按需编译，`--no-default-features` 可缩小二进制体积
- **工具白名单 `enabled_tools`** — 按任务类型裁剪 system prompt 中暴露的工具列表

### SDK

- **SDK 二进制拆分** — Python SDK 打包 no-TUI `mink-core` 替代完整 `mink` 二进制，降低分发体积
- **SDK streaming 控制** — 新增 `stream_events`/`verbose` 参数，`AgentStreamEvent` 归一化事件协议，`raw_stream()` 公开为公共 API
- `max_search_files`/`max_search_results` 通过 `SandboxConfig` 暴露

### Refactor

- **工具过滤统一到配置层** — 合并 `filter_disabled_tools` + `filter_enabled_tools` 为 `ToolConfig::filter_tools_json` 单一路径
- **`TOOL_DISABLE_MAP` 从 `prefix.rs` 移到 `config.rs`**
- **PythonSandbox 重构** — CPython WASI 沙箱逻辑重构（`src/tools/sandbox_python.rs`）

### Config

- 新增 `max_search_files`（默认 5000）、`max_search_results`（默认 1000）配置项，支持环境变量覆盖
- `.minkrc.example` 重构，分组对齐 Python SDK 配置风格

### Fixes

- **PythonSandbox** — 修复路径权限、`os.chdir` 注入、WASI 文件系统隔离等问题
- **SDK** — 修复 wheel 构建中二进制路径和包名不一致问题

## v0.1.7 (2026-06-09)

### Features

- **TUI 文件选择器** — 新增 Tab 路径补全、父目录入口和沙箱感知过滤 (`src/tui/file_picker.rs`)
- **TUI 任务完成通知** — 新增任务完成/失败通知链路，兼容 macOS 系统通知，接入用户输入与 compact 流程

### Refactor

- **精简 CLI 参数** — 移除 11 个中低频 CLI 参数，改为 `--config <toml>` 统一传递
  - 涉及参数：max-tokens、max-turns、max-context、tool-timeout 等
  - 对齐 Python SDK 的配置构建方式，统一走 TOML 通道

### CI & Build

- **新增 FreeBSD CI 构建目标** (`x86_64-unknown-freebsd`)
- **修复 FreeBSD CI** 包名和 release 版本

### Tests

- **测试加速** — 标记 25 个 PythonSandbox 重型测试为 `slow-tests` feature gate
  - 日常 `cargo test` 从 ~120 秒降至 ~5 秒
  - CI 环境通过 `--features slow-tests -- --include-ignored` 全量覆盖

## v0.1.6 (2026-06-09)

### Features

- **PythonSandbox 工具** — 新增基于 wasmtime + CPython WASI 的沙箱 Python 执行环境
  - WASI 级进程隔离：无子进程、无网络、无 C 扩展
  - 完整 CPython 标准库（json/csv/re/math/datetime/xml 等）
  - 通过 `--enable-python-sandbox` CLI 参数或 `.minkrc` 的 `[sandbox_python]` 段启用
  - 默认禁用，避免与宿主 Python 工具混用
  - 支持相对路径和绝对路径（自动注入 `os.chdir`）
  - 通过 `read_dirs` / `write_dirs` 精细化控制文件访问权限
  - 25 个边界测试覆盖

- **hashline patch 解析器** — 用 Tokenizer + Executor 两阶段状态机替换旧手写解析器
  - 修复 markdown 表格行（`|`）导致 Edit 崩溃的问题
  - 非 `+` 前缀 body 行接受并给出警告，而非直接报错
  - 空白行在 hunk body 中正确跳过，不终止收集

### Refactor

- **移除 Python 工具字符串过滤** — 删除 `BLOCKED_PATTERNS`
  - 安全策略不再由工具层承担，交给 OS 进程沙箱处理
  - 宿主 Python 现拥有完整生态访问能力（网络、子进程、C 扩展）
- **Edit 工具内部重构** — 用 hashline 解析器替换旧 `parse_anchored_patch`
- **系统提示词结构优化** — 简化 Read 工具 schema 描述
- **Skill 系统重构** — 统一 skill 发现与读取协议

### Config

- 新增 `[sandbox_python]` 配置段（CPython WASI 沙箱路径和权限）
- 新增 `--enable-python-sandbox` CLI 参数
- 新增 `.minkrc.example` 完整配置示例
- 更新 `.minkrc` 统一为 Mink 格式

### SDK & Protocol

- Agent JSONL 协议优化，支持 single-shot 模式
- SubAgent 调用协议优化
- Session 命名与 Read 资源协议优化

### Fixes

- Agent 停止与超时边界问题
- 工具执行稳定性问题
- Web 工具替换为 DuckDuckGo 实现（无需 API Key）

### Tests

- 470 测试通过（0 failed）
- 新增 25 个 PythonSandbox 边界测试（路径权限、读写隔离、路径穿越、Unicode 路径、stdout/stderr 捕获等）

### Dependencies

- 新增 `wasmtime` 28、`wasmtime-wasi` 28

## v0.1.5 (2026-04-22)

### Features

- **工具协议与资源读取优化** — Session 命名、Read 资源协议走 URL 模式
- 离线的 TUI 操作模式
- TUI 支持 UTF-8 光标和软换行
- `print` 模式输出 ndjson 事件流

### CI & Build

- 完善的 Linux wheel 构建流水线
  - manylinux_2_35 glibc 原生 wheel
  - musllinux_1_2 静态编译 wheel
  - Apple Silicon 原生 wheel
  - 修复 musl 构建 segfault（cargo-zigbuild 替代 musl-gcc）
  - 正确设置 wheel tag（PEP 656）
- Ubuntu 22.04 迁移（glibc 2.35 兼容性）

### Fixes

- **沙箱 work_dir 写入权限修复** — sandbox 内 work_dir 写入权限问题
- **bwrap 缺少 --chdir** — 修复沙箱内 cwd 不正确问题
- TUI 退出流程修复（Ctrl+C 行为）
- 会话恢复逻辑修复

### Config

- 新增 `.minkrc` 配置文件和 `[sandbox]` 配置段
- 新增 `--disable-bash`、`--disable-python`、`--disable-sub-agent`、`--disable-web` CLI 参数
- 沙箱后端支持：nsjail（Linux）、bubblewrap（Linux）、sandbox-exec（macOS）
- 沙箱自举 re-exec 到 sandbox-exec / nsjail / bwrap



## v0.1.2 (2026-03-15)

### Features

- **信号驱动的信念系统** — 自动检测工具执行错误（ToolFailed/ToolError/EditLoop）
  - 滑动窗口信念度计算（拉普拉斯平滑）
  - 低信念注入修正提示 + 恢复首步守卫
  - 低于阈值自动中止执行
  - 可通过 `MINK_SIGNAL_MODE=off` 关闭
- **自适应上下文压缩** — 三级 Tier 压缩，自动摘要，保持上下文在窗口内
- **维修流水线** — Scavenge 回收遗漏工具调用 → Truncation 修复 → StormBreaker 重复调用抑制
- **Session 持久化** — JSONL 格式，`--continue` 无缝恢复
- **SubAgent 子代理** — 隔离或 fork 上下文，并发执行
- **Skill 系统** — 按需加载 skill 文件，不污染后续 prompt
- **自定义提示词** — `--mission` 加载 MISSION.md 文件

### Tools

- Read / Write / Edit（anchored patch）
- Bash / Python（受限）
- Glob / Grep / WebSearch / WebFetch
- TodoWrite / PlanConfirm / PlanClear
- 工具元数据与审批策略（ApprovalTier / ToolResultKind）
- Artifact 持久化（超长工具输出落盘）

### TUI

- 基于 ratatui 的全屏界面
- 消息列表 + 输入区 + 状态栏
- Markdown 子集渲染
- 工具结果折叠 / 子代理详情 / 鼠标点击

### Fixes

- **sandbox reexec 移到 JSON-RPC 解析之前** — 修复 session_id 丢失问题
- **PyPI 发布修复** — 分离 wheel 目录与二进制目录
- 修复 CI Python 包发布流程

## v0.1.1 (2026-03-01)

### Features

- **REPL 模式**（rustyline 行编辑 + TerminalDisplay 同步渲染）
- DeepSeek V4 流式请求（SSE → Event → ToolCall）
- 工具调用执行循环（LLM → Tool → Decision）
- Artifact 持久化（session artifacts 读写）
- CLI 参数解析 + 配置合并（.minkrc / 环境变量 / CLI）
- 危险命令过滤（safety.rs）

### SDK

- Python pip 包发布流水线（GitHub Actions CI）
- 跨平台构建支持（Linux x86 + musl + ARM，macOS ARM）
- Python SDK 基础接口

### TUI

- 初始 TUI 原型
- 多行输入 / Ctrl+C 中断
- Slash command（`/flash`、`/pro`、`/compact`、`/help`、`/exit`）

### Fixes

- 修复自适应超时测试竞态条件
- 修复 ARM64 交叉编译 CI

## v0.1.0 (2026-02-15)

### Features

- 初始发布
- 基本 CLI 参数解析 + 配置加载
- LLM 流式请求基础框架
- Read 工具（本地文件 + URL）
- Bash 工具执行
- Session 管理基础
