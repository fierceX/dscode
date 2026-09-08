// 会话打开/关闭编排：SSE 只承载增量事件，conversation 是断线后的权威恢复源。

import { api } from "./api";
import type { SessionSummary } from "./api";
import { appState, attachSession, detachSession, sseClient } from "./store";
import { reduceEvent } from "./reducer";
import { conversationToEvents } from "./toolFormat";
import { emptySession, type RawEvent, type SessionState } from "./types";
import { SseClient } from "./sse";

let openToken = 0;
let recoveryRevision = 0;
let reloadWorker: Promise<void> | null = null;
let reconcileWorker: Promise<void> | null = null;
let liveGeneration = 0;
let connected = false;
let activeAttempt: { revision: number; events: RawEvent[] } | null = null;
let recoveryFinal: RawEvent | null = null;
const recoveryRequests = new Set<AbortController>();

const POLL_MS = 500;
const RECOVERY_REQUEST_TIMEOUT_MS = 15_000;
const LIFECYCLE_EVENTS = new Set(["turn_started", "turn_final"]);

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isCurrent(summary: SessionSummary, token: number): boolean {
  return token === openToken
    && appState.currentSessionId === summary.id
    && appState.currentProjectKey === summary.project_key;
}

function setDesynced(summary: SessionSummary, token: number): void {
  if (isCurrent(summary, token) && appState.sessionState) {
    appState.sessionState.desynced = true;
  }
}

function abortRecoveryRequests(): void {
  // Recovery reads are disposable snapshots. Once their revision or attached
  // session is no longer current, allowing them to remain pending would block
  // the single worker slot and prevent a reconnect from reaching authority.
  for (const controller of recoveryRequests) controller.abort();
  recoveryRequests.clear();
}

async function recoveryRequest<T>(request: (signal: AbortSignal) => Promise<T>): Promise<T | null> {
  const controller = new AbortController();
  recoveryRequests.add(controller);
  const aborted = new Promise<null>((resolve) => {
    controller.signal.addEventListener("abort", () => resolve(null), { once: true });
  });
  const timeout = setTimeout(() => controller.abort(), RECOVERY_REQUEST_TIMEOUT_MS);
  try {
    // The abort branch also releases the worker if a mocked or non-standard
    // request ignores AbortSignal. Native fetch still receives the signal and
    // stops its underlying network work through api.ts.
    return await Promise.race([
      request(controller.signal).catch(() => null),
      aborted,
    ]);
  } finally {
    clearTimeout(timeout);
    recoveryRequests.delete(controller);
  }
}

function invalidateRecovery(summary: SessionSummary, token: number): number {
  abortRecoveryRequests();
  const revision = ++recoveryRevision;
  activeAttempt = null;
  setDesynced(summary, token);
  return revision;
}

function outcomeError(raw: RawEvent): boolean {
  if (raw.type !== "turn_final" || typeof raw.outcome !== "object" || raw.outcome === null) return false;
  return typeof (raw.outcome as Record<string, unknown>).error === "string";
}

function applyLive(raw: RawEvent): void {
  if (appState.sessionState) appState.sessionState = reduceEvent(appState.sessionState, raw);
}

function seedState(summary: SessionSummary): SessionState {
  const state = emptySession(summary.id, summary.title ?? summary.alias ?? summary.id);
  state.tokensIn = summary.tokens_in ?? 0;
  state.tokensOut = summary.tokens_out ?? 0;
  state.cacheReadTokens = summary.cache_read_tokens ?? 0;
  state.contextTokens = summary.last_context_tokens ?? state.tokensIn;
  state.desynced = true;
  return state;
}

function reduceConversation(summary: SessionSummary, rows: Record<string, unknown>[]): SessionState {
  let state = seedState(summary);
  for (const row of rows) {
    for (const event of conversationToEvents(row)) state = reduceEvent(state, event);
  }
  state.running = false;
  state.workState = "idle";
  return state;
}

async function loadConversation(summary: SessionSummary): Promise<Record<string, unknown>[] | null> {
  const tail = await recoveryRequest((signal) => api.conversation(summary.id, {
    limit: 20,
    tail: true,
    project: summary.project_key,
    signal,
  }));
  if (tail?.code !== 200 || !Array.isArray(tail.data)) return null;
  return tail.data as Record<string, unknown>[];
}

