// 工具展示辅助：类型着色、参数摘要、结果徽章（纯函数，可单测）。

import type { ResultView, ToolColor } from "./types";

/** 工具名 → 语义颜色 */
export function toolColor(name: string | undefined): ToolColor {
  const n = String(name ?? "");
  if (/^Bash$|^Python/.test(n)) return "exec";
  if (/^Read$|^Write$|^Edit$/.test(n)) return "file";
  if (/^Glob$|^Grep$/.test(n)) return "search";
  if (/^TodoRead|^TodoWrite|^TodoAdvance/.test(n)) return "todo";
  if (/^Plan/.test(n)) return "plan";
  if (/^SubAgent$/.test(n)) return "delegate";
  return "tool";
}

/** 工具参数摘要：按工具类型解析 input（人眼友好的单行） */
export function toolSummary(name: string | undefined, input: unknown): string {
  const raw = (input ?? {}) as Record<string, unknown>;
  switch (name) {
    case "Bash":
    case "Python":
      return truncate(String(raw.command ?? ""), 80);
    case "Read":
      return [raw.path, raw.selector].filter(Boolean).join(" ");
    case "Write":
    case "Edit":
      return String(raw.path ?? "");
    case "Glob":
      return String(raw.pattern ?? "");
    case "Grep":
      return [raw.pattern, raw.path].filter(Boolean).join("  ");
    case "TodoRead":
      return "查看 Todo";
    case "TodoWrite": {
      const parts: string[] = [];
      const add = Array.isArray(raw.add) ? raw.add.length : 0;
      const remove = Array.isArray(raw.remove) ? raw.remove.length : 0;
      const update = Array.isArray(raw.update) ? raw.update.length : 0;
      if (add) parts.push(`+${add}`);
      if (remove) parts.push(`-${remove}`);
      if (update) parts.push(`~${update}`);
      return `更新 Todo ${parts.join(" ")}`.trim();
    }
    case "TodoAdvance": {
      const parts: string[] = [];
      if (Array.isArray(raw.activate) && raw.activate.length) parts.push(`激活 ${raw.activate.length}`);
      if (Array.isArray(raw.complete) && raw.complete.length) parts.push(`完成 ${raw.complete.length}`);
      if (Array.isArray(raw.pause) && raw.pause.length) parts.push(`暂停 ${raw.pause.length}`);
      return parts.join(" ") || "推进 Todo";
    }
    case "PlanDraft":
      return "起草计划";
    case "PlanConfirm":
      return "确认计划";
    case "PlanClear":
      return "清除计划";
    case "SubAgent":
      return String(raw.prompt ?? "").slice(0, 80);
    default:
      return JSON.stringify(input).slice(0, 120);
  }
}

/** 结果类型徽章（与核心 ToolResultKind 对应）。
 * result_kind 缺失时按工具名推断——对齐 TUI content.rs:223 的兜底映射。 */
export function kindBadge(kind: string | undefined, name?: string): string {
  switch (kind) {
    case "Command": return "CMD";
    case "FileRead": return "READ";
    case "FileWrite": return "WRITE";
    case "Edit": return "EDIT";
    case "Search": return "SEARCH";
    case "SubAgent": return "AGENT";
    case "Control": return "CTRL";
    case "Text": return "TEXT";
  }
  switch (name) {
    case "Read": return "READ";
    case "Glob":
    case "Grep": return "SEARCH";
    case "Write": return "WRITE";
    case "Edit": return "EDIT";
    case "Bash":
    case "Python":
    case "PythonSandbox": return "CMD";
    case "PlanDraft":
    case "PlanConfirm":
    case "PlanClear":
    case "TodoRead":
    case "TodoWrite":
    case "TodoAdvance": return "CTRL";
    case "SubAgent": return "AGENT";
    default: return "TEXT";
  }
}

/** 提取 artifact:// 引用 */
export function extractArtifact(content: string): string | null {
  const m = String(content).match(/artifact:\/\/([A-Za-z0-9._-]+)/);
  return m ? m[1] : null;
}

