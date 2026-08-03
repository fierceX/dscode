// 事件 → 状态 纯函数 reducer。
// 历史重放与 SSE 实时共用；不可变更新，便于测试与 Svelte 细粒度渲染。

import type { RawEvent, SessionState, ToolItem, TranscriptItem } from "./types";
import { emptySession } from "./types";
import {
  extractArtifact,
  formatSignal,
  isFailed,
  kindBadge,
  resultViewKind,
  stripAnsi,
  toolColor,
  toolSummary,
} from "./toolFormat";

/** 追加或合并流式块（thinking/text 同段连续追加）。
 * key 必须保留（合并时沿用原 key，新块用事件 seq）——懒加载依赖首项 key。 */
function appendStream(
  items: TranscriptItem[],
  kind: "thinking" | "text",
  content: string,
  seq?: unknown,
): TranscriptItem[] {
  const last = items[items.length - 1];
  if (last && last.kind === kind) {
    const updated = { ...last, text: last.text + content };
    return [...items.slice(0, -1), updated];
  }
  const item = kind === "thinking" ? { kind, text: content } : { kind, text: content };
  return [...items, { ...item, key: Number(seq ?? 0) }];
}

/** 查找等待结果的工具项（最近未填充的 tool item） */
function lastPendingTool(items: TranscriptItem[], toolUseId?: string): number {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.kind === "tool") {
      if (toolUseId && item.id !== toolUseId) continue;
      if (item.result === undefined) return i;
      if (!toolUseId) return -1;
    }
    // 遇到用户消息或结果已填充的边界：停止回溯
    if (item.kind === "user" || (item.kind === "tool" && item.result !== undefined && !toolUseId)) {
      break;
    }
  }
  return -1;
}

/** 给 item 附加稳定 key（事件 seq，用于懒加载前插时的 each key） */
function withKey<T extends object>(item: T, raw: RawEvent): T & { key: number } {
  return { ...item, key: Number(raw.seq ?? 0) };
}

/** 前插一批更早的事件（懒加载）：新批用空会话独立 reduce（避免与旧 items
 * 流式合并导致 addedCount=0/顺序颠倒），再整体拼接头部。 */
export function prependEvents(state: SessionState, raws: RawEvent[]): SessionState {
  let batch = emptySession(state.sessionId, state.title);
  let firstSeq = 0;
  for (const raw of raws) {
    if (!firstSeq) firstSeq = Number(raw.seq ?? 0);
    batch = reduceEvent(batch, raw);
  }
  return {
    ...state,
    items: [...batch.items, ...state.items],
    lastSeq: Math.max(state.lastSeq, firstSeq),
  };
}

