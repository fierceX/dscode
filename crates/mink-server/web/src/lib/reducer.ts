// 事件 → 状态 纯函数 reducer。
// 历史重放与 SSE 实时共用；不可变更新，便于测试与 Vue 3 响应式渲染。

import type { RawEvent, SessionState, ToolItem, TranscriptItem, TranscriptKey } from "./types";
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
  return [...items, { ...item, key: seq as TranscriptKey }];
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
function withKey<T extends object>(item: T, raw: RawEvent): T & { key: TranscriptKey } {
  return { ...item, key: eventKey(raw) };
}

function eventKey(raw: RawEvent): TranscriptKey {
  return raw.stream_sequence == null
    ? Number(raw.seq ?? 0)
    : `live:${Number(raw.stream_sequence)}`;
}

function eventSequence(raw: RawEvent): number {
  return Number(raw.stream_sequence ?? raw.seq ?? 0);
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
  const turnId = typeof raw.turn_id === "string" ? raw.turn_id : undefined;
  const coreSequence = typeof raw.sequence === "number" ? raw.sequence : undefined;
  const dedupeKey = turnId != null && coreSequence != null ? `${turnId}:${coreSequence}` : undefined;
  if (dedupeKey && state.seenTurnEvents.includes(dedupeKey)) return state;
  const seenTurnEvents = dedupeKey
    ? [...state.seenTurnEvents, dedupeKey].slice(-4096)
    : state.seenTurnEvents;
  const next: SessionState = {
    ...state,
    lastSeq: Math.max(state.lastSeq, eventSequence(raw)),
    lastTurnId: turnId ?? state.lastTurnId,
    lastCoreSequence: coreSequence ?? state.lastCoreSequence,
    seenTurnEvents,
    items: state.items,
  };

  switch (raw.type) {
    case "user_input":
      next.items = [...next.items, withKey({ kind: "user", text: String(raw.content ?? "") }, raw)];
      break;

    case "thinking":
      next.workState = "thinking";
      next.items = appendStream(next.items, "thinking", String(raw.content ?? ""), eventKey(raw));
      break;

    case "text":
      next.workState = "generating";
      next.items = appendStream(next.items, "text", String(raw.content ?? ""), eventKey(raw));
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
        key: eventKey(raw),
      };
      next.items = [...next.items, item];
      break;
    }

    case "tool_result": {
      // Realtime AgentEvent always carries tool_use_id. Only legacy conversation
      // replay may omit it and fall back to the nearest pending tool.
      const liveRequired = ["tool_use_id", "tool_name", "presentation", "artifacts", "status", "exit_code", "result_kind"];
      if (raw.stream_sequence != null && liveRequired.some((field) => !Object.hasOwn(raw, field))) break;
      if (raw.stream_sequence != null && raw.tool_use_id == null) break;
      const toolUseId = raw.tool_use_id == null ? undefined : String(raw.tool_use_id);
      const idx = lastPendingTool(next.items, toolUseId);
      if (idx < 0) break;
      const item = next.items[idx] as ToolItem;
      const toolName = raw.tool_name == null ? item.name : String(raw.tool_name);
      const content = stripAnsi(String(raw.content ?? ""));
      const status = raw.status as Record<string, unknown> | undefined;
      const succeeded = status?.state === "succeeded";
      const updated: ToolItem = {
        ...item,
        name: toolName,
        result: content,
        resultKind: kindBadge(String(raw.result_kind ?? ""), toolName),
        rawResultKind: raw.result_kind == null ? undefined : String(raw.result_kind),
        success: status == null ? undefined : succeeded,
        exitCode: typeof raw.exit_code === "number" ? raw.exit_code : raw.exit_code === null ? null : undefined,
        artifact: extractArtifact(content),
        failed: isFailed(status == null ? undefined : succeeded, content),
        presentation: raw.presentation,
        artifacts: Array.isArray(raw.artifacts) ? raw.artifacts : undefined,
      };
      next.items = [...next.items.slice(0, idx), updated, ...next.items.slice(idx + 1)];
      break;
    }

    case "turn_started":
      next.workState = "waiting";
      next.running = true;
      break;

    case "title_update": {
      // 轮结束权威统计：覆盖（非累计）——与 usage 事件增量语义区分
      next.model = String(raw.model ?? state.model);
      const stats = (raw.stats ?? {}) as Record<string, unknown>;
      if (typeof stats.total_input_tokens === "number") next.tokensIn = stats.total_input_tokens;
      if (typeof stats.total_output_tokens === "number") next.tokensOut = stats.total_output_tokens;
      if (typeof stats.total_cache_read_tokens === "number") next.cacheReadTokens = stats.total_cache_read_tokens;
      if (typeof stats.current_context_tokens === "number") next.contextTokens = stats.current_context_tokens;
      if (typeof stats.max_context_tokens === "number") next.maxContextTokens = stats.max_context_tokens;
      if (typeof stats.belief === "number") next.belief = stats.belief;
      break;
    }

    case "stop":
      // Stop records why generation ended. Final owns authoritative runtime state.
      next.items = [
        ...next.items,
        withKey({
          kind: "system",
          text: raw.reason === "interrupted" ? "— 已中断 —" : "— turn 结束 —",
        }, raw),
      ];
      break;

    case "turn_final": {
      const outcome = (raw.outcome ?? {}) as Record<string, unknown>;
      const error = typeof outcome.error === "string" ? outcome.error : undefined;
      next.workState = "idle";
      next.running = false;
      if (error) {
        next.workState = "error";
        next.items = [...next.items, withKey({ kind: "error", text: error }, raw)];
      }
      break;
    }

    case "turn_error":
      next.workState = "error";
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

    case "sub_agent_status": {
      next.workState = "sub-agent";
      const sessionId = String(raw.session_id ?? "");
      const idx = next.items.findIndex((item) => item.kind === "sub_agent" && item.sessionId === sessionId);
      const status = String(raw.status ?? "");
      const inTokens = Number(raw.in_tokens ?? 0);
      const outTokens = Number(raw.out_tokens ?? 0);
      if (idx < 0) {
        next.items = [...next.items, withKey({
          kind: "sub_agent" as const,
          sessionId,
          status,
          thinking: "",
          text: "",
          inTokens,
          outTokens,
        }, raw)];
      } else {
        const existing = next.items[idx];
        if (existing.kind !== "sub_agent") break;
        next.items = [
          ...next.items.slice(0, idx),
          { ...existing, status, inTokens, outTokens },
          ...next.items.slice(idx + 1),
        ];
      }
      break;
    }

    case "sub_agent_output": {
      next.workState = "sub-agent";
      const sessionId = String(raw.session_id ?? "");
      const idx = next.items.findIndex((item) => item.kind === "sub_agent" && item.sessionId === sessionId);
      const item = withKey({
        kind: "sub_agent" as const,
        sessionId,
        status: String(raw.status ?? ""),
        thinking: String(raw.thinking ?? ""),
        text: String(raw.text ?? ""),
        inTokens: Number(raw.in_tokens ?? 0),
        outTokens: Number(raw.out_tokens ?? 0),
      }, raw);
      next.items = idx < 0
        ? [...next.items, item]
        : [...next.items.slice(0, idx), { ...next.items[idx], ...item }, ...next.items.slice(idx + 1)];
      break;
    }

    default:
      // tool_surface / tool_capability_resolution / prompt_workflow_resolution /
      // session_start 等系统事件不渲染
      break;
  }
  return next;
}
