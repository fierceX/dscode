# Changelog

## Unreleased

### 费用统计移除 & TUI 流式渲染修复

- **费用统计移除**：上游模型单价随时段（高峰/空闲）与官方调价变动，硬编码价目表无法准确持续计价；移除 `PricingCatalog`/`UsageCost` 与全部费用输出（TUI 状态栏、REPL 标题栏、Web 前端会话指标/用量面板、首页演示状态栏，以及协议 `final`/`title_update`/`/api/sessions` 中的费用字段）；`UsageSummary` 与 `mink::ui::StatsSnapshot` 不再携带费用，`StatsSnapshot::format_cost()` 一并删除。
- **兼容字段**：`usage.jsonl` 记录的 `cost_nano_cny` 保留——已上报记录恒写 `0`，未上报记录写 `null`（历史记录照常读取，不影响既有 session 文件）。
- **TUI 流式渲染修复**：流式期间到达的 Info 信号（如 LLM 等待心跳 `Waiting for model response...`）不再调用 `finalize_stream()` 把文本拦腰截断——此前会把含未闭合代码围栏的 markdown 切成两段，下半段丢失围栏上下文按段落重新渲染、残留闭合围栏把后续内容吞为原始文本；现在心跳仅以状态栏精简标签（如 `·30s`）瞬时展示，流恢复/结束即清除，不再进入 transcript；流式期间的其它告警推迟到流结束时落盘。心跳文案由 mink-core 统一格式化与解析（`mink::runtime::llm_wait_heartbeat_message` / `parse_llm_wait_heartbeat_elapsed`），展示层不再复制字符串。

## v0.6.0 (2026-09-03)

### 多模态读图支持

新增读图全链路：`Read` 捕获图片 → 内容寻址缓存 → 请求时物化注入，并贯通能力冻结、配额与降级矩阵。

- **能力层**：新增 `SessionModelCapabilities`（`capabilities/model_capabilities.rs`）与
  `ImageInputCapability`（`Unsupported` / `OpenAiChatImageUrl`）——含 detail、
  allowed_mime、wire_protocol、token_estimator 与图片数量/字节/维度/像素限额，以及覆盖
  以上字段的能力指纹。能力在 session 初始化时解析并冻结，持久化到
  `model-capabilities.json`（原子替换）；恢复时重算指纹核对，模型切换经兼容谓词门控：
  Unsupported 会话接受任意模型但永远 text-only，图片会话要求指纹精确匹配（不兼容时
  拒绝启动/切换）。
- **工具层**：`Read` 支持图片捕获——本地文件 magic 嗅探（PNG/JPEG/GIF/WebP）、
  `image://sha256:<hex64>` 引用读取（重新注入）、VFS 逐张读图（字节不跨批累积）；
  图片路径拒绝行 selector / `:raw`；MIME 双层校验（Mink 支持集 ∩ 模型 allowed_mime）、
  单图字节/边长/像素上限，超限 fail closed。能力关闭时行为与旧版逐字节一致
  （`image://` 仍为未知 scheme fail-closed）。
- **缓存层**：`<home>/.mink/cache/images/v1/` 内容寻址不可变对象（原始字节 SHA-256
  命名），两阶段提交（EEXIST 去重 + 目录 fsync + 读回 digest 校验），无索引文件；
  子代理不继承父缓存，fork 后的父引用统一按历史 unavailable 处理。
- **请求投影**：conversation 只存 `tool_attachment` 块（`image://` 引用 + 预算元数据，
  无路径无文件名）；请求时物化为 OpenAI `image_url` data-URL（原字节 base64，无转换）。
  单次消费生命周期：最后一条 assistant 消息之后的引用为未消费批，物化一次后降级为
  确定性文本引用（`Read image://...` 可重新注入，幂等）。每批配额（默认 600 张 /
  16MB 原始字节）从 0 计数、批次间重置、历史引用永不锁死；本轮引用物化失败使请求
  失败（不谎报"已附加"），历史引用降级 `[image unavailable]`。
- **请求预算**：OpenAI 分块 token 估算（长边 2000 / 短边 768 两级缩放；detail=high
  时 85 base + 170/512px tile）；最终请求体硬上限 32MB；压缩器与主请求共用同一投影
  （已消费图片不再计为视觉 token），摘要输入降噪把 `tool_attachment` 置为占位。
- **配置（CLI / `.minkrc`）**：`[provider] image_input = "on"|"off"`（显式覆盖 backend
  声明）、`vision_models = [...]`（替换内置默认 `deepseek-v4-flash-vision-exp`，
  空列表 = 全部关闭）、`[provider.image]` 限额覆盖（detail /
  max_images_per_request / max_image_bytes_per_request / max_image_bytes /
  max_dimension / max_pixels，字节支持 K/M/G 后缀）；环境变量 `MINK_IMAGE_INPUT` /
  `MINK_VISION_MODELS`。示例见 `.minkrc.example`。
- **Rust 库 API（新增，非破坏）**：`AgentOptions::with_image_input()` /
  `with_vision_models()` / `with_image_limits()`；`LlmBackend` 新增
  `image_input_capability(model)` 声明方法（默认 `Unsupported`，fail closed）；
  `RouterLlmBackend` 透传内层 backend 声明；相关类型（`ImageInputCapability`、
  `ImageLimitsOverrides`、`OpenAiChatImageUrlLimits`、`ImageDetail`、`ImageFormat`、
  `TokenEstimator`、`WireProtocol`）自 `mink::runtime` 导出。分辨率优先级：显式
  `image_input` > backend 声明 > `Unsupported`；`image_limits` 只作用于已支持会话，
  不会把文本会话变成视觉会话。
