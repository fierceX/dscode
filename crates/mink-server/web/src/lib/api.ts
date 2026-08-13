// REST 客户端：统一 ApiResponse 信封。

export interface ApiResponse<T = unknown> {
  code: number;
  message: string;
  data: T;
}

export interface SessionSummary {
  project_key: string;
  corrupt: boolean;
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
  getSession: (id: string, project?: string, signal?: AbortSignal) =>
    request<{ id: string; open: boolean; running: boolean }>(
      sessionUrl(id, "", project),
      { signal },
    ),
  openSession: (id: string, project?: string) =>
    request(sessionUrl(id, "/open", project), { method: "POST" }),
  deleteSession: (id: string, project?: string) =>
    request(sessionUrl(id, "", project), { method: "DELETE" }),
  sendTurn: (id: string, input: string, project?: string) =>
    request(sessionUrl(id, "/turn", project), {
      method: "POST",
      body: JSON.stringify({ input }),
    }),
  interrupt: (id: string, project?: string) =>
    request(sessionUrl(id, "/interrupt", project), { method: "POST" }),
  /** conversation.jsonl 完整轮次分页（历史展示主源） */
  conversation: (id: string, opts: { limit?: number; tail?: boolean; beforeSeq?: number; project?: string; signal?: AbortSignal } = {}) => {
    const params = new URLSearchParams({ limit: String(opts.limit ?? 20) });
    if (opts.project) params.set("project", opts.project);
    if (opts.tail) params.set("tail", "true");
    if (opts.beforeSeq) params.set("before_seq", String(opts.beforeSeq));
    return request<unknown[]>(`/api/sessions/${encodeURIComponent(id)}/conversation?${params}`, {
      signal: opts.signal,
    });
  },
  plan: (id: string, project?: string) =>
    request<{ plan: string | null; draft: string | null }>(
      sessionUrl(id, "/plan", project),
    ),
  todo: (id: string, project?: string) =>
    request<{ todos: unknown }>(sessionUrl(id, "/todo", project)),
  artifacts: (id: string, project?: string) =>
    request<{ artifacts: { id: string; tool?: string }[] }>(
      sessionUrl(id, "/artifacts", project),
    ),
  artifact: (id: string, name: string, project?: string) =>
    request<{ name: string; content: string }>(
      sessionUrl(id, `/artifacts/${encodeURIComponent(name)}`, project),
    ),
  files: (id: string, path: string, raw = false, project?: string) =>
    request<{ items?: FileItem[]; content?: string; path?: string }>(
      `${sessionUrl(id, "/files", project)}${project ? "&" : "?"}path=${encodeURIComponent(path)}${raw ? "&raw=true" : ""}`,
    ),
};

export function sessionUrl(id: string, suffix = "", project?: string): string {
  const base = `/api/sessions/${encodeURIComponent(id)}${suffix}`;
  return project ? `${base}?project=${encodeURIComponent(project)}` : base;
}
