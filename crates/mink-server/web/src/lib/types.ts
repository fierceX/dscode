// 事件与状态类型：与核心 events.jsonl 协议对齐。

export interface RawEvent {
  type: string;
  seq?: number;
  /** Core turn-local diagnostic sequence. */
  sequence?: number;
  /** Server-wide live stream sequence; the authoritative realtime UI key. */
  stream_sequence?: number;
  [key: string]: unknown;
}

export type TranscriptKey = number | `live:${number}`;

export type ToolColor = "exec" | "file" | "search" | "todo" | "plan" | "delegate" | "tool";
export type ResultView = "command" | "file" | "search" | "todo" | "plan" | "diff" | "text";

export interface ThinkingItem { key?: TranscriptKey; kind: "thinking"; text: string; }
export interface TextItem { key?: TranscriptKey; kind: "text"; text: string; }
export interface UserItem { key?: TranscriptKey; kind: "user"; text: string; }
export interface ErrorItem { key?: TranscriptKey; kind: "error"; text: string; }
export interface SignalItem { key?: TranscriptKey; kind: "signal"; text: string; }
export interface SystemItem { key?: TranscriptKey; kind: "system"; text: string; }

export interface ToolItem {
  key?: TranscriptKey;
  kind: "tool";
  id: string;
  name: string;
  color: ToolColor;
  view: ResultView;
  summary: string;
  input: string;
  result?: string;
  resultKind?: string;
  rawResultKind?: string;
  success?: boolean;
  exitCode?: number | null;
  artifact?: string | null;
  failed?: boolean;
  presentation?: unknown;
  artifacts?: unknown[];
}

export interface SubAgentItem {
  key?: TranscriptKey;
  kind: "sub_agent";
  sessionId: string;
  status: string;
  thinking: string;
  text: string;
  inTokens: number;
  outTokens: number;
}

export type TranscriptItem = ThinkingItem | TextItem | UserItem | ToolItem | SubAgentItem | ErrorItem | SignalItem | SystemItem;

export interface SessionState {
  sessionId: string;
  title: string;
  running: boolean;
  /** Live stream may have missed events; input stays disabled until reload succeeds. */
  desynced: boolean;
  lastSeq: number;
  /** Last core turn-local envelope coordinates, for diagnostics. */
  lastTurnId?: string;
  lastCoreSequence?: number;
  /** Bounded turn_id+sequence keys used to reject duplicate live delivery. */
  seenTurnEvents: string[];
  model: string;
  tokensIn: number;
  tokensOut: number;
  /** 费用（微人民币）与未计价请求数——来自 title_update 实时统计 */
  costMicros: number;
  unpricedRequests: number;
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
    sessionId, title, running: false, desynced: false, lastSeq: 0, seenTurnEvents: [], model: "",
    tokensIn: 0, tokensOut: 0, costMicros: 0, unpricedRequests: 0, belief: 0, workState: "idle", cacheReadTokens: 0, contextTokens: 0, maxContextTokens: 0, items: [],
  };
}