- **测试**：库级捕获/引用/配额/VFS/资源排除用例（`tools/runner_tests.rs` 等）、
  `crates/mink-core/tests/image_e2e.rs` 端到端、router 透传测试。

### 缓存命中与压缩/Plan 协议

压缩器让 provider 缓存命中可见，并把 Plan 状态变更收敛为可重放协议：

- auto 压缩使用最近有效 provider prompt usage 校准压力，preflight 保持保守本地估算；
  新增 pressure source、baseline 和 projection generation 事件字段。
- `LlmBackend` 新增可选 cache projection seam；OpenAI-compatible 与 Router backend
  支持摘要请求复用主请求的 system/tools 和历史公共缓存前缀，不支持时自动降级。
- 压缩摘要改为 internal user `<compacted-summary>` checkpoint，旧 session 恢复时无需迁移。
- PlanConfirm/PlanClear 在成功工具结果后追加 transition，不再强制压缩；历史压缩后活动计划
  通过 `<active-plan-checkpoint>` 投影。Plan 文件变更与 conversation 追加由
  `plan-transaction.json` 可重放 journal 协调：未绑定的中断操作回滚，已绑定操作在恢复时
  幂等补齐 tool result / transition，避免 `plan.md` 与模型历史永久分叉。
- 删除 `plan_projection_tail` 配置；旧 TOML 字段按未知字段策略拒绝。

## v0.5.0 (2026-08-18)

### 信号系统：分层响应模型（Breaking）

信号系统从"命令式恢复指令"重构为"轨迹事实注入 + 分级响应"：

- **轨迹证据层**（新增 `guard/evidence.rs`）：EvidenceTracker 聚合重复调用、失败聚类与
  编辑路径统计，注入 `[trajectory]` / `[detector]` **事实帧**（替代旧
  "Enter SIGNAL_RECOVERY" 祈使句）；证据去重哈希只覆盖事实文本
  （`evidence_dedup_window` 控制窗口），同一证据批不重复注入，响应事件携带证据文本
  （可回溯 conversation.jsonl）。
- **信念跨轮衰减**：每用户输入按 `decay_per_input`（默认 0.6）衰减替代硬重置——跨轮
  重复失败累积升级、偶然失败自然消退；`DecisionEngine` 冷却与 StormBreaker 仍每输入
  reset。
- **决策门控**：单次软失败（ToolError/EditLoop/ArgumentError）且信念 ≥ warn_threshold
  时记录但不干预；累计 ≥ 2 次软失败或出现硬信号（ToolFailed/SafetyBlocked/
  CompileError/TestFailure 与结构化错误码）才参与决策。
- **五级响应**：Reminder（仅证据注入）→ Warning（证据注入 + 快照回滚 + 恢复首步守卫）
  → guard_max_blocks 绕过守卫强制注入 → Restart（策略重启子代理）→ Abort（用户接管：
  `signal_handover` 事件 + Failed）。回滚只作用于循环窗口内被编辑路径，回滚目标是
  最后一次 Read/Write 完整内容基线，写回经原子替换。
- **策略枚举**：`SignalMode{Off,Full}` → `SignalPolicy{Off,Evidence,StateOps,Restart,Full}`；
  `MINK_SIGNAL_MODE` → `MINK_SIGNAL_POLICY`。阈值/超参（remind 0.70 / warn 0.50 /
  abort 0.30 / window 16 / seq 6 / decay 0.6 / cooldown 3 / guard_max 3）内部化为
  `Config.signal` 常量，不再对外配置。
- **中断语义修正**：Interrupted 不再作为 ToolFailed 喂信念、不再触发回滚；中断不泄漏
  并发 permit。
- **StormBreaker 修复**：mutating 调用不再清空整个窗口（旧实现每次写入/编辑先
  `window.clear()` 再入队，使断路器对最易死循环的写操作永久失效）；抑制时机改为
  threshold+1 次（第 4 次才抑制，保留前 N 次放行）。
- **守卫反馈**：拦截批次全量生成 Blocked 结果反馈模型并喂信念（旧实现只反馈首调用）；
  连续拦截达 guard_max_blocks 必须绕过守卫并强制注入。
- `MINK_SIGNAL_POLICY=off` 完整旁路：不生成 belief-awareness 段、不采集/注入/回滚/接管。

### 配置分组化与公共 API 收紧（Breaking）

- `Config` 从 mink-core 迁至 mink-cli 并分组化：`[provider]` / `[generation]` /
  `[context]` / `[tools]` / `[tools.edit]` / `[signal]` / `[sandbox]`；扁平键
  `deny_unknown_fields` 拒绝，旧配置文件直接不可用。
- `CliOverrides` 18 → 10 收敛（删除无生产者的死标记与 24 个 `cli_X = overrides ||
  != default` 启发式）；`--print` / `--agent-jsonl` 显式置 `output_format` override，
  修复配置文件反向压过 `--print` 的问题。
- env 优先级修正：4 个 `MINK_EDIT_*` 变量从"高于文件"翻转为"低于文件"
  （CLI > `--config` > 项目 `.minkrc` > 用户 `~/.minkrc` > env > 默认）。
