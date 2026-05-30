# TUI 优化路线与变更记录

更新日期：2026-05-31

## 目标

本文档记录当前 TUI 改造的阶段性结果，并给出阶段 6 之前及阶段 6 的主要功能优化方向。主题聚焦 `ratatui` 全屏交互体验、状态模型稳固、渲染性能、子代理展示和可测试性。

当前判断：TUI 主路径已经具备继续进入阶段 6 的基础，但阶段 6 应先以“状态模型继续收敛 + 渲染和交互边界加固”为起点，再推进更复杂的体验功能。

## 当前 TUI 结构

当前 TUI 已从原来的单一 `src/tui/mod.rs` 拆分为多个职责明确的模块：

| 文件 | 职责 |
|------|------|
| `src/tui/mod.rs` | TUI 入口、主循环、测试集合 |
| `src/tui/state.rs` | TUI 状态、消息行、工作状态、视图状态 |
| `src/tui/signal.rs` | `TuiSignal` 定义和信号到状态的 reducer |
| `src/tui/input.rs` | 键盘、鼠标、粘贴、slash command 输入处理 |
| `src/tui/render.rs` | 主视图、输入区、状态栏、子代理详情渲染 |
| `src/tui/markdown.rs` | Markdown、diff、ANSI 清理和软换行渲染 |
| `src/tui/replay.rs` | session JSONL 历史重放 |
| `src/tui/display.rs` | `Display` trait 到 TUI signal 的适配层 |

入口仍然保持在 `run_tui()`，编排器仍通过 `Display` trait 输出事件，TUI 线程通过 `mpsc::Receiver<TuiSignal>` 消费并渲染。

## 当前已完成变更

### 1. 模块拆分

将 TUI 从大文件拆分为 `display/input/markdown/render/replay/signal/state` 等模块，降低单文件复杂度，便于后续分别优化输入、渲染和状态转换。

收益：
- 降低 `mod.rs` 的职责密度。
- 信号处理和渲染逻辑分离。
- 后续测试可以更自然地覆盖纯函数和 reducer。

### 2. 工作状态统一

移除旧的 `busy` 状态，统一使用 `WorkState` 描述 TUI 当前状态：

- `Idle`
- `WaitingModel`
- `StreamingThinking`
- `StreamingText`
- `RunningTool`
- `RunningSubAgent`
- `Compacting`
- `Error`

收益：
- 状态栏不再依赖 `busy + streaming` 的组合判断。
- Ctrl+C 是否中断当前任务改为基于 `WorkState::is_working()`。
- `/compact`、工具执行、流式输出和错误状态更容易推理。

### 3. `/compact` 状态闭环

修复手动 `/compact` 后 TUI 卡在 `compacting` 的问题。编排器手动 compact 分支结束后调用 `render_stop()`，让 TUI 回到 `Idle`。

涉及文件：
- `src/agent/orchestrator.rs`

### 4. 输入区多行和视口加固

输入区支持多行输入后，新增输入视口滚动，光标所在行始终保持可见。`split_at_visual_width()` 现在正确处理显式换行：

- `a\nb` 拆成两行。
- `a\n` 保留末尾空行。
- `\n` 拆成两个空视觉行。
- CJK 宽字符仍按视觉宽度处理。

涉及文件：
- `src/tui/render.rs`
- `src/tui/input.rs`

### 5. Stream cache 优化

流式输出缓存从完整文本比较改为 revision 校验：

- `stream_revision`
- `cached_stream_revision`

收益：
- 避免每帧比较完整 `stream_line`。
- 降低长流式输出时的 clone 和字符串比较成本。
- 保持历史消息缓存和当前 streaming 缓存分离。

涉及文件：
- `src/tui/state.rs`
- `src/tui/render.rs`
- `src/tui/signal.rs`

### 6. 子代理信号结构化

`SubAgentStatus` 从格式化字符串改为结构化字段：

```rust
SubAgentStatus {
    session_id: String,
    status: String,
    in_tokens: u64,
    out_tokens: u64,
}
```

TUI 内部通过 `session_id -> line_idx` 更新对应子代理行，避免依赖字符串 contains 匹配。

收益：
- 避免 session id 字符串误匹配。
- 重复状态更新不会插入重复行。
- 子代理流式内容和最终输出可以稳定写回同一行。

### 7. 子代理活跃状态加固

将 `active_sub_agents: usize` 改为 `active_sub_agent_sessions: HashSet<String>`。

收益：
- 重复 `SubAgentOutput` 不会导致活跃计数漂移。
- 状态判断从计数增减改为集合 membership，更贴近真实语义。
- 后续支持子代理重试、乱序事件和异常恢复时更稳。

### 8. 未知 slash command 拦截

未知 `/xxx` 命令不再发送给模型，而是在 TUI 内提示：

```text
Unknown command. Prefix with a space to send it as text.
```

