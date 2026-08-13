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
}

describe("SseClient", () => {
  afterEach(() => {
    FakeEventSource.instances = [];
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
});