- `max_turns ≤ 0` 启动即失败（v0.4.0 负值经 `as usize` 回绕为无上限 turn 循环）；
  压缩预算组合校验 fail-fast（`compact_max_output + tail + reserve ≤ max_context`）。
- Rust 公共 API 删除：`mink::config::Config`、`start_with_options()`、
  `try_stream_turn()`、同步 `EventSink`、`command_sender()` / `cancel_token()` /
  `interrupt_flag()`、`with_config()` / `config()` / `config_mut()` /
  `with_tool_approval(_policy)` / `with_skills()` / `with_default_tools()` /
  `with_display()` / `into_runtime_config()` / `TryFrom` 等（迁移文档见下文）。
- 新增分组策略 setter：`with_provider_options` / `with_generation_options` /
  `with_context_policy` / `with_tool_options` / `with_signal_policy` /
  `with_model_alias` / `with_interactive` / `with_tool` / `with_tools`；setter 不再
  静默 clamp，非法值启动 fail-fast。
- 模块面收敛：仅公开 `mink::{runtime, prelude, sdk_protocol, ui}`；`SandboxConfig` /
  `reexec_in_sandbox` 移至 `mink::runtime`；`public_api_boundary` 测试强制。
- SDK 协议 v2 → v3：`SdkOptions` 七组分组、`version` 必须 3、扁平 options 拒绝；
  Python SDK 同步（`_build_request` v3 + `--config` 分组 TOML）；模型名白名单移除
  （任意非空透传，与 ModelResolver 一致）。
- `UsageSummary.cost_nano_cny: u64` → `cost: UsageCost{known_nano_cny,
  unpriced_requests}`；`UsageRecord.cost_nano_cny` 改 `Option<u64>`；未定价模型计
  `unpriced_requests` 而非 0 成本。

### 会话、事件日志与持久化

- `events.jsonl` 统一类型化 EventLog（10 → 30+ 变体），每 session 后台写线程
  （有界 1024 队列，满则阻塞背压）；`jsonl.rs` 共享尾部修复助手。
- **`prefix_snapshot` 事件**：前缀构建/失效重建时落日志
  （fingerprint / dependency_fingerprint / system_prompt / tools_json），任意请求模型
  的前缀可离线重建；缓存命中不重复写（invariant 测试钉住）。
- project key 改为可读前缀(48) + `--` + SHA-256 前 8 字节；旧 key 目录双读兼容、
  新目录写入；同身份跨目录歧义 fail-closed。
- `usage.jsonl` 半截尾修复；metadata/stats 损坏 fail-closed（不再静默重建）；
  Windows 原子替换改用 `MoveFileExW`；stats 脏标记竞态修复（record 与 flush 时序）。
- compaction 重构：`new()` fail-closed 加载、启动投影修复、commit 走原子替换、
  `flush_projection()` 兜底、摘要请求接入共享 interrupt（20ms 轮询 + commit 前复查）、
  cut 不变式 debug_assert 化。
- **events.jsonl 字段级破坏**（严格解析的第三方受影响，仓库内 reader 全 loose 解析）：
  `signal_recovery_guard` 新增必填 `guard_blocks`；`tool_result` 的 `success` 删除改
  `status` 对象；`TurnFinal.usage` 形状变化。

### 计划、提示词与 REPL/TUI

- **计划投影默认尾置**：`<current-plan>` 作为最后一条 system message 投影
  （`plan_projection_tail=false` 可回退前置），计划修订不失效前缀缓存。
- 提示词文本层优化：删除 -574 行死机制（`PromptFact::Workflow` / tag / 依赖环从未被
  使用），除 belief-awareness 一节外逐段字节等价；caps 占位符注入先于
  `surface_fingerprint`；`prompt_discipline` 机械检查（bullet 数/词数/禁词/占位符/
  示例置尾）护航。
- REPL SIGINT 双按退出修复；标题栏信念度同步；`--session` 值收紧（裸用 / `-` 开头拒绝）。

### Prefab 会话重组（新，临时功能）

- 新增独立 `mink-prefab` crate：独立 seeder（模板加载/校验/会话写入）与可选 `mink-integration`
  适配层（`adapter::PrefabPrefixSource` / `PrefabRestructureHook` / `install_template`）；
  会话重组写入模板会话 + 标准 `prefix_snapshot` 事件，系统提示词从 session 事件重建缓存
  前缀。CLI `--prefab[=TEMPLATE]` 经适配层接线；core 提供中立扩展点 `PrefixSource` /
  `PostInitHook`（`runtime/extensions.rs`），本身零 mink-prefab 依赖。
- 重组只允许写入全新 conversation；已有会话不重写模板，缺 `prefix_snapshot` 事件时
  只补写标准前缀事件；子代理继承父前缀源。（临时功能：后续 DeepSeek 更新模型
  后可能撤销。）

### mink-router：Flash 推理模式路由（新）

- 新增独立 `mink-router` crate：将 pi-deepseek-route 的 Flash 路由策略移植为
  `LlmBackend` 装饰器（`RouterLlmBackend`），纯逻辑与 Prefab 感知 helper 分离，
  与 mink-core agent 循环解耦。
- 任务分类（build/fix/chat/complex）、四档 persona（spec / mixed / react / weak）、
  weak 模式近场引导注入、首轮工具面收窄（`narrow_first_turn_tools`）、Prefab 预热
  消息感知；仅路由 `LlmPurpose::Agent` 请求，压缩/子代理透传。
