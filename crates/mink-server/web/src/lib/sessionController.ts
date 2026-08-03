// 会话打开/关闭编排：attach → conversation 完整轮次加载 → SSE 实时。

import { api } from "./api";
import type { SessionSummary } from "./api";
import { appState, applyEvent, attachSession, detachSession, prependOlder, sseClient } from "./store";
import { conversationToEvents } from "./toolFormat";
import { SseClient } from "./sse";

// 单调递增 token：快速切换会话时，旧会话的异步回调（open/conversation/SSE）
// 必须校验 token 与当前会话一致，否则丢弃（防止 A 的历史污染 B 的状态）。
let openToken = 0;

export async function openSession(summary: SessionSummary): Promise<void> {
  const token = ++openToken;
  const current = () =>
    token === openToken && appState.currentSessionId === summary.id;

  detachSession();
  attachSession(summary);

  const opened = await api.openSession(summary.id);
  if (opened.code !== 200) {
    if (current()) throw new Error(opened.message || "打开失败");
    return;
  }
  if (!current()) return;

  // 重开感知：服务端 turn 可能正在进行（前端关闭不影响）——查询并标记 running
  const detail = await api.getSession(summary.id);
  if (current() && detail.code === 200 && detail.data?.running && appState.sessionState) {
    appState.sessionState.running = true;
  }

  // 历史加载主源：conversation.jsonl（完整轮次，一轮含 thinking/text/工具调用）。
  // tail 最近 20 轮；工具卡片不足 3 张时继续往前加载（最多 20 批 × 20 轮）。
  let earliestSeq = 0;
  const toolCount = () =>
    appState.sessionState?.items.filter((i) => i.kind === "tool").length ?? 0;

  const applyConv = (rows: Record<string, unknown>[]) => {
    if (!current()) return;
    for (const row of rows) {
      const seq = Number(row.seq ?? 0);
      if (!earliestSeq || (seq && seq < earliestSeq)) earliestSeq = seq;
      for (const ev of conversationToEvents(row)) applyEvent(ev);
    }
  };

  const tail = await api.conversation(summary.id, { limit: 20, tail: true });
  if (tail.code === 200 && Array.isArray(tail.data)) {
    applyConv(tail.data as Record<string, unknown>[]);
  }
  for (let i = 0; i < 20 && current(); i++) {
    if (toolCount() >= 3) break;
    if (earliestSeq <= 1) break;
    const older = await api.conversation(summary.id, { limit: 20, beforeSeq: earliestSeq });
    if (older.code !== 200 || !Array.isArray(older.data) || older.data.length === 0) break;
    const rows = older.data as Record<string, unknown>[];
    const seqs = rows.map((r) => Number(r.seq ?? 0));
    const newEarliest = seqs.length ? Math.min(...seqs) : earliestSeq;
    const converted = rows.flatMap(conversationToEvents);
    if (current()) prependOlder(converted as never);
    earliestSeq = newEarliest;
  }
  if (!current()) return;

  // SSE：订阅 runtime 广播（纯转手，无 from_seq）。
  // 起始 seq = 会话当前最大 key（历史 key=行号*100 更大，实时从该值继续，
  // 同会话 key 全局唯一——重连不冲突）
  const maxKey =
    appState.sessionState?.items.reduce(
      (m, it) => Math.max(m, (it as { key?: number }).key ?? 0),
      0,
    ) ?? 0;
  // 历史重放结束：非运行中会话强制空闲（conversation 末尾可能停在 text/tool 事件）
  if (current() && appState.sessionState && !appState.sessionState.running) {
    appState.sessionState.workState = "idle";
  }
  const client = new SseClient(
    summary.id,
    (raw) => { if (current()) applyEvent(raw); },
    maxKey,
  );
  client.connect();
  sseClient.value = client;
}

export function closeSessionView(): void {
  detachSession();
}