/** 单一入口：处理一个原始事件，返回新状态 */
export function reduceEvent(state: SessionState, raw: RawEvent): SessionState {
  const next: SessionState = {
    ...state,
    lastSeq: Math.max(state.lastSeq, raw.seq ?? 0),
    items: state.items,
  };

  switch (raw.type) {
    case "user_input":
      next.items = [...next.items, withKey({ kind: "user", text: String(raw.content ?? "") }, raw)];
      break;

    case "thinking":
      next.workState = "thinking";
      next.items = appendStream(next.items, "thinking", String(raw.content ?? ""), raw.seq);
      break;

    case "text":
      next.workState = "generating";
      next.items = appendStream(next.items, "text", String(raw.content ?? ""), raw.seq);
      break;

    case "tool_call": {
      next.workState = "tool";
      const id = String(raw.id ?? "");
      const name = String(raw.name ?? "tool");
      // AgentEvent 流（实时）的 ToolCall 只有 name+summary（无 input）；
      // conversation 历史有完整 input。summary 优先用 raw.summary，
      // input 缺失时用 summary 兜底（调用区不显示 "{}"）。
      const summary = raw.summary ? String(raw.summary) : toolSummary(name, raw.input);
      const item: ToolItem = {
        kind: "tool",
        id,
        name,
        color: toolColor(name),
        view: resultViewKind(name),
        summary,
        input:
          typeof raw.input === "string"
            ? raw.input
            : raw.input
              ? JSON.stringify(raw.input, null, 2)
              : summary,
        key: Number(raw.seq ?? 0),
      };
      next.items = [...next.items, item];
      break;
    }

    case "tool_result": {
      const idx = lastPendingTool(next.items, String(raw.tool_use_id ?? ""));
      if (idx < 0) break;
      const item = next.items[idx] as ToolItem;
      const content = stripAnsi(String(raw.content ?? ""));
      const updated: ToolItem = {
        ...item,
        result: content,
        resultKind: kindBadge(String(raw.result_kind ?? ""), item.name),
        success: typeof raw.success === "boolean" ? raw.success : undefined,
        exitCode: typeof raw.exit_code === "number" ? raw.exit_code : undefined,
        artifact: extractArtifact(content),
        failed: isFailed(typeof raw.success === "boolean" ? raw.success : undefined, content),
        presentation: raw.presentation,
      };
      next.items = [...next.items.slice(0, idx), updated, ...next.items.slice(idx + 1)];
      break;
    }

    case "turn_start":
      next.workState = "waiting";
      next.running = true;
      next.model = String(raw.model ?? raw.model_alias ?? state.model);
      break;

    case "title_update":
      // 轮结束权威统计：覆盖（非累计）——与 usage 事件增量语义区分
      next.model = String(raw.model ?? state.model);
      if (typeof raw.tokens_in === "number") next.tokensIn = raw.tokens_in;
      if (typeof raw.tokens_out === "number") next.tokensOut = raw.tokens_out;
      if (typeof raw.cache_read === "number") next.cacheReadTokens = raw.cache_read;
      if (typeof raw.context_tokens === "number") next.contextTokens = raw.context_tokens;
      if (typeof raw.max_context === "number") next.maxContextTokens = raw.max_context;
      if (typeof raw.cost_micros === "number") next.costMicros = raw.cost_micros;
      if (typeof raw.belief === "number") next.belief = raw.belief;
      break;

    case "turn_final":
      next.workState = "idle";
    case "stop":
      next.workState = "idle";
      next.running = false;
      next.items = [
        ...next.items,
        {
          kind: "system",
          text:
            raw.type === "stop" && raw.reason === "interrupted" ? "— 已中断 —" : "— turn 结束 —",
        },
      ];
      break;

    case "turn_error":
      next.workState = "error";
      next.running = false;
      next.items = [...next.items, withKey({ kind: "error", text: String(raw.error ?? "turn 失败") }, raw)];
      break;

    case "error":
      next.items = [...next.items, withKey({ kind: "error", text: String(raw.message ?? "error") }, raw)];
      break;

    case "usage":
      next.tokensIn += Number(raw.input_tokens ?? 0);
      next.tokensOut += Number(raw.output_tokens ?? 0);
      next.cacheReadTokens += Number(raw.cache_read_input_tokens ?? 0) + Number(raw.cache_creation_input_tokens ?? 0);
      if (raw.context_tokens != null) next.contextTokens = Number(raw.context_tokens);
      if (raw.max_context != null) next.maxContextTokens = Number(raw.max_context);
      break;

    case "signal":
      next.items = [...next.items, withKey({ kind: "signal", text: formatSignal(raw) }, raw)];
      break;

    case "compact":
      next.workState = "compacting";
      next.items = [...next.items, withKey({ kind: "system", text: "上下文压缩" }, raw)];
      break;

    case "retry":
      next.items = [...next.items, withKey({ kind: "system", text: "重试中…" }, raw)];
      break;

    case "sub_agent": {
      next.workState = "sub-agent";
      const sid = String(raw.session_id ?? "").slice(0, 8);
      next.items = [
        ...next.items,
        withKey({ kind: "system", text: `子代理 ${sid} ${String(raw.status ?? "")}` }, raw),
      ];
      break;
    }

    default:
      // tool_surface / tool_capability_resolution / prompt_workflow_resolution /
      // session_start 等系统事件不渲染
      break;
  }
  return next;
}