- CLI/TUI 集成：`--router[=flash]`，`router` feature（`full-cli` 默认包含）；
  非 Flash 模型自动透传。22 个单元测试 + mock e2e 捕获脚本
  （`scripts/e2e_router_mock.py`）。

### 运行时缺陷修复群（约 15 个 fix 提交）

- **7 项 P0**：Bash/Python 超时误报成功、输出读取线程挂起、恢复守卫反馈、execute_all
  失败合成结果落库、429 重试不可取消、REPL SIGINT 双按退出、replan 会话 id 碰撞。
- Bash/Python 显式 timeout 上限 fail-closed（600s/300s），模型不可再传任意大 timeout
  挂住进程；Bash 超时结果语义反转（v0.4.0 报 success=true 为缺陷，现报失败 124/None）。
- 进程监督收敛："超时不设 exit_code → `unwrap_or(0)==0` 误判成功"修复；124/130 退出码
  区分；1s 有界 join 防孙进程挂起。PythonSandbox 接入 wasmtime epoch interruption
  真停执行线程（旧 detached 线程超时后仍继续写盘）。
- LLM/SSE：reqwest client 缓存复用；重试全面可取消 + 429 retry-after 上限 10s；
  EOF 残留末帧送 parser 再 finish；DeepSeek 原生 `prompt_cache_hit_tokens` 拼写兜底。
- 文件子系统：raw 读取不再生成可编辑 snapshot；memo 端行语义/UTF-8 边界/CRLF 判定
  修正；行选择器 `N+K` 饱和、offset 超总行数报错（不回读幻影行号）；session id 后缀
  u16 → u32 修并发碰撞。
- 持久化：`usage.jsonl` 半截尾修复；alias 消毒；stats flush 时序修复；execute_all
  失败路径合成结果落库。

### mink-server / Web

- graceful shutdown、session listing、SDK options、tool baseline 处理修复；SSE 断线
  重连加固；stats flush 与 alias 消毒。
- SSE 协议自 v0.4.0 起为 core `AgentEvent` envelope 全量透传（`turn_started` /
  `turn_final` 语义分工、`stream_sequence`、30s 心跳、`stream_gap` 对账）；
  `turn_error` 字段名为 `error`（文档已同步）。

### 测试与稳定性

- mink-core **663 passed**（v0.4.0 为 633，+30）；mink-cli 147、mink-router 22、
  server/Web 用例全绿；`cargo fmt --check`、`cargo clippy --all-targets` 零警告。
- 新增 invariant 测试：请求重建（prefix_snapshot 缓存命中不重复写）、展示透传
  （ToolCallDisplay / PresentedToolResultDisplay 结构化字段完整转发）。
- 全量测试套件稳健性修复；prompt_discipline 机械检查纳入常规测试。

### 迁移（Breaking 汇总）

- `MINK_SIGNAL_MODE`（off/full）→ `MINK_SIGNAL_POLICY`（off/evidence/state_ops/
  restart/full）；旧值静默失效。
- `.minkrc` / `--config` 扁平键全部拒绝，必须按 `[provider]` / `[generation]` /
  `[context]` / `[tools]` / `[tools.edit]` / `[signal]` 分组。
- agent-jsonl v2 → v3：扁平 options 被拒、`version` 必须 3、options 七组；Python
  `extra_options` 必须分组形状。
- Rust：`start_with_options()` → `start()`；`try_stream_turn()` → 返回 Result 的
  `stream_turn()`；`AgentEvent` 通过 `ev.kind` 匹配 `AgentEventKind`；同步 `EventSink`
  迁移到异步 `on_event`；`mink::config::Config` 移除；信号阈值不再可配置。
- events.jsonl：`guard_blocks` 必填、`success` → `status`、usage 形状变化（见上）。
- StormBreaker 抑制时机第 3 → 4 次；Bash 显式超时语义反转；`--session` 裸用 /
  `-` 开头拒绝；4 个 `MINK_EDIT_*` env 优先级翻转。

## v0.4.0 (2026-08-13)

### Runtime 所有权与统一事件（Breaking）

- `AgentRuntime::start(AgentOptions)` 成为唯一公开构建入口：`AgentRuntimeConfig` 与
  `build_runtime()` 降为 crate-private，`AgentOptions` 独占构建路径（含 LLM backend、
  只读 VFS、resource session scope 与自定义工具注入）。
- 每轮使用独立 `TurnId`；`AgentEvent` 改为 `{turn_id, sequence, kind}` 有序 envelope，
  变体收敛到 `AgentEventKind`；`TurnOutcome` 完整聚合 turn_id、billing、status、text、
  thinking、工具调用/错误计数与 usage 明细。
- `run_turn()` / `stream_turn()` 共用非阻塞 turn permit：runtime 忙时立即返回
  `RuntimeError::Busy { active_turn_id }`；stream 的 Drop 与显式 `cancel()` 都等待核心
  清理后才释放 permit，取消不再泄漏并发槽位。
- 新增可克隆 `AgentRuntimeHandle`：`run_turn` / `stream_turn` / `compact` / `set_model` /
  `interrupt_current_turn` 共享同一 Busy 门禁；只有原始 `AgentRuntime` 拥有 `shutdown()`。
  `compact()` 返回 `CompactOutcome`（Compacted/Skipped），`set_model()` 返回 typed error。
- 全局观察者改为异步 `EventSink`（1024 容量 dispatcher 隔离慢 observer）：溢出或 observer
  失败只停止 observer，不中断 turn，错误在 `shutdown()` 返回；shutdown 执行 5s 宽限、
  orchestrator abort，并 flush usage/stats/compaction projection。
