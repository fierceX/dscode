// 事件与状态类型：与核心 events.jsonl 协议对齐。

export interface RawEvent {
  type: string;
  seq?: number;
  [key: string]: unknown;
}

export type ToolColor = "exec" | "file" | "search" | "todo" | "plan" | "delegate" | "tool";
export type ResultView = "command" | "file" | "search" | "todo" | "plan" | "diff" | "text";

export interface ThinkingItem { key?: number; kind: "thinking"; text: string; }
export interface TextItem { key?: number; kind: "text"; text: string; }
export interface UserItem { key?: number; kind: "user"; text: string; }
export interface ErrorItem { key?: number; kind: "error"; text: string; }
export interface SignalItem { key?: number; kind: "signal"; text: string; }
export interface SystemItem { key?: number; kind: "system"; text: string; }

export interface ToolItem {
  key?: number;
  kind: "tool";
  id: string;
  name: string;
  color: ToolColor;
  view: ResultView;
  summary: string;
  input: string;
  result?: string;
  resultKind?: string;
  success?: boolean;
  exitCode?: number;
  artifact?: string | null;
  failed?: boolean;
  presentation?: unknown;
}

export type TranscriptItem = ThinkingItem | TextItem | UserItem | ToolItem | ErrorItem | SignalItem | SystemItem;

export interface SessionState {
  sessionId: string;
  title: string;
  running: boolean;
  lastSeq: number;
  model: string;
  tokensIn: number;
  tokensOut: number;
  /** 费用（微美元）与信念度——来自 title_update 实时统计 */
  costMicros: number;
  belief: number;
  /** Agent 工作状态（由事件推导）：idle/waiting/thinking/generating/tool/sub-agent/compacting/error */
  workState: string;
  /** 缓存命中 tokens（usage 事件累计）与当前上下文估计 */
  cacheReadTokens: number;
  contextTokens: number;
  maxContextTokens: number;
  items: TranscriptItem[];
}

export function emptySession(sessionId: string, title: string): SessionState {
  return {
    sessionId, title, running: false, lastSeq: 0, model: "",
    tokensIn: 0, tokensOut: 0, costMicros: 0, belief: 0, workState: "idle", cacheReadTokens: 0, contextTokens: 0, maxContextTokens: 0, items: [],
  };
}
