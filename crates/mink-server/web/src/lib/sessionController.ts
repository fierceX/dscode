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

const POLL_MS = 500;
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

function invalidateRecovery(summary: SessionSummary, token: number): number {
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
  state.costMicros = Math.round((summary.cost_nano_cny ?? 0) / 1000);
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
  const tail = await api.conversation(summary.id, {
    limit: 20,
    tail: true,
    project: summary.project_key,
  }).catch(() => null);
  if (tail?.code !== 200 || !Array.isArray(tail.data)) return null;
  return tail.data as Record<string, unknown>[];
}

export async function openSession(summary: SessionSummary): Promise<void> {
  const token = ++openToken;
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
  const recover = () => {
    if (!isCurrent(summary, token)) return;
    connected = false;
    invalidateRecovery(summary, token);
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
        recover();
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
  reloadWorker = runReloadWorker(summary, token).finally(() => {
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
    const before = await api.getSession(summary.id, summary.project_key).catch(() => null);
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
    const after = await api.getSession(summary.id, summary.project_key).catch(() => null);
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
  reconcileWorker = (async () => {
    while (isCurrent(summary, token)) {
      const detail = await api.getSession(summary.id, summary.project_key).catch(() => null);
      if (!isCurrent(summary, token)) return;
      if (detail?.code === 200 && detail.data?.running === false) {
        invalidateRecovery(summary, token);
        return;
      }
      await delay(POLL_MS);
    }
  })().finally(() => {
    reconcileWorker = null;
    if (isCurrent(summary, token) && connected) scheduleReload(summary, token);
  });
}

export function closeSessionView(): void {
  ++openToken;
  ++recoveryRevision;
  connected = false;
  reloadWorker = null;
  reconcileWorker = null;
  activeAttempt = null;
  recoveryFinal = null;
  detachSession();
}
