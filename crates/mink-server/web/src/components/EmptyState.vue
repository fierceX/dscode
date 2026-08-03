<script setup lang="ts">
// 空状态工作台：无会话时展示（hero + 最近会话卡片网格 + 统计条）
import { computed, onMounted, onUnmounted } from "vue";
import { fmtK } from "../lib/fmt";
import { appState, workspaces } from "../lib/store";
import { openSession } from "../lib/sessionController";
import { api } from "../lib/api";
import type { SessionSummary } from "../lib/api";

const emit = defineEmits<{ browse: [] }>();

const projList = computed(() => workspaces());
const curProj = computed(() =>
  projList.value.find((w) => w.cwd === appState.currentWorkspace) ?? projList.value[0] ?? null,
);
const curSessions = computed(() =>
  appState.sessions.filter((s) => s.cwd === (curProj.value?.cwd ?? "")),
);
const recent = computed(() => curSessions.value.slice(0, 4));

const totalSessions = computed(() => appState.sessions.length);
const baseName = (p: string) => p.split("/").filter(Boolean).pop() ?? p;

function fmtTime(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return "昨天";
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}
function statusLabel(s: SessionSummary): string {
  return s.status === "running" ? "运行中" : s.status === "active" ? "进行中" : "空闲";
}
function statusCls(s: SessionSummary): string {
  return `st-${s.status}`;
}

// 运行中统计实时化：列表 status 由服务端 registry 提供，但前端只加载一次——
// 首页轮询刷新（10s），使"运行中"计数与真实执行状态一致
let refreshTimer: ReturnType<typeof setInterval> | null = null;
const refreshSessions = async () => {
  try {
    const resp = await api.listSessions();
    if (resp.code === 200 && Array.isArray(resp.data)) {
      appState.sessions = resp.data;
    }
  } catch { /* 轮询失败静默，下轮重试 */ }
};
onMounted(() => {
  refreshSessions();
  refreshTimer = setInterval(refreshSessions, 10_000);
});
onUnmounted(() => {
  if (refreshTimer) clearInterval(refreshTimer);
});

const onNew = async () => {
  if (!curProj.value) return;
  const name = prompt("会话名称（alias）", "untitled");
  if (name == null) return;
  try {
    const resp = await api.createSession(name || "untitled", curProj.value.cwd);
    if (resp.code === 200 && resp.data?.id) {
      appState.sessions.unshift(resp.data);
      appState.currentWorkspace = resp.data.cwd;
      await openSession(resp.data);
    }
  } catch { /* openSession 内部提示 */ }
};
const open = (s: SessionSummary) => openSession(s).catch(() => {});
</script>

