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
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("abandons a permanently pending snapshot after stream_gap", async () => {
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

    // Deliberately leave the first promise unresolved. The abort race must
    // release its worker slot so the newly opened SSE connection can perform
    // a fresh authoritative read instead of remaining desynced forever.
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

  it("aborts the old recovery request when switching sessions", async () => {
    const oldConversation = deferred<{ code: number; message: string; data: unknown[] }>();
    const nextSummary: SessionSummary = {
      ...summary,
      project_key: "project-2",
      id: "session-2",
      alias: "second",
      title: "Second",
      path: "/tmp/session-2",
    };
    let oldSignal: AbortSignal | undefined;
    vi.spyOn(api, "openSession").mockResolvedValue({ code: 200, message: "", data: {} });
    vi.spyOn(api, "getSession").mockImplementation(async (id) => ({
      code: 200, message: "", data: { id, open: true, running: false },
    }));
    vi.spyOn(api, "conversation").mockImplementation((id, opts = {}) => {
      if (id === summary.id) {
        oldSignal = opts.signal;
        return oldConversation.promise;
      }
      return Promise.resolve({ code: 200, message: "", data: [] });
    });
    const controller = await import("./sessionController");

    await controller.openSession(summary);
    FakeEventSource.instances[0].open();
    await vi.waitFor(() => expect(oldSignal).toBeDefined());

    await controller.openSession(nextSummary);
    expect(oldSignal?.aborted).toBe(true);
    FakeEventSource.instances[1].open();
    await vi.waitFor(() => expect(appState.sessionState?.desynced).toBe(false));

    // A late completion from the detached session must neither replace the B
    // snapshot nor clear/schedule work through B's worker ownership slots.
    oldConversation.resolve({
      code: 200,
      message: "",
      data: [{ role: "user", content: "stale session A" }],
    });
    await settle();
    expect(appState.currentSessionId).toBe(nextSummary.id);
    expect(appState.sessionState?.sessionId).toBe(nextSummary.id);
    expect(appState.sessionState?.items).toEqual([]);
  });

  it("times out a stuck recovery read and retries through reconcile", async () => {
    vi.useFakeTimers();
    const stuckConversation = deferred<{ code: number; message: string; data: unknown[] }>();
    vi.spyOn(api, "openSession").mockResolvedValue({ code: 200, message: "", data: {} });
    vi.spyOn(api, "getSession").mockResolvedValue({
      code: 200, message: "", data: { id: summary.id, open: true, running: false },
    });
    vi.spyOn(api, "conversation")
      .mockImplementationOnce(() => stuckConversation.promise)
      .mockResolvedValue({ code: 200, message: "", data: [] });
    const controller = await import("./sessionController");

    await controller.openSession(summary);
    FakeEventSource.instances[0].open();
    await settle();
    expect(appState.sessionState?.desynced).toBe(true);

    await vi.advanceTimersByTimeAsync(15_000);
    await settle();
    expect(api.conversation).toHaveBeenCalledTimes(2);
    expect(appState.sessionState?.desynced).toBe(false);
  });
});