- 移除公开的 raw actor sender、cancel token 与 interrupt flag，控制面收敛到 handle。

### AgentTool 自定义工具（稳定异步 API）

- 新增稳定异步 `AgentTool` 接口：`ToolDefinition`（approval tier、result kind、并行只读/
  串行执行模式、mutating、discoverable、storm_exempt、activation、硬依赖与语义能力 offer）
  + `ToolExecutionContext`（cwd + interrupt 查询）+ `ToolOutput` / `ToolError`；
  `AgentOptions::with_tool()` / `with_tools()` 注册。
- 自定义工具与 builtin 共用 catalog 校验（`validate_custom_tools`）、ModelToolSurface
  解析、approval、执行顺序（只读并行/写入串行）、artifact spill 与 mutation memo 失效语义。
- 自定义工具复用全局 tool timeout，executor / timeout / interrupt 三路竞争，非合作式工具
  也能被局部终止。

### Session 持久化与恢复加固

- project key 改为可读前缀 + canonical cwd SHA-256 短哈希：旧 key 目录双读兼容、新 key
  目录写入；同一 session 身份跨目录出现时 fail-closed 报歧义。
- session metadata fail-closed：损坏 metadata 直接报错拒绝启动；已有数据但缺 metadata
  的目录报错（旧 key 遗留目录除外）。
- 原子替换跨平台化：Windows 使用 `MoveFileExW(MOVEFILE_REPLACE_EXISTING |
  MOVEFILE_WRITE_THROUGH)` + 父目录 sync；stats/todos/plan/compaction state/summary
  统一走 `atomic_file::atomic_replace`。
- Compaction 启动校验 `active_start` 不拆分 tool call/result 协议；加载时自动修复 summary
  投影，shutdown 时 `flush_projection()` 兜底落盘；摘要请求接入共享 interrupt
  （linked cancellation 覆盖请求建立与流消费），中断后释放 turn gate，后续 turn 可继续。
- Artifact index 尾部修复 + `sync_all` + 写锁；conversation/events/summary 初始化错误
  传播而非静默忽略。

### mink-server 生命周期与跨进程一致性

- SessionRegistry 引入 typed `RegistryError`：NotFound→404、Ambiguous/Locked/Busy→409、
  Capacity→429、Internal→500；`(project_key, id)` locator 区分同名会话。
- create/open/close/delete 并发语义：operation lock + create lock（锁前/锁后幂等检查，
  不等待锁、不启动临时 runtime、不改写已有 metadata）；delete 先获取并持有系统文件锁，
  阻止其他 Registry 或进程删除使用中的会话。
- session lease 改为 fs2 advisory file lock：锁文件永久保留，独占打开的文件句柄持有 lease；
  移除 PID 存活与陈旧锁启发式判断，跨进程状态一致。
- SessionRuntime 显式阶段机（Idle/Running/Cancelling/Closing/Closed）；forced terminal
  统一登记并发布——外部 shutdown 竞争下 Closed 先于 timeout stop/final 可见且最多发布一次。
- graceful shutdown：ctrl_c → server 停止 → reaper abort → `shutdown_all()`；idle reaper
  按 `idle_close_secs` 定期关闭闲置会话。
- API project-aware：session 路由接受 `?project=` 消歧；history/conversation limit 校验
  1..=2000；SSE 广播通道 1024、30s `: ping` 心跳、`stream_gap {missed}` 断线对账；
  SSE 事件增加 `stream_sequence` 传输序号，`turn_final` 携带完整 `outcome`。

### Web 权威恢复

- reducer 事件语义更新：`turn_started` / `turn_final` / `stop` 职责分离（stop 记录结束原因，
  final 持有权威运行态并携带 outcome error）；`title_update` 改用 `stats` 快照；
  实时 tool_result 强制完整字段（tool_use_id/presentation/artifacts/success/exit_code/
  result_kind）；`(turn_id, sequence)` 去重窗口 4096；transcript key 区分 `seq`（历史）
  与 `live:N`（实时）。
- 权威恢复改为 recovery revision 驱动的单 worker：断线、stream_gap、失败终态与生命周期
  变化使代次失效；恢复请求带 AbortSignal + 15s 超时；会话切换/断线/代次失效取消陈旧请求；
  worker 实例身份保护清理逻辑，旧会话不能覆盖新会话 worker；稳定 Idle 后才解除 desynced。

### PythonSandbox 取消

- 接入 Wasmtime epoch interruption：`epoch_interruption(true)` + `set_epoch_deadline(1)`，
  超时/取消时 `increment_epoch()` 中断执行并 join 执行线程；退出码 124（timeout）/
  130（user cancel）区分原因；epoch/interrupt trap 映射为可读错误。

### CLI / TUI

- CLI 命令统一走 runtime broker 串行队列（Run/Compact/SetModel 排队执行、Interrupt 旁路
  直发），模型切换等待 ack 后更新状态，TUI 错误恢复不再绕过 runtime 门禁。

### CI 与构建

- 发布矩阵统一 cargo-zigbuild 交叉编译：Linux GNU/musl、macOS ARM 与 FreeBSD 均由
  Ubuntu runner 产出（FreeBSD 用 Zig sysroot，macOS 保留 macOS runner 提供 Apple SDK），
  移除 FreeBSD VM 独立分支；统一 Python/cargo-zigbuild/Node/Web 依赖安装，保留
  mink-server 内嵌前端体积检查；修复 Rust 1.97 `-D warnings` 下 collapsible_if Clippy 错误。