/** 结果是否失败：优先 success 字段，兼容 Error 前缀文本 */
export function isFailed(success: boolean | undefined, content: string): boolean {
  if (success === false) return true;
  return /^Error[:：\s]/.test(content);
}

/** 格式化信号行 */
export function formatSignal(raw: Record<string, unknown>): string {
  const sev =
    typeof raw.severity === "number" ? ` severity=${raw.severity.toFixed(2)}` : "";
  return `signal[${String(raw.signal_kind ?? "")}${sev}] ${String(raw.message ?? "")}`;
}


/** 工具名 → 结果视图类型（TUI 语义对齐：命令/文件/搜索/Todo/Plan/编辑差异） */
/** 解析 presentation.changes（Todo 增量变更摘要） */
export interface ChangeSummary {
  added: number;
  removed: number;
  completed: number;
  reopened: number;
  updated: number;
  activated: number;
  paused: number;
}

export function parseChanges(presentation: unknown): ChangeSummary {
  const zero: ChangeSummary = { added: 0, removed: 0, completed: 0, reopened: 0, updated: 0, activated: 0, paused: 0 };
  const changes = (presentation as { data?: { changes?: { change?: string }[] } })?.data?.changes;
  if (!Array.isArray(changes)) return zero;
  const out = { ...zero };
  for (const c of changes) {
    const kind = c.change;
    if (kind === "added") out.added++;
    else if (kind === "removed") out.removed++;
    else if (kind === "completed") out.completed++;
    else if (kind === "reopened") out.reopened++;
    else if (kind === "updated") out.updated++;
    else if (kind === "activated") out.activated++;
    else if (kind === "paused") out.paused++;
  }
  return out;
}

/** 变更摘要 → 展示文本 */
export function formatChanges(summary: ChangeSummary): string {
  const parts: string[] = [];
  if (summary.added) parts.push(`新增 ${summary.added}`);
  if (summary.removed) parts.push(`删除 ${summary.removed}`);
  if (summary.completed) parts.push(`完成 ${summary.completed}`);
  if (summary.reopened) parts.push(`重开 ${summary.reopened}`);
  if (summary.updated) parts.push(`更新 ${summary.updated}`);
  if (summary.activated) parts.push(`激活 ${summary.activated}`);
  if (summary.paused) parts.push(`暂停 ${summary.paused}`);
  return parts.join(" · ");
}

export function resultViewKind(name: string | undefined): ResultView {
  switch (name) {
    case "Bash":
    case "Python":
      return "command";
    case "Read":
    case "Write":
      return "file";
    case "Glob":
    case "Grep":
      return "search";
    case "TodoRead":
    case "TodoWrite":
    case "TodoAdvance":
      return "todo";
    case "PlanDraft":
    case "PlanConfirm":
    case "PlanClear":
      return "plan";
    case "Edit":
      return "diff";
    default:
      return "text";
  }
}

/** ═══ Todo 结果解析（对齐核心 tools/todo.rs 生成格式）═══
 *  <todo-snapshot revision pending in_progress completed>
 *    - [status] ID: 内容
 *  <todo-event revision kind="structure|progress">
 *    - added ID [status]: 内容  /  - updated ID: 内容  /  - removed ID
 *    Completed: id1, id2  /  Activated: ...  /  Paused: ...  /  Reopened: ...
 *  <current-todos revision pending in_progress completed>
 *    Pending todo items exist... / Active batch: / - ID: 内容
 */

export interface TodoTask {
  status: string;
  id: string;
  text: string;
}

export interface TodoBlock {
  kind: "snapshot" | "event" | "current";
  revision?: number;
  counts?: { pending: number; in_progress: number; completed: number };
  /** event 变更行（structure 的 added/updated/removed 或 progress 的 Completed:/Activated:...） */
  lines?: string[];
  /** current-todos 的提示文本或 Active batch 任务 */
  note?: string;
  tasks: TodoTask[];
}

