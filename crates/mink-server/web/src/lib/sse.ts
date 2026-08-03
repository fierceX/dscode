// SSE 客户端：订阅 runtime 事件广播（纯转手，无 from_seq/重放逻辑）。
// 服务端帧为 data-only（无 event: 头）——统一用 onmessage 解析 type 字段。
// 断线由浏览器 EventSource 自动重连；断线期间丢失的事件（核心已写入
// conversation）由页面上的"重连"按钮手动对账（复用 openSession 重拉）。

import type { RawEvent } from "./types";

const RECONNECT_DELAY_MS = 2000;

export class SseClient {
  private es: EventSource | null = null;
  private sessionId: string;
  private onEvent: (raw: RawEvent) => void;
  private closed = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  /** 实时事件无行号：本地单调递增 seq，保证 each key 唯一（与历史 key 空间分离） */
  private seq = 0;

  constructor(
    sessionId: string,
    onEvent: (raw: RawEvent) => void,
    startSeq = 0,
  ) {
    this.sessionId = sessionId;
    this.onEvent = onEvent;
    // 起始 seq = 会话当前最大 key：重连后 seq 从该值继续，
    // 同会话内实时块 key 全局唯一（不会与旧实时块冲突导致 keyed diff 错乱）
    this.seq = startSeq;
  }

  connect() {
    if (this.es || this.closed) return;
    const url = `/api/sessions/${encodeURIComponent(this.sessionId)}/stream`;
    const es = new EventSource(url);
    this.es = es;

    // data-only 帧：data 为完整 JSON（含 type 字段）
    es.onmessage = (ev: MessageEvent) => {
      const raw = safeParse(ev.data);
      if (!raw.type) return;
      this.seq += 1;
      this.onEvent({ ...raw, type: String(raw.type), seq: this.seq });
    };

    es.onerror = () => {
      es.close();
      this.es = null;
      if (!this.closed) {
        this.reconnectTimer = setTimeout(() => this.connect(), RECONNECT_DELAY_MS);
      }
    };
  }

  close() {
    this.closed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    if (this.es) {
      this.es.close();
      this.es = null;
    }
  }
}

function safeParse(s: string): Record<string, unknown> {
  try {
    return JSON.parse(s) as Record<string, unknown>;
  } catch {
    return {};
  }
}