- Node 20.18.1 → 20.19.0，满足 Rolldown 1.2.x 原生绑定 engines 要求，避免 npm 静默跳过
  可选依赖导致 build.rs 调 Vite 失败。

### 测试

- mink-core **624 passed** / 5 ignored（+28：runtime gate/handle、observer dispatcher、
  custom tool surface、session 旧/新目录双读与歧义、compaction 启动校验与中断、
  atomic replace 故障注入、artifact index 修复、sandbox epoch 取消等）；mink-server 14、
  Web vitest（reducer/sessionController/sse）与 E2E 14 全部通过。
- 新增共享 protocol fixture `protocol-fixtures/agent-events.json`：mink-core 测试反序列化
  round-trip，保证 server SSE 事件与 core `AgentEvent` 协议一致。

迁移：`start_with_options()` → `start()`；`try_stream_turn()` → 返回 `Result` 的
`stream_turn()`；`AgentEvent` 改为通过 `ev.kind` 匹配 `AgentEventKind`；
`AgentRuntimeConfig` / `build_runtime()` 不再公开；`command_sender()` / `cancel_token()` /
`interrupt_flag()` 由 `handle()` / `interrupt_current_turn()` / `shutdown()` 取代；
同步 `EventSink` 迁移到异步 `on_event`。

## v0.3.3 (2026-08-06)

### hashline 文本锚点范围定位与编辑协议容错

**新机制：文本锚点范围定位（`'start'..'end'`）**
- `PUT`/`CUT` 的范围端点可以是行文本锚点（`PUT 'fn foo('..'}':`、`CUT 'start'..'end':`），
  按 trim 后精确匹配文件行并在 apply 时解析为行号——范围边界由行文本唯一匹配决定，
  消除行号协议固有的 ±1 行/丢括号**静默**错误：锚点 0 匹配（要求重读并复制行文本）与
  多匹配（要求使用唯一长行）都是可见可诊断错误，整个 Edit fail-closed、文件原封不动。
- 与既有机制完全复用：幂等（`already_applied`）、stale 恢复（锚点不参与行号平移）、
  结构保护（delimiter delta）、batch 预检、`@register` 剪贴板与 `MV` 共存。
- 协议引导：hashline 工作流资产 critical/正文/anti-patterns/示例同步（含唯一性约束、
  "行号不确定时用锚点"、示例置尾）；Edit 工具描述补充锚点说明。

**行为修复**
- **缺空格归一化**：`PUT18.=18:` / `PUT>40:` / `CUT5.=8` 按带空格解析并输出 warning
  （仅数字/`<`/`>`/`:` 开头归一化，普通单词不误判）。
- **锚点文本中的 `*` 不再误拒绝**：移除 Block locator 专门拦截——`N*` 本就不是合法
  语法，由行号解析自然拒绝（invalid range / expected a positive line number）；
  引号内锚点文本（如 `exprs = ["3 + 4 * 2"]`）按普通字符解析。
- **Read/Edit 工具描述强化**：Read 明确"只接受 path 参数、行范围写进路径选择器、
  禁止发明其他参数名"；Edit 补充常见错误反例（缺空格可解析、范围必须 start<=end
  且不越界、unified-diff/apply_patch 语法永不合法）。
- **TodoAdvance 容错**：直接 complete pending 条目自动先激活再完成（结果在
  activated 中体现）；重复 complete 已完成的条目仍 fail-closed。

**测试与稳定性**
- mink-server API 测试隔离：`test_router` 每次调用使用唯一临时 home（进程内并行
  测试共享固定目录导致的 artifact 竞态失败消除）。
- `cargo test -p mink-core` **596 passed** / 5 ignored（+12 用例：锚点定位
  CUT/PUT/单行/trim/双引号+register/0 匹配/多匹配/倒置/幂等、缺空格归一化、
  锚点含 `*`、行号 `5*` 自然拒绝、todo 自动激活与重复 complete 拒绝）；
  server 8、clippy 0 警告。


## v0.3.2 (2026-08-06)

### 工具可靠性修复（基于 640 份生产会话轨迹分析）

本版依据收集的 640 份 `conversation.jsonl`（38,298 次工具调用）的
失败模式统计，修复工具契约、读取缓存、编辑幂等、输出协议、压缩边界与提示词引导六类问题。

**Breaking：`Read` 参数契约收窄为单参数**
- 轨迹中 Read 参数名瞎猜（`selector`/`path_sel`/`offset` 混用）出现 178 次，其中 63 次落在同一份 451KB 计算书上。`Read` 现只接受 `path`，删除模型可见的 `offset`/`limit` 字段及旧参数 fallback 合并；行范围一律通过路径选择器（`path:1-200`、`path:10+5`、`path:raw`）表达。
- `assets/tools.json` 全部 15 个工具 schema 增加 `additionalProperties:false`，杜绝未知字段静默接受；新增"schema 声明字段 == serde 接受字段"全工具一致性测试（`catalog.rs`）。
- 空 path selector（`:45-50`）报错并提示正确写法；file-not-found 错误附带 `Glob(pattern)` 建议；>100KB 整读不再直接报错，改为返回头尾预览 + 字节/行数 + selector 示例（`(file too large: N bytes / M lines; showing first 200 lines)`），并保留 `path:start-end` 范围读路径。

