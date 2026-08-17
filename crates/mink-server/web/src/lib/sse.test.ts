import { afterEach, describe, expect, it, vi } from "vitest";
import { SseClient } from "./sse";

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  close() {
    this.closed = true;
  }

  emit(raw: Record<string, unknown>) {
    this.onmessage?.({ data: JSON.stringify(raw) } as MessageEvent);
  }
}

describe("SseClient", () => {
  afterEach(() => {
    FakeEventSource.instances = [];
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("断线后只通知 controller，不静默自动创建新连接", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const onDisconnect = vi.fn();
    const client = new SseClient("s/1", vi.fn(), onDisconnect, "project key");
    client.connect();
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.instances[0].url).toContain("s%2F1/stream?project=project%20key");

    FakeEventSource.instances[0].onerror?.();
    expect(FakeEventSource.instances[0].closed).toBe(true);
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(onDisconnect).toHaveBeenCalledOnce();

    client.reconnect();
    expect(FakeEventSource.instances).toHaveLength(2);
  });

  it("reconnect 使用指数退避，不会无限紧密重连", () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", FakeEventSource);
    const client = new SseClient("s/1", vi.fn(), vi.fn());
    client.connect();

    FakeEventSource.instances[0].onerror?.();
    client.reconnect();
    expect(FakeEventSource.instances).toHaveLength(2);

    FakeEventSource.instances[1].onerror?.();
    client.reconnect();
    // 第二次重连不应立即创建连接，而是等待退避。
    expect(FakeEventSource.instances).toHaveLength(2);

    vi.advanceTimersByTime(500);
    expect(FakeEventSource.instances).toHaveLength(3);
  });

  it("收到 session_closed 后停止自动重连", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const onDisconnect = vi.fn();
    const client = new SseClient("s/1", vi.fn(), onDisconnect);
    client.connect();

    FakeEventSource.instances[0].emit({ type: "session_closed" });
    expect(FakeEventSource.instances[0].closed).toBe(true);
    expect(onDisconnect).toHaveBeenCalledOnce();

    client.reconnect();
    expect(FakeEventSource.instances).toHaveLength(1);
  });
});
