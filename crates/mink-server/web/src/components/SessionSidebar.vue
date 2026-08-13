<script setup lang="ts">
// 会话抽屉：分组（今天/昨天/更早）+ 搜索 + 状态点 + 预览；tokens 徽标由 P3 数据接入
import { computed, ref } from "vue";
import { fmtK } from "../lib/fmt";
import { appState, workspaceSessions } from "../lib/store";
import { api } from "../lib/api";
import type { SessionSummary } from "../lib/api";
import { openSession } from "../lib/sessionController";

const query = ref("");

// 未选项目时回退到第一个项目（顶栏项目下拉可切换）
const curCwd = computed(() => appState.currentWorkspace ?? appState.sessions[0]?.cwd ?? "");
const all = computed(() => (curCwd.value ? workspaceSessions(curCwd.value) : []));
const filtered = computed(() => {
  const q = query.value.toLowerCase();
  return all.value.filter((s) => (s.title ?? s.alias ?? s.id).toLowerCase().includes(q));
});

type GroupKey = "today" | "yesterday" | "earlier";
const groups = computed(() => {
  const g: Record<GroupKey, SessionSummary[]> = { today: [], yesterday: [], earlier: [] };
  const now = new Date();
  const y = new Date(now); y.setDate(now.getDate() - 1);
  for (const s of filtered.value) {
    const d = new Date(s.updated_at);
    if (d.toDateString() === now.toDateString()) g.today.push(s);
    else if (d.toDateString() === y.toDateString()) g.yesterday.push(s);
    else g.earlier.push(s);
  }
  return g;
});
const groupLabel: Record<GroupKey, string> = { today: "今天", yesterday: "昨天", earlier: "更早" };

function fmtTime(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}
function statusColor(s: SessionSummary): string {
  return s.status === "running" ? "var(--green)" : s.status === "active" ? "var(--blue)" : "#c3ccd8";
}
const displayTitle = (s: SessionSummary) => s.title ?? s.alias ?? s.id.slice(0, 12);

const del = async (s: SessionSummary) => {
  if (!confirm(`删除会话 ${s.title ?? s.id.slice(0, 8)} 及其全部文件？`)) return;
  await api.deleteSession(s.id, s.project_key);
  location.reload();
};
const open = async (s: SessionSummary) => {
  try { await openSession(s); } catch (e) { alert(`打开失败: ${String(e)}`); }
};
</script>

<template>
  <aside class="sessions-sidebar">
    <div class="panel-head">会话 <span class="count">{{ all.length }}</span></div>
    <div class="drawer-search"><input v-model="query" type="text" placeholder="搜索会话…" /></div>
    <div class="session-list">
      <template v-for="(list, key) in groups" :key="key">
        <template v-if="list.length">
          <div class="dgroup">{{ groupLabel[key as GroupKey] }}</div>
          <div
            v-for="s in list" :key="`${s.project_key}:${s.id}`"
            class="sess-row" :class="{ active: s.id === appState.currentSessionId && s.project_key === appState.currentProjectKey }"
            role="button" tabindex="0" :data-id="s.id" @click="open(s)" @keydown.enter="open(s)"
          >
            <div class="sess-top">
              <span class="dot" :style="{ background: statusColor(s) }"></span>
              <span class="sess-title" :title="s.title ?? s.alias ?? s.id">{{ displayTitle(s) }}</span>
              <span v-if="(s.tokens_in ?? 0) > 0" class="tk">{{ fmtK(s.tokens_in! + s.tokens_out!) }}</span>
              <button class="sess-del" title="删除" @click.stop="del(s)">×</button>
            </div>
            <div class="sess-meta">
              <span>{{ fmtTime(s.updated_at) }}</span>
              <span class="sess-preview">{{ s.path.split("/").pop() }}</span>
            </div>
          </div>
        </template>
      </template>
      <div v-if="all.length === 0" class="hint">先选择一个工作区</div>
      <div v-else-if="filtered.length === 0" class="hint">无匹配会话</div>
    </div>
  </aside>
</template>

<style scoped>
.sessions-sidebar { display: flex; flex-direction: column; background: var(--bg-elevated); overflow: hidden; }
.panel-head {
  font-size: 10.5px; font-family: var(--mono); letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--text-dim); padding: 14px 14px 8px; display: flex; align-items: center; gap: 8px;
}
.panel-head .count { font-size: 10px; background: var(--panel-2); border-radius: 999px; padding: 1px 7px; }
.drawer-search { padding: 0 12px 8px; }
.drawer-search input {
  width: 100%; box-sizing: border-box;
  border: 1px solid var(--line); border-radius: 8px; padding: 7px 11px;
  font: inherit; font-size: 12.5px; outline: none; background: #fff;
}
.drawer-search input:focus { border-color: var(--blue); box-shadow: 0 0 0 3px rgba(47, 111, 237, 0.09); }
.session-list { flex: 1; overflow-y: auto; padding: 2px 8px 14px; }
.dgroup {
  font-size: 9.5px; font-family: var(--mono); letter-spacing: 0.1em; text-transform: uppercase;
  color: var(--text-dim); padding: 10px 10px 4px;
}
.sess-row { display: flex; flex-direction: column; gap: 3px; padding: 8px 10px; border-radius: 9px; cursor: pointer; margin-bottom: 2px; transition: background 0.12s; }
.sess-row:hover { background: var(--panel-2); }
.sess-row.active { background: var(--blue-soft); }
.sess-row.active .sess-title { color: var(--blue); font-weight: 600; }
.sess-top { display: flex; align-items: center; gap: 8px; }
.sess-top .dot { width: 6px; height: 6px; border-radius: 50%; flex-shrink: 0; }
.sess-title { flex: 1; font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tk { font-size: 9.5px; font-family: var(--mono); color: var(--text-dim); background: var(--panel-2); border-radius: 999px; padding: 1px 7px; flex-shrink: 0; }
.sess-del { background: none; border: none; color: var(--text-dim); font-size: 13px; padding: 0 4px; line-height: 1; opacity: 0; }
.sess-row:hover .sess-del { opacity: 1; }
.sess-del:hover { color: var(--red); }
.sess-meta { display: flex; gap: 8px; padding-left: 14px; font-size: 10.5px; color: var(--text-dim); font-family: var(--mono); }
.sess-preview { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.hint { color: var(--text-dim); font-size: 12px; padding: 14px 10px; text-align: center; }
</style>