export function parseTodoContent(content: string): TodoBlock[] {
  const text = String(content);
  const blocks: TodoBlock[] = [];

  // <todo-snapshot>
  const snap = text.match(/<todo-snapshot[^>]*>/);
  if (snap) {
    const head = snap[0];
    const revision = numAttr(head, "revision");
    const counts = countsOf(head);
    const tasks: TodoTask[] = [];
    for (const m of text.matchAll(/^- \[([a-z_]+)\] (\S+): (.+)$/gm)) {
      tasks.push({ status: m[1], id: m[2], text: m[3].trim() });
    }
    blocks.push({ kind: "snapshot", revision, counts, tasks });
  }

  // <todo-event ...>
  const ev = text.match(/<todo-event[^>]*>/);
  if (ev) {
    const head = ev[0];
    const end = text.indexOf("</todo-event>", ev.index ?? 0);
    const body = end > 0 ? text.slice((ev.index ?? 0) + head.length, end) : "";
    blocks.push({
      kind: "event",
      revision: numAttr(head, "revision"),
      lines: body.split("\n").map((l) => l.trim()).filter(Boolean),
      tasks: [],
    });
  }

  // <current-todos ...>
  const cur = text.match(/<current-todos[^>]*>/);
  if (cur) {
    const head = cur[0];
    const end = text.indexOf("</current-todos>", cur.index ?? 0);
    const body = end > 0 ? text.slice((cur.index ?? 0) + head.length, end) : "";
    const tasks: TodoTask[] = [];
    let note = "";
    for (const line of body.split("\n").map((l) => l.trim()).filter(Boolean)) {
      const m = line.match(/^- (\S+): (.+)$/);
      if (m) {
        tasks.push({ status: "in_progress", id: m[1], text: m[2].trim() });
      } else if (line !== "</current-todos>") {
        note = line;
      }
    }
    blocks.push({
      kind: "current",
      revision: numAttr(head, "revision"),
      counts: countsOf(head),
      note,
      tasks,
    });
  }

  return blocks;
}

function numAttr(head: string, name: string): number | undefined {
  const m = head.match(new RegExp(name + `="(\\d+)"`));
  return m ? Number(m[1]) : undefined;
}

function countsOf(head: string): { pending: number; in_progress: number; completed: number } | undefined {
  const pending = numAttr(head, "pending");
  const in_progress = numAttr(head, "in_progress");
  const completed = numAttr(head, "completed");
  if (pending === undefined || in_progress === undefined || completed === undefined) return undefined;
  return { pending, in_progress, completed };
}

/** event 行分类（对齐核心 change 语义） */
export type TodoEventLineKind = "add" | "update" | "remove" | "label";

/** diff 行分类（Edit 结果）：+ 增 / - 删 / @@ 头 / 其余上下文 */
export type DiffLineKind = "add" | "del" | "head" | "ctx";

export function classifyDiffLine(line: string): DiffLineKind {
  if (line.startsWith("@@")) return "head";
  if (line.startsWith("+") && !line.startsWith("+++")) return "add";
  if (line.startsWith("-") && !line.startsWith("---")) return "del";
  return "ctx";
}

export function classifyTodoLine(line: string): TodoEventLineKind {
  if (/^- added /.test(line)) return "add";
  if (/^- updated /.test(line)) return "update";
  if (/^- removed /.test(line)) return "remove";
  return "label"; // Completed:/Activated:/Paused:/Reopened:
}

/** 截断长文本（保留语义省略号） */
function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}

/** ═══ Edit patch 解析（锚定编辑指令）═══
 *  @/path/file#TAG
 *  replace 3..3:
 *  +新内容
 *  insert after 5:
 *  +内容
 *  delete 3..5
 *
 *  也兼容 unified diff（--- a/… / +++ b/… / @@ -N,M +N,M @@ / +/- 行），
 *  头部行转为结构化渲染，不再作为原始文本显示。
 */
export interface PatchLine {
  op: "add" | "del" | "replace" | "insert" | "delete" | "append" | "hunk" | "head" | "context";
  range?: string;
  content: string;
}

export interface ParsedPatch {
  path: string;
  tag: string;
  lines: PatchLine[];
}

