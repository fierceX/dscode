// REST 客户端：统一 ApiResponse 信封。

export interface ApiResponse<T = unknown> {
  code: number;
  message: string;
  data: T;
}

export interface SessionSummary {
  id: string;
  alias: string | null;
  title: string | null;
  cwd: string;
  created_at: string;
  updated_at: string;
  modified_secs: number | null;
  status: "free" | "active" | "running";
  path: string;
  /** usage.jsonl 汇总（无记录时为 0/缺省） */
  tokens_in?: number;
  tokens_out?: number;
  cache_read_tokens?: number;
  cost_nano_cny?: number;
  last_context_tokens?: number;
}

export interface FileItem {
  name: string;
  dir: boolean;
}

async function request<T>(path: string, options: RequestInit = {}): Promise<ApiResponse<T>> {
  const resp = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  const body = (await resp.json().catch(() => ({}))) as ApiResponse<T>;
  if (resp.status >= 400 && !body.code) {
    throw new Error(`HTTP ${resp.status}`);
  }
  return body;
}

export const api = {
  listSessions: () => request<SessionSummary[]>("/api/sessions"),
  createSession: (name: string, cwd: string) =>
    request<SessionSummary>("/api/sessions", {
      method: "POST",
      body: JSON.stringify({ name, cwd }),
    }),
  getSession: (id: string) =>
    request<{ id: string; open: boolean; running: boolean }>(
      `/api/sessions/${encodeURIComponent(id)}`,
    ),
  openSession: (id: string) =>
    request(`/api/sessions/${encodeURIComponent(id)}/open`, { method: "POST" }),
  deleteSession: (id: string) =>
    request(`/api/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }),
  sendTurn: (id: string, input: string) =>
    request(`/api/sessions/${encodeURIComponent(id)}/turn`, {
      method: "POST",
      body: JSON.stringify({ input }),
    }),
  interrupt: (id: string) =>
    request(`/api/sessions/${encodeURIComponent(id)}/interrupt`, { method: "POST" }),
  /** conversation.jsonl 完整轮次分页（历史展示主源） */
  conversation: (id: string, opts: { limit?: number; tail?: boolean; beforeSeq?: number } = {}) => {
    const params = new URLSearchParams({ limit: String(opts.limit ?? 20) });
    if (opts.tail) params.set("tail", "true");
    if (opts.beforeSeq) params.set("before_seq", String(opts.beforeSeq));
    return request<unknown[]>(`/api/sessions/${encodeURIComponent(id)}/conversation?${params}`);
  },
  plan: (id: string) =>
    request<{ plan: string | null; draft: string | null }>(
      `/api/sessions/${encodeURIComponent(id)}/plan`,
    ),
  todo: (id: string) =>
    request<{ todos: unknown }>(`/api/sessions/${encodeURIComponent(id)}/todo`),
  artifacts: (id: string) =>
    request<{ artifacts: { id: string; tool?: string }[] }>(
      `/api/sessions/${encodeURIComponent(id)}/artifacts`,
    ),
  artifact: (id: string, name: string) =>
    request<{ name: string; content: string }>(
      `/api/sessions/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(name)}`,
    ),
  files: (id: string, path: string, raw = false) =>
    request<{ items?: FileItem[]; content?: string; path?: string }>(
      `/api/sessions/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}${raw ? "&raw=true" : ""}`,
    ),
};
