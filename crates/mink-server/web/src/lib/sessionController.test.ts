import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, type SessionSummary } from "./api";
import { appState } from "./store";

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  constructor(public url: string) { FakeEventSource.instances.push(this); }
  close() { this.closed = true; }
  open() { this.onopen?.(); }
  emit(raw: Record<string, unknown>) {
    this.onmessage?.({ data: JSON.stringify(raw) } as MessageEvent);
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

const summary: SessionSummary = {
  project_key: "project",
  corrupt: false,
  id: "session-1",
  alias: "session",
  title: "Session",
  cwd: "/tmp/project",
  created_at: "",
  updated_at: "",
  modified_secs: 0,
  status: "active",
  path: "/tmp/session-1",
};

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

describe("session authoritative recovery", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    FakeEventSource.instances = [];
    vi.stubGlobal("EventSource", FakeEventSource);
  });

  afterEach(async () => {
    const controller = await import("./sessionController");
    controller.closeSessionView();
    vi.unstubAllGlobals();
  });

  it("discards a conversation snapshot invalidated by stream_gap", async () => {
    const firstConversation = deferred<{ code: number; message: string; data: unknown[] }>();
    vi.spyOn(api, "openSession").mockResolvedValue({ code: 200, message: "", data: {} });
    vi.spyOn(api, "getSession").mockResolvedValue({
      code: 200, message: "", data: { id: summary.id, open: true, running: false },
    });
    vi.spyOn(api, "conversation")
      .mockImplementationOnce(() => firstConversation.promise)
      .mockResolvedValue({ code: 200, message: "", data: [] });
    const controller = await import("./sessionController");

    await controller.openSession(summary);
    FakeEventSource.instances[0].open();
    await settle();
    FakeEventSource.instances[0].emit({ type: "stream_gap", missed: 1 });
    expect(appState.sessionState?.desynced).toBe(true);
    expect(FakeEventSource.instances).toHaveLength(2);

    firstConversation.resolve({ code: 200, message: "", data: [] });
    await settle();
    expect(appState.sessionState?.desynced).toBe(true);

    FakeEventSource.instances[1].open();
    await vi.waitFor(() => {
      expect(api.conversation).toHaveBeenCalledTimes(2);
      expect(appState.sessionState?.desynced).toBe(false);
    });
  });

  it("keeps a failed final desynced until the new authoritative read commits", async () => {
    const recoveryConversation = deferred<{ code: number; message: string; data: unknown[] }>();
    vi.spyOn(api, "openSession").mockResolvedValue({ code: 200, message: "", data: {} });
    vi.spyOn(api, "getSession").mockResolvedValue({
      code: 200, message: "", data: { id: summary.id, open: true, running: false },
    });
    vi.spyOn(api, "conversation")
      .mockResolvedValueOnce({ code: 200, message: "", data: [] })
      .mockImplementationOnce(() => recoveryConversation.promise);
    const controller = await import("./sessionController");

    await controller.openSession(summary);
    FakeEventSource.instances[0].open();
    await vi.waitFor(() => expect(appState.sessionState?.desynced).toBe(false));

    FakeEventSource.instances[0].emit({
      type: "turn_final",
      outcome: { error: "failed", status: "failed" },
    });
    await settle();
    expect(appState.sessionState?.desynced).toBe(true);

    recoveryConversation.resolve({ code: 200, message: "", data: [] });
    await vi.waitFor(() => expect(appState.sessionState?.desynced).toBe(false));
  });
});
