# 轨迹样本 fixture（真实生产数据驱动）

来源：`生产轨迹分析（`640 份 `conversation.jsonl`、
38,298 次工具调用：Read 28,349 / Grep 4,124 / TodoWrite 1,343 / Write 1,208 / Python 1,189 /
Glob 1,061 / Edit 937 / Bash 71 / SubAgent 16）。

## 场景（每个 fixture 对应真实轨迹片段，参数形状与报错口径取自原始记录）

| 文件 | 真实轨迹来源 | 修复后断言 |
|------|--------------|------------|
| `repeated_read.json` | compliance_report.md 单会话整读 15 次 | 第 2 次起 memo 命中（"unchanged, no edits since"），含范围覆盖 |
| `param_guess.json` | `path_selector: ":160-186"` 报 unknown field（全库 178 次：path_selector/path2/selector/selectors/path_selectors/path_sel/pathSelector/path_range；另有 limit/offset 529/495 次） | 未知字段 fail closed 并指明字段名；正确 selector 生效 |
| `no_change_loop.json` | 对 :55 反复 insert/replace 同一条文 6+ 次、tag 每次变化、60 次 "produced no changes" | 第 1 次真实更新；重试（当前 tag）幂等成功 "already applied (idempotent)" |
| `disabled_tool.json` | 全库 356 次 "Tool 'Bash' is disabled by configuration."（如 `ls -la` 调用） | 禁用工具 blocked（"unavailable"），不执行 |
| `big_file.json` | 2094 行 / 154707 字节整读报 too-large（全库 89 次） | 返回头尾预览 + 字节/行数 + selector 指引；范围读正常 |

## 运行方式

fixture 由 `regression.rs::trace_fixtures_regress_behaviors` 加载执行：
- `setup.files` 写入临时工作区；`setup.config` 合并进 ToolConfig；
- `steps` 逐条通过 `ToolRunner::execute_all` 执行；
- `asserts` 按 `after_step` 校验 success 与输出包含关系；
- 占位符由测试注入：`{{REPORT_FILE}}` 生成 200 行报告形态文本；`{{BIG_FILE}}` 生成精确
  154707 字节 / 2094 行的文件（与真实样本同尺寸）；`{{PATCH_BODY}}` 使用最近一次 Read 返回的
  真实路径与 snapshot tag 构造补丁。

断言口径（契约/prompt 层，不依赖信号机制）：
同文件重复读不重复输出全文（memo 覆盖全量→子范围）、unknown-field 首次即报错带字段名、
no-change 死循环在 5 次调用内转为幂等成功、禁用工具调用即 blocked 不执行、
大文件首次整读即返回预览与范围读指引。
