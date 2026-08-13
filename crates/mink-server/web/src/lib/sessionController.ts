// 会话打开/关闭编排：先连接 SSE，再原地用 conversation 权威对账。

import { api } from "./api";
import type { SessionSummary } from "./api";
import { appState, attachSession, detachSession, sseClient } from "./store";
import { reduceEvent } from "./reducer";
import { conversationToEvents } from "./toolFormat";
import { emptySession, type RawEvent, type SessionState } from "./types";
import { SseClient } from "./sse";

let openToken = 0;
let reconcileToken = 0;
let reloadPromise: Promise<void> | null = null;
let liveGeneration = 0;
let reloadBuffer: RawEvent[] | null = null;
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

function outcomeError(raw: RawEvent): boolean {
  if (raw.type !== "turn_final" || typeof raw.outcome !== "object" || raw.outcome === null) return false;
  return typeof (raw.outcome as Record<string, unknown>).error === "string";
}

function applyLive(raw: RawEvent): void {
  if (!appState.sessionState) return;
  appState.sessionState = reduceEvent(appState.sessionState, raw);
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
    for (const event of conversationToEvents(row)) {
      state = reduceEvent(state, event);
    }
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
  ++reconcileToken;
  reloadPromise = null;
  reloadBuffer = null;
  recoveryFinal = null;
  liveGeneration = 0;

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
    setDesynced(summary, token);
    client.reconnect();
  };
  const onOpen = () => {
    if (isCurrent(summary, token)) void authoritativeReload(summary, token);
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
      if (reloadBuffer) reloadBuffer.push(raw);
      applyLive(raw);
      if (outcomeError(raw)) {
        recoveryFinal = raw;
        setDesynced(summary, token);
      }
      if (raw.type === "turn_final" && appState.sessionState?.desynced) {
        void authoritativeReload(summary, token);
      }
    },
    recover,
    summary.project_key,
    onOpen,
  );
  sseClient.value = client;
  client.connect();
}

async function authoritativeReload(summary: SessionSummary, token: number): Promise<void> {
  if (!isCurrent(summary, token)) return;
  if (reloadPromise) return reloadPromise;
  reloadPromise = (async () => {
    setDesynced(summary, token);
    const before = await api.getSession(summary.id, summary.project_key).catch(() => null);
    if (!isCurrent(summary, token)) return;
    if (before?.code !== 200 || !before.data || before.data.running) {
      void reconcileSession(summary, token);
      return;
    }

    const generation = liveGeneration;
    const buffer: RawEvent[] = [];
    reloadBuffer = buffer;
    const conversation = await loadConversation(summary);
    const after = await api.getSession(summary.id, summary.project_key).catch(() => null);
    if (!isCurrent(summary, token)) return;
    reloadBuffer = null;

    const stable = before.data.running === false
      && after?.code === 200
      && after.data?.running === false
      && generation === liveGeneration
      && !buffer.some((event) => LIFECYCLE_EVENTS.has(event.type));
    if (!conversation || !stable) {
      void reconcileSession(summary, token);
      return;
    }

    let state = reduceConversation(summary, conversation);
    for (const event of buffer) state = reduceEvent(state, event);
    if (recoveryFinal && !buffer.includes(recoveryFinal)) state = reduceEvent(state, recoveryFinal);
    state.desynced = false;
    appState.sessionState = state;
    recoveryFinal = null;
  })().finally(() => {
    reloadBuffer = null;
    reloadPromise = null;
  });
  return reloadPromise;
}

async function reconcileSession(summary: SessionSummary, expectedOpenToken: number): Promise<void> {
  const token = ++reconcileToken;
  while (token === reconcileToken && isCurrent(summary, expectedOpenToken)) {
    const detail = await api.getSession(summary.id, summary.project_key).catch(() => null);
    if (token !== reconcileToken || !isCurrent(summary, expectedOpenToken)) return;
    if (detail?.code === 200 && detail.data?.running === false) {
      await authoritativeReload(summary, expectedOpenToken);
      return;
    }
    await delay(POLL_MS);
  }
}

export function closeSessionView(): void {
  ++openToken;
  ++reconcileToken;
  reloadPromise = null;
  reloadBuffer = null;
  recoveryFinal = null;
  detachSession();
}
