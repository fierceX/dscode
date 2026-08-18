# TUI 实现说明与维护建议

> 更新日期：2026-08-18

## 定位

TUI 是 `AgentEventStream` 结构化事件的两种终端 surface：

- `--tui` / `--tui=full`：全屏应用内 transcript，保留鼠标滚动、卡片点击、可逆折叠和详情视图。
- `--tui=inline`：原生 terminal scrollback，完成内容不可修改，面向 SSH 和长日志。

两种模式共用 `TuiSignal`、transcript reducer、session replay、Markdown、语义工具卡片、
自动折叠策略、输入编辑、状态栏和结构化详情数据。模式差异只存在于终端生命周期、viewport、
鼠标路由和最终输出方式。

## 数据流

```text
LLM / ToolRunner
  -> AgentEvent
  -> TuiSignal
  -> shared transcript reducer
  -> TranscriptItem / Plan / Todo / Artifact / SubAgent state
        ├── Full projection   -> application viewport + click map
        └── Inline projection -> sealed/stable output + insert_before

events.jsonl
  -> replay decoder
  -> same transcript reducer

plan.draft / plan.md / todos.json
  -> startup state loader
  -> current Plan / Todo detail baseline
```

工具调用和结果通过 `tool_use_id` 合并。`PresentedToolResultDisplay` 携带 `ToolStatus`、
`ToolResultKind`、Plan/Todo presentation 和 Artifact 元数据；两个 surface 均不得从展示文本
反向解析结构化状态。

## 共用渲染

工具卡片由同一 renderer 产生：

- `ToolResultKind` 决定 Read/Search/Edit/Command/Control/SubAgent 的语义颜色。
- `ToolStatus` 决定 pending/success/failure/blocked/interrupted 状态标记。
- `CollapsePolicy` 决定初始自动折叠。
- Plan、Todo、Artifact 和普通工具正文使用同一份 presentation。
- 含 Artifact 元数据的折叠卡片保留首个 `artifact://ID`，详情按 ID 有界读取正文。
- Markdown normalize、block、inline、table、diff 和 wrap 逻辑只保留一套。
- Plan、Todo 和 Artifact 详情使用扣除水平 padding 后的内容宽度折行，滚动范围按折行后的
  可视行计算。

Full projection 可通过 `collapse_overridden` 改变自动折叠结果；Inline projection 只输出自动
折叠后的最终形态，不显示可展开标识。

## Full TUI

Full 模式使用 alternate screen 和 mouse capture。内容区保存完整 transcript：

- 鼠标滚轮和 PageUp/PageDown 操作应用内 viewport。
- 点击普通可折叠卡片切换展开状态。
- 点击 Plan、Todo、Artifact 或 SubAgent 卡片进入对应详情。
- 输入区、文件选择器和状态栏固定在主视图底部。
- 实时 signal 和 replay 使用完全相同的 item 模型。

Full 模式不读取 Inline 的 committed 边界。

## Inline TUI

Inline 模式使用 ratatui inline viewport，主视图不启用 mouse capture：

- 只能提交从 `inline.committed` 开始的连续 sealed 前缀。
- committed item 不得再次修改或重复写入。
- 流式 assistant text 在完整 Markdown 段落或闭合代码块处提前形成稳定输出。
- 空闲时保留最后一个 sealed item；新工作开始后再将它提交到 scrollback。
- `insert_before` 使用 terminal scrolling region，宽字符占位 cell 不得输出为可见空格。
- 动态 viewport 高度依据终端尺寸在 8–12 行内计算。
- Plan、Todo、Artifact 和 SubAgent 详情可临时进入 alternate screen，退出后恢复 inline viewport。
- alternate screen 生命周期保存并恢复同一个 inline terminal；禁止在返回主视图时重新创建
  inline viewport。

Inline 写入不额外插入 item 间空行；Markdown parser 只保留原始内容实际表达的段落间距。

## 不变式

- 实时和 replay 必须经过同一个 reducer。
- 工具 call/result 必须按稳定 `tool_use_id` 合并。
- AgentEvent 投影必须保留详细调用 ID 和结构化 result presentation；缺少或不匹配
  `tool_use_id` 时不得根据工具名推断关联。
- Todo mutation presentation 必须归并到当前完整 Todo 状态。
- 缺失结果的 replay 工具调用和终止信号前的未完成项必须封口。
- Full 与 Inline 必须调用同一个工具卡片和 Markdown renderer。
- Full 的折叠覆盖只影响交互投影；Inline committed 状态只影响原生输出投影。
- Artifact 详情最多读取 256 KiB。
- Plan/Todo/Artifact 详情必须先折行再计算垂直滚动边界。
- 输入 cursor 始终位于 UTF-8 char boundary。

## 验证

```bash
cargo test -p mink-cli --features tui
cargo test -p mink-core
cargo check --workspace --all-targets
```

测试至少覆盖：

- `--tui`、`--tui=full` 和 `--tui=inline` 参数解析。
- 工具调用/结果合并和 v2 replay。
- ToolResultKind 语义颜色在共享 renderer 中保持稳定。
- Full click map、鼠标折叠和结构化详情动作。
- Inline sealed boundary、稳定 Markdown 提交和无重复输出。
- Markdown 结尾换行不产生额外空白行。
- UTF-8 输入、Ctrl+C、文件选择器、表格、diff、详情长行折行和滚动。

## 维护建议

1. 为 Full 模式增加可见 item 高度索引，避免长会话每次重建完整扁平行缓存。
2. 为 Full 滚动增加事件合并和终端输出字节基准，控制 SSH 重绘成本。
3. 为 Full/Inline 终端初始化与恢复增加伪终端集成测试。
4. Inline 如扩展 Markdown 稳定边界，必须保证未闭合 table/fence 不会提前提交。
5. Artifact 如需浏览后续区段，应扩展有界分页接口，不得绕过工具层限制一次性读取全文。
