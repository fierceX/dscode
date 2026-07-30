# TUI 实现说明与维护建议

更新日期：2026-07-30

## 定位

TUI 是 mink 的全屏终端交互模式，基于 `ratatui`。它不是 agent 主循环的一部分，而是 `Display` 抽象的一种实现：编排器输出 UI 事件，`TuiDisplay` 转成 `TuiSignal`，TUI 事件循环负责本地状态和渲染。

## 当前模块

| 文件 | 职责 |
|------|------|
| `crates/mink-cli/src/tui/mod.rs` | TUI 入口、事件循环、集成测试 |
| `crates/mink-cli/src/tui/display.rs` | `Display` 到 `TuiSignal` 的适配 |
| `crates/mink-cli/src/tui/file_picker.rs` | 输入区路径补全、父目录扫描和沙箱感知过滤 |
| `crates/mink-cli/src/tui/notify.rs` | TUI 任务结束后的系统通知，按平台尽力发送 |
| `crates/mink-cli/src/tui/signal.rs` | `TuiSignal` reducer |
| `crates/mink-cli/src/tui/state.rs` | 消息、输入、视口、缓存、子代理状态 |
| `crates/mink-cli/src/tui/input.rs` | 键盘、鼠标、粘贴、历史和 slash command |
| `crates/mink-cli/src/tui/command.rs` | slash command 解析 |
| `crates/mink-cli/src/tui/render.rs` / `crates/mink-cli/src/tui/render/*` | 主布局、消息区、详情页、输入区、状态栏 |
| `crates/mink-cli/src/tui/markdown.rs` / `crates/mink-cli/src/tui/markdown/*` | Markdown 子集渲染 |
| `crates/mink-cli/src/tui/replay.rs` | session 历史重放 |
| `crates/mink-cli/src/tui/sanitize.rs` | TUI 输入和展示文本的控制字符/ANSI 清理 |

## 行为边界

- 输入编辑必须保持 UTF-8 char boundary 安全。
- Ctrl+C 在工作状态中断当前 turn，空闲时按退出语义处理。
- 未知 slash command 在本地提示，不发送给模型。
- 工具结果显示使用 `ToolResultDisplay.content`，该内容已受工具层最大字节限制保护。
- 长工具结果可折叠；用户手动折叠/展开后不应被自动策略覆盖。
- 点击目标只对应当前可见 viewport。
- 子代理详情以 `session_id` 查找。

## 操作特性

| 操作 | 行为 |
|------|------|
| 多行输入 | 输入区按视觉行渲染，光标所在行保持可见 |
| `Ctrl+C` | 工作状态中断 turn，空闲状态走退出语义 |
| `/flash` / `/pro` | 切换模型 |
| `/compact` | 手动压缩上下文 |
| `/help` / `/skills` | 本地展示信息 |
| 未知 slash command | 本地提示，不进入 LLM conversation |
| 行首空格 + slash 文本 | 作为普通用户消息发送 |
| `Tab` | 打开路径选择器，支持当前目录、相对路径和受限父目录补全 |
| 任务结束 | 普通输入和 `/compact` 完成后发送系统通知，失败时发送失败通知 |
| 点击折叠消息 | 展开或收起长内容 |
| 点击子代理消息 | 打开详情页，查看 thinking/text |

系统通知按平台尽力发送：TUI 会先向终端写入常见通知 OSC escape（iTerm2/WezTerm/rxvt 风格）和 bell fallback，再异步调用系统通知命令；macOS 优先使用 `terminal-notifier`，未安装时回退到 `/usr/bin/osascript`，Linux/FreeBSD 使用 `notify-send`。通知发送失败不影响 TUI 主循环。

## Markdown 渲染

当前 renderer 面向 coding agent 输出，支持标题、段落、列表、引用、代码块、pipe table、inline code、strong/emphasis/link 和 diff。复杂 Markdown 语法按普通文本降级。

原则：

- 不 panic。
- 不丢内容。
- 不破坏终端布局。
- 优先保证 CJK 宽度、ANSI 清理、长行 wrap 和表格可读。

## 验证

当前验证基线：

```text
cargo test -p mink-cli --all-features tui
83 passed

cargo test
workspace quick tests
```

TUI 相关测试覆盖输入、中断、slash command、Markdown、表格、diff、折叠、点击目标、子代理详情、状态栏和 replay。

## 已记录低风险项

- 文件选择器的扫描缓存只在 overlay 生命周期内生效，不监听文件系统变化；关闭并重新打开 overlay 会刷新。后续如果出现长时间打开期间文件频繁变化的场景，可增加 TTL 或显式刷新键。
- 受限沙箱下，从父目录补全到允许 root 时只展示通向允许 root 的下一跳目录，不枚举同级不可访问路径。深层 allowed root 可能需要多次补全，这是安全优先的取舍。
- 空 query 的扫描深度已降低，仍然是同步扫描。超大仓库下首次打开可能有短暂停顿；如后续实测明显影响输入响应，再考虑后台索引或异步扫描。

## 维护建议

优先级建议：

1. 将集中在 `crates/mink-cli/src/tui/mod.rs` 的测试按职责迁移到 input、command、signal、render、markdown 模块。
2. 为 `Display::render_tool_result_detail()` 增加回归测试，确保工具展示内容和 LLM conversation 内容边界清晰。
3. 增加长会话本地搜索和消息类型跳转，状态只保存在 TUI 本地。
4. 为 Bash/Python/Edit/Read/TodoWrite 提供更清晰的工具结果头部和分区展示。
5. 对高频 streaming 做 signal drain 和渲染节流，保持输入和中断事件优先级。

不建议：

- 追求完整 CommonMark。
- 绕过工具层截断读取原始工具输出。
- 引入大型 UI 框架抽象。
- 在没有明确使用场景前实现多 tab 或主题系统。