export function parsePatch(patch: string): ParsedPatch | null {
  const text = String(patch);
  const rows = text.split("\n");
  const head = rows[0] ?? "";
  const m = head.match(/^@(.+)#([0-9A-Fa-f]+)$/);
  const path = m ? m[1] : "";
  const tag = m ? m[2] : "";
  const lines: PatchLine[] = [];
  // unified diff：--- a/<path> 开头
  const unified = /^--- a\//.test(head) || /^--- /.test(head);
  if (unified && !path) {
    const pm = head.match(/^--- (?:a\/)?(.+)$/);
    if (pm) lines.push({ op: "head", content: `--- ${pm[1]}` });
  }
  for (const raw of rows.slice(1)) {
    const line = raw;
    const opMatch = line.match(/^(replace|insert|delete|append)\b/);
    if (opMatch) {
      const op = opMatch[1] as PatchLine["op"];
      lines.push({ op, range: line.slice(op.length).trim(), content: "" });
    } else if (line.startsWith("+++")) {
      const bm = line.match(/^\+\+\+ (?:b\/)?(.+)$/);
      lines.push({ op: "head", content: `+++ ${bm ? bm[1] : line.slice(3).trim()}` });
    } else if (line.startsWith("---")) {
      const am = line.match(/^--- (?:a\/)?(.+)$/);
      lines.push({ op: "head", content: `--- ${am ? am[1] : line.slice(3).trim()}` });
    } else if (line.startsWith("@@")) {
      lines.push({ op: "hunk", range: line.replace(/^@@\s*/, "").replace(/\s*@@$/, ""), content: "" });
    } else if (line.startsWith("+")) {
      lines.push({ op: "add", content: line.slice(1) });
    } else if (line.startsWith("-")) {
      lines.push({ op: "del", content: line.slice(1) });
    } else if (/^\d+: /.test(line)) {
      // 带行号快照行（Read/快照输出混入 patch）——剥离行号，作为内容行渲染
      lines.push({ op: "context", content: line.replace(/^\d+: /, "") });
    } else if (line.trim() !== "") {
      lines.push({ op: "context", content: line });
    }
  }
  if (!path && lines.length === 0) return null;
  return { path, tag, lines };
}

/** ═══ conversation.jsonl → 伪事件转换 ═══
 * 一条 conversation 消息展开为多条事件（复用 reduceEvent 渲染）。
 * seq 用 行号*100+子序号 保证 key 唯一。
 */
export function conversationToEvents(raw: Record<string, unknown>): RawEventLike[] {
  const seq = Number(raw.seq ?? 0);
  const role = String(raw.role ?? "");
  const content = raw.content;
  const out: RawEventLike[] = [];
  let i = 0;
  const s = () => seq * 100 + i++;
  if (role === "user") {
    if (typeof content === "string") {
      out.push({ type: "user_input", content, seq: s() });
    } else if (Array.isArray(content)) {
      for (const c of content) {
        const cobj = c as Record<string, unknown>;
        if (cobj.type === "tool_result") {
          out.push({ type: "tool_result", tool_use_id: cobj.tool_use_id, content: cobj.content ?? "", seq: s() });
        }
      }
    }
  } else if (role === "assistant") {
    if (Array.isArray(content)) {
      for (const c of content) {
        const cobj = c as Record<string, unknown>;
        if (cobj.type === "thinking") {
          const text = String(cobj.thinking ?? cobj.content ?? "");
          if (text.trim()) out.push({ type: "thinking", content: text, seq: s() });
        }
        else if (cobj.type === "text") out.push({ type: "text", content: cobj.text ?? cobj.content ?? "", seq: s() });
        else if (cobj.type === "tool_use") out.push({ type: "tool_call", id: cobj.id, name: cobj.name, input: cobj.input ?? {}, seq: s() });
      }
    }
  }
  return out;
}

export interface RawEventLike {
  type: string;
  seq: number;
  [k: string]: unknown;
}

/** 剥离 ANSI 转义码（Bash/Edit 结果常含彩色转义，前端按行分类重新着色） */
export function stripAnsi(text: string): string {
  return String(text).replace(/\u001b\[[0-9;]*m/g, "");
}
