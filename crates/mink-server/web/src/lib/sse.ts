// SSE 客户端：订阅 runtime 事件广播（纯转手，无 from_seq/重放逻辑）。
// 服务端帧为 data-only（无 event: 头）——统一用 onmessage 解析 type 字段。
// 断线必须由 controller 显式进入 desynced 并做权威 conversation 对账；
// 不依赖 EventSource 的静默自动重连。

import type { RawEvent } from "./types";

export class SseClient {
  private es: EventSource | null = null;
  private sessionId: string;
  private onEvent: (raw: RawEvent) => void;
  private onDisconnect: () => void;
  private onOpen: () => void;
  private closed = false;
  private project?: string;

  constructor(
    sessionId: string,
    onEvent: (raw: RawEvent) => void,
    onDisconnect: () => void,
    project?: string,
    onOpen: () => void = () => {},
  ) {
    this.sessionId = sessionId;
    this.onEvent = onEvent;
    this.onDisconnect = onDisconnect;
    this.project = project;
    this.onOpen = onOpen;
  }

  connect() {
    if (this.es || this.closed) return;
    const base = `/api/sessions/${encodeURIComponent(this.sessionId)}/stream`;
    const url = this.project ? `${base}?project=${encodeURIComponent(this.project)}` : base;
    const es = new EventSource(url);
    this.es = es;

    es.onopen = () => this.onOpen();

    // data-only 帧：data 为完整 JSON（含 type 字段）
    es.onmessage = (ev: MessageEvent) => {
      const raw = safeParse(ev.data);
      if (!raw.type) return;
      this.onEvent({ ...raw, type: String(raw.type) });
    };

    es.onerror = () => {
      es.close();
      this.es = null;
      if (!this.closed) this.onDisconnect();
    };
  }

  reconnect() {
    if (this.closed) return;
    this.es?.close();
    this.es = null;
    this.connect();
  }

  close() {
    this.closed = true;
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