export async function openSession(summary: SessionSummary): Promise<void> {
  const token = ++openToken;
  abortRecoveryRequests();
  ++recoveryRevision;
  reloadWorker = null;
  reconcileWorker = null;
  activeAttempt = null;
  recoveryFinal = null;
  liveGeneration = 0;
  connected = false;

  detachSession();
  attachSession(summary);
  setDesynced(summary, token);

  const opened = await api.openSession(summary.id, summary.project_key);
  if (opened.code !== 200) {
    if (isCurrent(summary, token)) throw new Error(opened.message || "打开失败");
    return;
  }
  if (!isCurrent(summary, token)) return;

  let client: SseClient;
  const reconnectNow = () => {
    if (!isCurrent(summary, token)) return;
    connected = false;
    invalidateRecovery(summary, token);
    client.reconnect();
  };
  const recover = async () => {
    if (!isCurrent(summary, token)) return;
    connected = false;
    invalidateRecovery(summary, token);
    const detail = await recoveryRequest((signal) =>
      api.getSession(summary.id, summary.project_key, signal));
    if (!isCurrent(summary, token)) return;
    // Session deleted (404) or explicitly closed: stop the SSE reconnect loop.
    // The UI remains desynced and the user can reopen the session explicitly.
    if (detail?.code === 404
      || (detail?.code === 200 && (detail.data as { open?: boolean } | null)?.open === false)) {
      client.close();
      return;
    }
    client.reconnect();
  };
  const onOpen = () => {
    if (!isCurrent(summary, token)) return;
    connected = true;
    invalidateRecovery(summary, token);
    scheduleReload(summary, token);
  };
  client = new SseClient(
    summary.id,
    (raw) => {
      if (!isCurrent(summary, token)) return;
      if (raw.type === "stream_gap") {
        reconnectNow();
        return;
      }
      if (LIFECYCLE_EVENTS.has(raw.type)) liveGeneration++;
      activeAttempt?.events.push(raw);
      applyLive(raw);
      if (outcomeError(raw)) {
        recoveryFinal = raw;
        invalidateRecovery(summary, token);
      }
      if (raw.type === "turn_final" && appState.sessionState?.desynced) {
        scheduleReload(summary, token);
      }
    },
    recover,
    summary.project_key,
    onOpen,
  );
  sseClient.value = client;
  client.connect();
}

function scheduleReload(summary: SessionSummary, token: number): void {
  if (!isCurrent(summary, token) || !connected || reloadWorker) return;
  const worker = runReloadWorker(summary, token);
  reloadWorker = worker;
  void worker.finally(() => {
    // A session switch may install a newer worker while this promise is still
    // unwinding. Only the worker that still owns the slot may clear it or
    // schedule follow-up work for the current session.
    if (reloadWorker !== worker) return;
    reloadWorker = null;
    if (isCurrent(summary, token) && connected && appState.sessionState?.desynced && !reconcileWorker) {
      queueMicrotask(() => scheduleReload(summary, token));
    }
  });
}

async function runReloadWorker(summary: SessionSummary, token: number): Promise<void> {
  while (isCurrent(summary, token) && connected) {
    const revision = recoveryRevision;
    setDesynced(summary, token);
    const before = await recoveryRequest((signal) =>
      api.getSession(summary.id, summary.project_key, signal));
    if (!isCurrent(summary, token) || !connected) return;
    if (revision !== recoveryRevision) continue;
    if (before?.code !== 200 || !before.data || before.data.running) {
      scheduleReconcile(summary, token);
      return;
    }

    const generation = liveGeneration;
    const attempt = { revision, events: [] as RawEvent[] };
    activeAttempt = attempt;
    const conversation = await loadConversation(summary);
    const after = await recoveryRequest((signal) =>
      api.getSession(summary.id, summary.project_key, signal));
    if (activeAttempt === attempt) activeAttempt = null;
    if (!isCurrent(summary, token) || !connected) return;

    const stable = revision === recoveryRevision
      && attempt.revision === recoveryRevision
      && before.data.running === false
      && after?.code === 200
      && after.data?.running === false
      && generation === liveGeneration
      && !attempt.events.some((event) => LIFECYCLE_EVENTS.has(event.type));
    if (!conversation || !stable) {
      if (revision !== recoveryRevision) continue;
      scheduleReconcile(summary, token);
      return;
    }

    let state = reduceConversation(summary, conversation);
    for (const event of attempt.events) state = reduceEvent(state, event);
    if (recoveryFinal && !attempt.events.includes(recoveryFinal)) state = reduceEvent(state, recoveryFinal);
    state.desynced = false;
    appState.sessionState = state;
    recoveryFinal = null;
    return;
  }
}

function scheduleReconcile(summary: SessionSummary, token: number): void {
  if (reconcileWorker || !isCurrent(summary, token)) return;
  const worker = (async () => {
    while (isCurrent(summary, token)) {
      const detail = await recoveryRequest((signal) =>
        api.getSession(summary.id, summary.project_key, signal));
      if (!isCurrent(summary, token)) return;
      if (detail?.code === 200 && detail.data?.running === false) {
        invalidateRecovery(summary, token);
        return;
      }
      await delay(POLL_MS);
    }
  })();
  reconcileWorker = worker;
  void worker.finally(() => {
    if (reconcileWorker !== worker) return;
    reconcileWorker = null;
    if (isCurrent(summary, token) && connected) scheduleReload(summary, token);
  });
}

export function closeSessionView(): void {
  ++openToken;
  abortRecoveryRequests();
  ++recoveryRevision;
  connected = false;
  reloadWorker = null;
  reconcileWorker = null;
  activeAttempt = null;
  recoveryFinal = null;
  detachSession();
}