**新机制：Read memo（会话级读取缓存）**
- 轨迹中同一文件单会话内重复全量读达 1,881 次（如单份报告整读 15 次）。新增 `tools/read_memo.rs`：以 `(path, len, mtime, 行范围, 压缩纪元, 变更纪元)` 为键的 LRU 缓存（容量 256）；命中时返回行为化短响应（"unchanged, no edits since. Reuse that content."），不再重复输出全文。
- 三路失效守卫：`CompactionEngine::commit_state` 提交成功后递增压缩纪元（压缩后上下文不再含旧内容，强制重读）；Write/Edit 成功后递增变更纪元；子代理各自持有独立 memo。仅本地文件参与缓存，resource/VFS 读取不缓存。
- 记忆 quick-pass 提示词片段：在既有会话/历史中先做快速检索（摘要 → 索引 → 按需展开），避免凭印象重做或重复推导。

**Edit no-change 幂等与 stale 恢复增强**
- 轨迹中 no-change 修补循环 60 次（单次会话约 20 次连续调用）。hashline 模式下，当补丁为单 section 且目标范围当前内容已与补丁结果逐行一致（位置精确校验，`hashline::already_applied`）时，返回幂等成功 `already applied (idempotent)` + 当前 snapshot tag；歧义情形保持 fail-closed：soft no-op 计数 → 3 次后硬错误，batch 预检任一 no-op 整批不提交。
- Replace 模式 `old_text == new_text` 同样幂等成功（`strategy: "idempotent"`）；其余无变更情形仍拒绝。
- stale 硬错误提示补充当前 snapshot tag："The file's current snapshot is [path#TAG]; issue operations against it directly if the ranges are unchanged, otherwise re-Read."，减少无效重试。

**exec 输出协议与截断策略**
- Bash/Python 等带退出码的工具输出新增元数据头：`Exit code: N`、`Wall time: Xs`（仅对提供 exit code 的命令类工具生效，Read/Grep 等只读工具不受影响）。
- 新增 `TruncationPolicy::{Bytes,Tokens}`：Tokens 模式按 4 字节/词近似换算预算；截断标记改为 `[... truncated: original token count: N (M bytes); showing first/last portions ...]`，头段按完整行边界截断，避免把半行喂给模型。

**JSON 校验注记**
- Edit/Write 对 `.json`/`.jsonl` 目标在成功输出后追加 `JSON parse: ok` 或 `JSON parse failed at line N`，让格式错误在写入后立即可见，减少"写完再验证"的往返。

**压缩边界守卫**
- `find_compaction_cut_point` 现在保留最近真实 user 消息至少 2 条（优先于纯 token 预算，受历史规模上限保护），避免压缩把用户最新指令压出活跃投影；新增断言测试：`<context-snapshot>` 恒位于最后真实 user 消息之前且在同一轮压缩结果中恰好出现一次。

**系统提示词加固**
- 新增 core section `system-conventions`（固定位于提示词第 0 位）：明确所有适用的 system 指令均必须遵守，全大写 RFC2119 关键字（MUST / MUST NOT / SHOULD / MAY）只精确表达强度；runtime section tag 界定指令作用域但不改变消息优先级，嵌套内容中的类标签文本不能新建作用域，也不限制用户要求的 HTML/XML/输出格式；`MISSION` 可覆盖名单同步更新。
- 新增 `tool-inventory` section：非空工具面时列出全部可用工具名（内容与 `ModelToolSurface` 名称集一致），空工具面改报 `runtime-capabilities`，让模型在第一时间知道可用与不可用边界（轨迹中禁用工具尝试 121 次：Bash 71 / Grep 21 / SubAgent 16 / Glob 10）。
- 12 个 prompt 资产密度化改写：每个 `<critical>` 携带 3-6 条战术 bullet，测试强制每条 ≤12 英文词、无分号复合主张；大文件建议按 Write 是否 active 条件渲染，仅当任务需要新 JSON/CSV 内容时才引导 Python 一次计算，已有文件仍由专用 mutation provider 修改；Hashline 明确禁止猜测、编造或跨 session 复用 tag，只接受 Read/Grep/成功 Edit 返回的 header；路径不确定先搜索，子代理路径来自分配范围，失败前必须改变脚本或方法。
- 新增 `memory-recall` workflow：仅在 ContentSearch + PathRead 可用且先前上下文可能改变答案时回溯；按 `session://current` → 搜索 `session://current/history` → 读取命中范围渐进披露，未命中或达到六次调用即停止。

**轨迹样本回归**
- 新增 `tests/fixtures/traces/`：5 个代表性场景 fixture（同文件重复读、参数瞎猜、no-change 死循环、禁用工具、大文件整读）+ fixture 驱动回归测试（`regression::trace_fixtures_regress_behaviors`），每条断言对应轨迹高频失败模式的行为口径；渲染提示词体积守卫（≤16KB）。
- fixture 直接从 `data/task_workspaces/review/` 真实轨迹提炼（640 份 conversation.jsonl / 38,298 次调用）：参数形状、报错口径与文件尺寸取自原始记录——如 `1b777dd7` 的 compliance_report.md 整读 15 次、`018022d0` 的 `path_selector` 瞎猜（全库 178 次）、`376dbfb8` 的 no-change 循环 6+ 次（全库 60 次）、`ls -la` 禁用报错（全库 356 次）、`0b3d46c0` 的 154707 字节方案.md 整读报错（全库 89 次）；断言全部对应修复后行为（memo 命中 / unknown field 指明字段 / 幂等成功 / blocked / 预览+selector）。