收益：
- 避免用户误输入命令时污染 LLM 上下文。
- slash command 行为更接近本地控制命令。

### 9. 长会话滚动行号加固

主视图内部滚动、点击映射和有效滚动位置从 `u16` 改为 `usize`。

收益：
- 避免长会话或大量工具输出时物理行号截断。
- 终端坐标仍在最终渲染位置使用 `u16`，但内部逻辑不再受终端坐标类型限制。

## 当前验证结果

最近一次验证：

```text
cargo test tui
15 passed, 0 failed

cargo test
287 passed, 0 failed, 6 ignored
```

新增或加强的 TUI 测试覆盖：

- CJK 视觉宽度切分。
- 显式换行切分。
- ANSI 清理。
- session replay 最近历史恢复。
- thinking/text stream 切换。
- 子代理状态更新复用已有行。
- 子代理 output 更新缓存失效。
- 重复子代理 output 不造成活跃状态漂移。
- 未知 slash command 不进入 orchestrator。
- 输入视口保持光标可见。
- Ctrl+C 中断和退出行为。
- 鼠标点击不命中时不误折叠。
- 可视行切片只 clone viewport 范围。
- readline 快捷键和 Shift+Enter 多行输入。

## 当前仍需关注的风险

### 1. `TuiState` 仍然偏胖

`TuiState` 当前同时持有：

- 消息列表。
- streaming 状态。
- 输入缓冲区。
- 历史输入。
- 滚动位置。
- 渲染缓存。
- 子代理索引。
- 工作状态。
- 视图状态。

短期可接受，但阶段 6 如果继续叠功能，建议拆出更小状态对象：

- `InputState`
- `ViewportState`
- `RenderCache`
- `SubAgentState`

### 2. 详情视图仍使用 `line_idx`

`View::SubAgentDetail { line_idx, scroll }` 当前依赖消息列表 append-only 的事实。只要后续不做消息删除、历史裁剪、虚拟列表或重排，它是稳定的。

阶段 6 如涉及虚拟滚动或消息裁剪，应改为稳定 id：

- 子代理详情优先使用 `session_id`。
- 普通消息详情未来可使用 `message_id`。

### 3. Markdown 样式仍会被软换行扁平化

`wrap_lines_word()` 当前将一行 spans 合并为纯文本，并使用首个 span 的 style。对于复杂 Markdown，一行内部的局部样式可能丢失。

这不影响稳定性，但影响展示质量。阶段 6 如果做 UI polish，应优化为 span-aware wrapping。

### 4. 表格渲染仍较粗糙

`render_md_with_tables()` 当前用简单条件识别表格并以 raw line 输出，能避免 `tui_markdown` 误解析，但没有做列宽对齐和 viewport 裁剪优化。

阶段 6 可按实际需求决定是否投入：

- 若目标是 coding agent 工具输出可读性，表格优化优先级中等。
- 若目标是性能和稳定，表格优化可以后置。

### 5. 主循环渲染频率仍较简单

当前 streaming 时 poll timeout 为 16ms，非 streaming 为 100ms。这个策略足够简单稳定，但没有对高频 signal 做 coalescing。

后续如果发现 streaming 输出时 CPU 占用偏高，可考虑：

- drain signals 后按 frame budget 合并渲染。
- 对 text/thinking 小 chunk 做短窗口聚合。
- 保持输入事件优先级高于普通 repaint。

## 阶段 6 建议目标

阶段 6 不建议优先追求“功能越多越好”，而应围绕 TUI 的长期可维护性和高频使用体验推进。

推荐目标：

1. 状态模型继续收敛。
2. 渲染性能和长会话体验提升。
3. 子代理详情体验完善。
4. 输入交互更接近成熟终端工具。
5. 增加 snapshot/fixture 级测试，降低 UI 重构风险。

## 阶段 6 建议实施顺序

### 6.1 状态拆分

优先拆出：

```rust
InputState {
    buf,
    cursor,
    scroll_row,
    history,
    history_idx,
}

ViewportState {
    scroll,
    max_scroll,
    auto_scroll,
    content_y,
    effective_scroll,
    click_map,
}

RenderCache {
    cached_width,
    cached_all,
    stream_revision,
    cached_stream_width,
    cached_stream_kind,
    cached_stream_revision,
    cached_stream_lines,
}

SubAgentState {
    lines,
    active_sessions,
}
```

验收标准：
- 不改变用户可见行为。
- `cargo test tui` 和 `cargo test` 通过。
- `TuiState` 字段数量明显下降。
- reducer 中对子状态的修改更集中。

### 6.2 子代理详情改为稳定 id

将 `View::SubAgentDetail { line_idx, scroll }` 改为：

```rust
View::SubAgentDetail {
    session_id: String,
    scroll: usize,
}
```

渲染时从 `sub_agent_lines` 查询 line index。

