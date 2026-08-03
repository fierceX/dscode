// Vue 版状态：模块级 reactive/ref（无 Svelte 5 赋值限制）。
// 组件直接读写属性，无需 setter 包装。

import { reactive, ref } from "vue";
import type { RawEvent, SessionState } from "./types";
import { emptySession } from "./types";
import { reduceEvent, prependEvents } from "./reducer";
import type { SessionSummary } from "./api";
import type { SseClient } from "./sse";

export const uiState = reactive({
  ctxOpen: false,
  ctxTab: "plan" as string,
  fileOpen: false,
});

export const appState = reactive({
  sessions: [] as SessionSummary[],
  currentWorkspace: null as string | null,
  currentSessionId: null as string | null,
  sessionState: null as SessionState | null,
});

export const sseClient = ref<SseClient | null>(null);

const SESSION_KEY = "mink.currentSession";

/** 会话打开/切换：重建状态 + 持久化当前会话（重开浏览器自动恢复并重连 SSE） */
export function attachSession(summary: SessionSummary) {
  appState.currentSessionId = summary.id;
  const st = emptySession(summary.id, summary.title ?? summary.alias ?? summary.id);
  // 历史用量初始化（usage.jsonl 汇总），实时 usage 事件继续累计
  st.tokensIn = summary.tokens_in ?? 0;
  st.tokensOut = summary.tokens_out ?? 0;
  st.cacheReadTokens = summary.cache_read_tokens ?? 0;
  // 当前上下文 = 最近一次请求的上下文（服务端 usage.jsonl 最后记录）；无记录时回退输入合计
  st.contextTokens = summary.last_context_tokens ?? st.tokensIn;
  st.costMicros = Math.round((summary.cost_nano_cny ?? 0) / 1000);
  appState.sessionState = st;
  try { localStorage.setItem(SESSION_KEY, summary.id); } catch { /* 隐私模式等忽略 */ }
}

export function savedSessionId(): string | null {
  try { return localStorage.getItem(SESSION_KEY); } catch { return null; }
}

export function clearSavedSession() {
  try { localStorage.removeItem(SESSION_KEY); } catch { /* ignore */ }
}

export function detachSession() {
  appState.currentSessionId = null;
  appState.sessionState = null;
  sseClient.value?.close();
  sseClient.value = null;
}

/** reducer 入口（SSE 实时与历史重放共用） */
export function applyEvent(raw: { type: string; [k: string]: unknown }) {
  if (appState.sessionState) {
    appState.sessionState = reduceEvent(appState.sessionState, raw);
  }
}

/** 懒加载前插（返回新状态并写回） */
export function prependOlder(raws: RawEvent[]) {
  if (appState.sessionState) {
    appState.sessionState = prependEvents(appState.sessionState, raws);
  }
}

export function workspaces() {
  const by = new Map<string, SessionSummary[]>();
  for (const s of appState.sessions) {
    const cwd = s.cwd || "(unknown)";
    if (!by.has(cwd)) by.set(cwd, []);
    by.get(cwd)!.push(s);
  }
  return [...by.entries()]
    .map(([cwd, list]) => ({ cwd, sessions: list }))
    .sort((a, b) => a.cwd.localeCompare(b.cwd));
}

export function workspaceSessions(cwd: string): SessionSummary[] {
  return appState.sessions
    .filter((s) => (s.cwd || "(unknown)") === cwd)
    .sort((a, b) => (b.modified_secs ?? 0) - (a.modified_secs ?? 0));
}