**边界加固（提交前多轮独立审查后合并）**：stale 错误不再把旧 tag 称为 current snapshot（明确 "cannot be reused，请重新 Read"）；失败的 Read 不写 memo、memo 判定基于最终 composed 输出且区分 raw/non-raw（截断/spill 的内容永不产生"复用"命中）；`.jsonl` 按行校验、多 section 与 MV 目标逐一校验；Replace `old==new` 仅目标存在时幂等（缺失保持 fail-closed、fuzzy 候选不改写文件）；幂等与 soft no-op 不写盘、不 bump mutation epoch（memo 保持有效）；截断 tail 受字节预算约束（单超长行不绕过 `tool_result_max_bytes`）；PlanConfirm/PlanClear/SubAgent 补 `deny_unknown_fields` 并新增全工具运行时拒绝测试；SubAgent 失败不再标记 spawn（非法调用不会启动子代理）；压缩守卫排除 todo reminder/final reminder/sync 与 signal recovery 注入消息（`internal`/`_mink` 元数据 + 字符串前缀双保险）；大文件预览补尾部 5 行（本地与 VFS 一致）；symlink 测试改用当前协议。新增 9 个回归用例。

**测试**：Rust 全量 583 通过（新增 44 用例：catalog schema 契约与运行时拒绝、Read 契约/预览/建议、memo 单测与集成、压缩守卫（含内部消息排除）、截断策略、幂等、JSON 注记、exec 头、SubAgent 契约、prompt 顺序/密度/门控、轨迹 fixture、外部审查回归）；`cargo fmt --check`、`cargo clippy --all-targets`、TUI 92 用例与 feature-matrix 全部通过。
## v0.3.1 (2026-08-04)

### Edit 工具稳定性（修复与改进）

- **Hashline tag 溯源强化**：unknown/stale 错误现在报告当前内容 hash、锚点上下文与明确恢复指引，禁止编造 tag 或跨 session 复用；成功 Edit 返回的 `[PATH#TAG]` header 被记录为可复用来源（`record_edit` / `is_edit_result_tag`），stale 恢复失败时直接提示复用该 header 重试。
- **Edit 输出完整进入模型上下文**：新 tag、`firstChangedLine` / `linesAdded` / `linesRemoved`、warnings、diff 与 artifact 引用（size-bounded）同时进入 UI 与 conversation，下一轮编辑可直接校准行号与 tag；删除/移动操作的输出同步增强。
- **软 no-op 升级与原子批处理**：同一 payload 连续 3 次无变更升级为硬错误；批处理中任一 section 为 no-op 时整批不提交。
- **信号误报修复**：正则错误模式检测仅对命令类工具（`ToolResultKind::Command`）生效，消除 Read/Glob/Grep/Edit 内容中的 `timeout`、`error[E0425]` 等字样产生的虚假 ToolError/CompileError 信号与信念污染。
- **引导强化**：hashline_edit 工作流 prompt 增加 `<critical>`（Edit 后取新 header、stale 立即重读）与 `<anti-patterns>`（禁止空范围 PUT、禁止改写 keeper 行、禁止编造 tag 等）；Edit 工具描述同步更新。
- **回归测试**：新增连续编辑、移动、删除、artifact 落盘、mismatch 恢复、no-op 升级与信号 gate 等 10 个用例（Rust 全量 539 通过）。

## v0.3.0 (2026-08-03)

### mink-server：Server 与 Web 前端（全新）

- **mink-server**：单二进制 Web 工作区服务器——REST + SSE API（会话管理、conversation/plan/todo/artifacts/files、`/stream` 实时事件流）、Session registry 与锁协议、turn 超时保护；与 TUI/CLI 共享 `~/.mink/projects` 会话布局。
- **前端嵌入**：`build.rs` 自动构建 web 产物并嵌入二进制（单文件分发）；`MINK_SERVER_DEV_WEB=1` 回退磁盘产物用于开发迭代；默认读取 `~/.minkrc`。
- **Web 前端**：单栏对话优先布局（顶栏面包屑、实时指标行、状态徽标）；上下文面板（计划/Todo/Artifacts/用量/文件）、文件预览（Markdown + 代码着色）、会话抽屉、空状态工作台、Home；Edit 卡片结构化渲染（anchored/unified diff，实时流 input 透传）；移动端适配（字母指标、顶栏换行、面板合并）。

### Edit 工具协议重构（Breaking）

- 输入协议从 `@path#tag + replace/insert/delete/append` 迁移至 `[PATH#TAG] + PUT/CUT/REM/MV`：单 `input` 参数、指令语义单一（替换/插入/删除/移动）、fail-closed（过期 tag 明确拒绝并提示重读）、structural-closer 安全保护，旧格式不再兼容。

### 其他变更

- **`--version` 与 git hash provenance**：`mink --version` 与 `mink-server --version` 现在输出语义化版本号与构建 git hash（工作区有未提交改动时附 `-dirty` 标记），便于定位二进制来源（`crates/mink-cli/build.rs`、`crates/mink-server/build.rs`）。


### 测试

- E2E 14 用例（真实浏览器 + 真实后端：布局几何/组件交互/数据渲染/实时广播/滚动状态机/移动端）；vitest 44；Rust 全量回归。

---
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