验收标准：
- 子代理行更新后详情视图仍显示正确内容。
- 未来消息裁剪或重排不会破坏详情页。
- 增加测试覆盖“打开详情后子代理 output 更新”的场景。

### 6.3 输入系统增强

建议新增：

- Alt+Backspace 删除前一个词。
- Ctrl+D 空输入退出或按配置行为。
- Ctrl+L 清屏或重置当前 viewport。
- 粘贴多行时保持 cursor 在合法 char boundary。
- 历史输入支持前缀搜索，或至少支持正在编辑时不误触历史切换。

验收标准：
- 多字节字符下所有 cursor 操作不 panic。
- 粘贴、删除、移动均保持 `input_cursor` 在 char boundary。
- 补充中文、emoji、CJK 宽字符相关测试。

### 6.4 渲染性能与长会话优化

建议方向：

- click map 只为可视区域构建，或记录全局行号但避免全量 clone。
- `cached_all` 可考虑分段缓存，而不是所有历史行合并为一个大 Vec。
- 工具结果和长 markdown 输出引入 collapsed/expand 策略。
- 子代理详情页对长 thinking/text 内容做缓存。

验收标准：
- 长会话滚动和 streaming 无明显卡顿。
- 大工具输出不会导致每帧大规模 clone。
- 新增大输出 fixture 测试或 micro benchmark。

### 6.5 展示体验优化

建议功能：

- 消息类型视觉层级统一：user/tool/thinking/text/error/sub-agent。
- 工具结果支持默认折叠，展开查看完整内容。
- 状态栏支持中断提示，例如 `interrupting`。
- 子代理详情页支持 thinking/text 分区折叠。
- 错误信息在状态栏和消息区同时给出明确反馈。

验收标准：
- 不增加复杂装饰。
- 所有状态在窄终端下不发生文字重叠。
- 重要操作可通过键盘完成。

### 6.6 测试体系补强

建议补充：

- reducer 单测：所有 `TuiSignal` 到状态转换。
- input 单测：多字节字符和 char boundary。
- render helper 单测：viewport、scroll、click map。
- replay fixture：真实 events JSONL 样例。
- 可选 snapshot：固定 terminal size 下的 `ratatui::TestBackend` 输出。

验收标准：
- 每个阶段 6 子改动都有对应测试。
- 至少覆盖一条“长会话 + stream + tool + sub-agent”的组合路径。

## 阶段 6 不建议立即做的事

以下事项可以后置：

- 过早引入复杂 UI 框架层。
- 大规模主题系统。
- 未验证需求的多 tab 工作区。
- 复杂动画或视觉装饰。
- 在状态模型未继续收敛前做虚拟列表大改。

## 当前本地变更记录

当前 TUI 相关变更主要包括：

### 修改文件

- `src/tui/mod.rs`
  - 保留 TUI 入口和测试。
  - 移除大部分具体实现。
  - 新增多项 TUI 回归测试。

- `src/agent/orchestrator.rs`
  - 手动 compact 分支结束后调用 `render_stop()`，修复 TUI compact 状态闭环。

### 新增文件

- `src/tui/display.rs`
- `src/tui/input.rs`
- `src/tui/markdown.rs`
- `src/tui/render.rs`
- `src/tui/replay.rs`
- `src/tui/signal.rs`
- `src/tui/state.rs`

### 需要注意

当前 `git status` 中还存在一些与 TUI 无关的未跟踪文件，例如：

- `.DS_Store`
- `.dscoderc`
- `.github/workflows/publish.yml`
- `build/`
- `docs/BACE.md`
- `missions/`
- `pyproject.toml`
- `skill_x.md`
- `test.py`

这些文件未在本轮 TUI 优化中处理。提交时建议只纳入 TUI 相关文件和本路线文档，避免混入无关变更。

## 推荐提交范围

建议以一次 TUI 主题提交纳入：

- `src/agent/orchestrator.rs`
- `src/tui/mod.rs`
- `src/tui/display.rs`
- `src/tui/input.rs`
- `src/tui/markdown.rs`
- `src/tui/render.rs`
- `src/tui/replay.rs`
- `src/tui/signal.rs`
- `src/tui/state.rs`
- `docs/TUI_OPTIMIZATION_ROADMAP.md`

建议提交信息：

```text
refactor(tui): modularize renderer and harden state handling
```

## 阶段 6 入口建议

建议阶段 6 从以下任务开始：

1. 将 `TuiState` 拆分为输入、视口、缓存和子代理子状态。
2. 将子代理详情视图从 `line_idx` 改为 `session_id`。
3. 用 `ratatui::TestBackend` 增加一组固定尺寸渲染测试。
4. 对长工具输出和长 streaming 输出做性能观察，再决定是否引入分段缓存。

这一路径能最大限度保护当前已经稳定的主路径，同时为后续更复杂的 TUI 功能提供更稳的基础。