<template>
  <div class="empty">
    <div class="e">
      <div class="hero"><span class="hm">M</span></div>
      <h1>继续工作</h1>
      <p>从最近会话继续，或创建新任务。任务在服务端后台运行——关闭浏览器也不中断。</p>
      <div class="quick">
        <button class="btn-primary" @click="onNew">＋ 新建会话</button>
        <button class="btn-secondary" @click="emit('browse')">浏览全部会话</button>
      </div>
      <div class="rc">
        <div class="rc-head">
          <h6>最近会话 · {{ curProj ? baseName(curProj.cwd) : "—" }}</h6>
          <span class="all" @click="emit('browse')">查看全部 →</span>
        </div>
        <div class="rc-grid">
          <div v-for="s in recent" :key="s.id" class="rrow" @click="open(s)">
            <div class="top">
              <span class="ico">💬</span>
              <span class="st" :class="statusCls(s)">{{ statusLabel(s) }}</span>
            </div>
            <div class="t">
              <div class="n">{{ s.title ?? s.alias ?? s.id }}</div>
              <div class="s">{{ s.path.split("/").pop() }}</div>
            </div>
            <span class="tm">{{ fmtTime(s.updated_at) }} · {{ baseName(s.cwd) }}<template v-if="(s.tokens_in ?? 0) > 0"> · {{ fmtK(s.tokens_in! + s.tokens_out!) }} tok</template></span>
          </div>
          <div v-if="recent.length === 0" class="no-sess">该项目暂无会话，点击"＋ 新建会话"开始。</div>
        </div>
      </div>
      <div class="stats">
        <span class="s"><b>{{ projList.length }}</b> 项目</span>
        <span class="s"><b>{{ totalSessions }}</b> 会话</span>
        <span class="s"><b>{{ curSessions.filter((s) => s.status === "running").length }}</b> 运行中</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.empty {
  flex: 1; display: flex; align-items: center; justify-content: center;
  overflow-y: auto; position: relative;
  background:
    radial-gradient(560px 260px at 18% 0%, rgba(79, 140, 255, 0.07), transparent 65%),
    radial-gradient(520px 240px at 85% 18%, rgba(124, 92, 255, 0.06), transparent 60%);
}
.e { max-width: 640px; width: 100%; text-align: center; padding: 46px 28px 40px; }
.hero {
  width: 64px; height: 64px; margin: 0 auto 18px; border-radius: 18px; position: relative;
  background: linear-gradient(135deg, #4f8cff, #7c5cff);
  box-shadow: 0 14px 34px rgba(84, 106, 255, 0.32);
}
.hero::after {
  content: ""; position: absolute; inset: -6px; border-radius: 22px;
  border: 1px solid rgba(84, 106, 255, 0.18);
}
.hero .hm { position: absolute; inset: 0; display: grid; place-items: center; color: #fff; font-weight: 800; font-size: 22px; }
h1 { font-size: 22px; margin-bottom: 7px; letter-spacing: -0.015em; }
p { color: var(--text-dim); font-size: 13.5px; margin-bottom: 24px; max-width: 440px; margin-left: auto; margin-right: auto; }
.quick { display: flex; gap: 10px; justify-content: center; }
.btn-primary {
  background: linear-gradient(180deg, #4c80ff, #3565ea);
  color: #fff; font-weight: 600; font-size: 13px;
  padding: 9px 17px; border-radius: 9px; border: none; cursor: pointer;
  box-shadow: 0 2px 8px rgba(53, 101, 234, 0.25);
}
.btn-primary:hover { background: linear-gradient(180deg, #5a8aff, #3b6ef6); }
.btn-secondary {
  padding: 9px 16px; border: 1px solid var(--line); border-radius: 9px;
  font-weight: 550; color: var(--text-soft); background: #fff; cursor: pointer; font-size: 13px;
}
.btn-secondary:hover { border-color: var(--blue); color: var(--blue); }
.rc { text-align: left; margin-top: 34px; }
.rc-head { display: flex; align-items: center; gap: 10px; margin-bottom: 11px; }
.rc-head h6 {
  font-size: 10px; font-family: var(--mono); letter-spacing: 0.14em;
  color: var(--text-dim); text-transform: uppercase; flex: 1;
}
.rc-head .all { font-size: 11.5px; color: var(--blue); cursor: pointer; padding: 2px 8px; border-radius: 6px; }
.rc-head .all:hover { background: rgba(47, 111, 237, 0.08); }
.rc-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 10px; min-width: 0; }
.rrow { min-width: 0;
  display: flex; flex-direction: column; gap: 8px;
  padding: 14px 15px; border: 1px solid var(--line); border-radius: 13px;
  cursor: pointer; transition: 0.16s; background: #fff;
}
.rrow:hover { border-color: var(--blue); box-shadow: 0 1px 2px rgba(16, 24, 40, 0.05), 0 8px 24px rgba(16, 24, 40, 0.06); transform: translateY(-2px); }
.rrow .top { display: flex; align-items: center; gap: 10px; min-width: 0; }
.rrow .t { min-width: 0; flex: 1; }
.rrow .ico {
  width: 34px; height: 34px; border-radius: 10px;
  background: linear-gradient(135deg, rgba(79, 140, 255, 0.12), rgba(124, 92, 255, 0.12));
  display: grid; place-items: center; font-size: 15px;
}
.rrow .st { font-size: 9.5px; font-family: var(--mono); padding: 2px 8px; border-radius: 999px; margin-left: auto; }
.st-running { background: rgba(23, 154, 97, 0.1); color: var(--green); }
.st-active { background: rgba(47, 111, 237, 0.08); color: var(--blue); }
.st-free { background: var(--panel-2); color: var(--text-dim); }
.rrow .t .n { font-size: 13.5px; font-weight: 650; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.rrow .t .s { font-size: 11.5px; color: var(--text-dim); margin-top: 2px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.rrow .tm { font-size: 10.5px; font-family: var(--mono); color: var(--text-dim); }
@media (max-width: 480px) {
  .e { padding: 32px 16px 28px; }
  .rc-grid { grid-template-columns: 1fr; }
  h1 { font-size: 19px; }
  p { font-size: 12.5px; }
}
.no-sess { grid-column: 1 / -1; color: var(--text-dim); font-size: 12.5px; text-align: center; padding: 20px; border: 1px dashed var(--line); border-radius: 12px; }
.stats {
  display: flex; justify-content: center; margin-top: 30px;
  padding: 12px 0; border-top: 1px solid var(--line);
  font-size: 11.5px; color: var(--text-dim); font-family: var(--mono);
}
.stats .s { display: flex; align-items: center; gap: 6px; padding: 0 18px; border-right: 1px solid var(--line); }
.stats .s:last-child { border-right: none; }
.stats .s b { font-weight: 650; color: var(--text-soft); }
</style>
