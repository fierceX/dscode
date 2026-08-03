<script setup lang="ts">
// TopBar：品牌 + 面包屑（项目 ▾ / 会话 ▾）+ 右侧操作 + 连接状态（最右胶囊）
// 提示类信息用独立 toast（右下角），不影响顶栏布局
import { computed, onMounted, onUnmounted, ref } from "vue";
import { appState, detachSession, uiState, workspaces } from "../lib/store";
import { fmtK } from "../lib/fmt";
import { openSession } from "../lib/sessionController";
import { api } from "../lib/api";
import type { SessionSummary } from "../lib/api";

const emit = defineEmits<{ toggleSidebar: [] }>();

const projOpen = ref(false);
const sessOpen = ref(false);
const sessQuery = ref("");

const projList = computed(() => workspaces());
const baseName = (p: string) => p.split("/").filter(Boolean).pop() ?? p;
const curProj = computed(() =>
  projList.value.find((w) => w.cwd === appState.currentWorkspace) ?? projList.value[0] ?? null,
);

// 项目 tokens 汇总（会话 tokens 求和）；行内徽标仅在有数据时显示
const projTokens = (cwd: string) => {
  const sum = appState.sessions
    .filter((s) => s.cwd === cwd)
    .reduce((acc, s) => acc + (s.tokens_in ?? 0) + (s.tokens_out ?? 0), 0);
  return sum > 0 ? fmtK(sum) : "";
};
const sessList = computed(() =>
  appState.sessions
    .filter((s) => s.cwd === (curProj.value?.cwd ?? ""))
    .filter((s) => (s.title ?? s.alias ?? "").toLowerCase().includes(sessQuery.value.toLowerCase())),
);
const curSess = computed(() => appState.sessions.find((s) => s.id === appState.currentSessionId) ?? null);
const crumbLabel = computed(() => curSess.value ? (curSess.value.title ?? curSess.value.alias ?? curSess.value.id) : "选择会话");

// 时间：今天 HH:MM / 昨天 / M月d日
function fmtTime(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const yesterday = new Date(now); yesterday.setDate(now.getDate() - 1);
  if (sameDay) return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (d.toDateString() === yesterday.toDateString()) return "昨天";
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}
function statusColor(s: SessionSummary): string {
  return s.status === "running" ? "var(--green)" : s.status === "active" ? "var(--blue)" : "#c3ccd8";
}

function selectProj(cwd: string) {
  appState.currentWorkspace = cwd;
  // 切换项目：清空当前会话（列表保留，可随时重开）
  appState.currentSessionId = null;
  projOpen.value = false;
}
function selectSess(s: SessionSummary) {
  sessOpen.value = false;
  openSession(s).catch((e) => flash("打开失败：" + String(e)));
}
// 点击品牌图标 → 回到 Home（空状态工作台；会话在服务端继续后台运行，可随时重开）
const goHome = () => {
  detachSession();
  projOpen.value = false;
  sessOpen.value = false;
};
const onNew = async () => {
  if (!curProj.value) { flash("请先选择工作区"); return; }
  const name = prompt("会话名称（alias）", "untitled");
  if (name == null) return;
  try {
    const resp = await api.createSession(name || "untitled", curProj.value.cwd);
    if (resp.code === 200 && resp.data?.id) {
      appState.sessions.unshift(resp.data);
      appState.currentWorkspace = resp.data.cwd;
      await openSession(resp.data);
    } else flash(`创建失败: ${resp.message}`);
  } catch (e) { flash(`创建失败: ${String(e)}`); }
};
const onFiles = () => {
  if (!appState.currentSessionId) { flash("请先选择会话"); return; }
  uiState.fileOpen = true;
};
const onCtx = () => {
  if (!appState.currentSessionId) { flash("请先选择会话"); return; }
  uiState.ctxOpen = true;
};

// toast
const toastMsg = ref("");
let toastTimer: ReturnType<typeof setTimeout> | null = null;
function flash(m: string) {
  toastMsg.value = m;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toastMsg.value = ""), 1800);
}

// 点击外部关闭下拉
const onDocClick = (e: MouseEvent) => {
  const t = e.target as HTMLElement;
  if (!t.closest(".topbar")) { projOpen.value = false; sessOpen.value = false; }
};
onMounted(() => document.addEventListener("click", onDocClick));
onUnmounted(() => document.removeEventListener("click", onDocClick));

// 顶栏 ⟳ 重连：复用 openSession（幂等 open + 重拉 conversation + 新订阅）
const onReconnect = async () => {
  const id = appState.currentSessionId;
  const s = id ? appState.sessions.find((x) => x.id === id) : null;
  if (!s) { flash("当前无会话"); return; }
  try { await openSession(s); flash("已重连"); } catch { flash("重连失败"); }
};
</script>

<template>
  <header class="topbar">
    <button class="hamburger" aria-label="菜单" @click="emit('toggleSidebar')">☰</button>
    <button class="brand" title="回到 Home" @click="goHome">
      <span class="brand-mark">M</span><span class="brand-label">Mink</span>
    </button>
    <div class="crumb">
      <button class="crumb-item proj" :class="{ on: projOpen }" @click="projOpen = !projOpen; sessOpen = false">
        <span class="crumb-ico">📁</span>
        <span class="crumb-label">{{ curProj ? baseName(curProj.cwd) : "选择项目" }}</span>
        <span class="caret">▾</span>
      </button>
      <span class="sep">/</span>
      <button class="crumb-item sess" :class="{ on: sessOpen }" @click="sessOpen = !sessOpen; projOpen = false">
        <span class="crumb-dot" :style="{ background: curSess ? statusColor(curSess) : '#c3ccd8' }"></span>
        <span class="crumb-label">{{ crumbLabel }}</span>
        <span class="caret">▾</span>
      </button>
    </div>

    <div class="spacer"></div>

    <button class="icon-btn" title="重连" @click="onReconnect">⟳</button>
    <button class="icon-btn" title="文件预览" @click="onFiles">📄</button>
    <button class="icon-btn ctx-btn" :class="{ off: !curSess }" title="上下文" @click="onCtx">▤</button>
    <button class="btn-primary" @click="onNew">＋ 新建</button>
    <span class="conn"><i></i> 已连接</span>

    <!-- 项目下拉 -->
    <div v-show="projOpen" class="drop proj-drop">
      <div class="drop-head">项目</div>
      <button v-for="w in projList" :key="w.cwd" class="drop-row" :class="{ on: w.cwd === curProj?.cwd }" @click="selectProj(w.cwd)">
        <span class="row-ico">📁</span>
        <span class="row-main">
          <span class="row-name">{{ baseName(w.cwd) }}</span>
          <span class="row-sub">{{ w.sessions.length }} 会话</span>
        </span>
        <span v-if="projTokens(w.cwd)" class="tk">{{ projTokens(w.cwd) }}</span>
      </button>
      <div v-if="projList.length === 0" class="drop-empty">暂无项目</div>
    </div>

    <!-- 会话下拉 -->
    <div v-show="sessOpen" class="drop sess-drop">
      <div class="drop-search"><input v-model="sessQuery" type="text" placeholder="搜索会话…" /></div>
      <div class="drop-list">
        <button v-for="s in sessList" :key="s.id" class="drop-row" :class="{ on: s.id === curSess?.id }" @click="selectSess(s)">
          <span class="row-dot" :style="{ background: statusColor(s) }"></span>
          <span class="row-main">
            <span class="row-name">{{ s.title ?? s.alias ?? s.id }}</span>
            <span class="row-sub">{{ fmtTime(s.updated_at) }} · {{ s.path.split("/").pop() }}</span>
          </span>
          <span v-if="(s.tokens_in ?? 0) + (s.tokens_out ?? 0) > 0" class="tk">{{ fmtK((s.tokens_in ?? 0) + (s.tokens_out ?? 0)) }}</span>
        </button>
        <div v-if="sessList.length === 0" class="drop-empty">该项目暂无会话</div>
      </div>
    </div>

    <transition name="toast">
      <div v-if="toastMsg" class="toast">{{ toastMsg }}</div>
    </transition>
  </header>
</template>

<style scoped>
.topbar {
  display: flex; align-items: center; gap: 8px;
  height: var(--topbar-h);
  padding: 0 20px;
  background: rgba(255, 255, 255, 0.92);
  border-bottom: 1px solid var(--line);
  backdrop-filter: blur(14px);
  flex-shrink: 0;
  position: relative;
  z-index: 30;
}
.hamburger {
  display: grid; place-items: center;
  width: 32px; height: 32px; border-radius: 8px;
  border: none; background: none; font-size: 15px; color: var(--text-dim); cursor: pointer;
}
.hamburger:hover { background: var(--panel-2); color: var(--text); }
.brand {
  display: flex; align-items: center; gap: 8px;
  font-weight: 750; font-size: 14px; margin-right: 4px;
  border: none; background: none; cursor: pointer;
  padding: 5px 9px; margin-left: -4px; border-radius: 9px;
  color: var(--text); font-family: inherit; transition: background 0.13s;
}
.brand:hover { background: var(--panel-2); }
.brand-mark {
  width: 26px; height: 26px; border-radius: 8px;
  background: linear-gradient(135deg, #4f8cff, #7c5cff); color: #fff;
  display: grid; place-items: center; font-size: 12px;
  box-shadow: 0 3px 10px rgba(84, 106, 255, 0.3);
}
.crumb { display: flex; align-items: center; gap: 6px; min-width: 0; }
.crumb-item {
  display: flex; align-items: center; gap: 7px;
  padding: 6px 12px; border-radius: 9px;
  border: none; background: none; cursor: pointer;
  font: inherit; font-size: 13px; color: var(--text);
  max-width: 260px;
}
.crumb-item:hover { background: var(--panel-2); }
.crumb-item.on { background: var(--panel-2); }
.crumb-item .crumb-label { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-weight: 600; }
.crumb-item .caret { font-size: 10px; color: var(--text-dim); }
.crumb-ico { font-size: 13px; }
.crumb-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; }
.crumb .sep { color: var(--text-dim); font-size: 13px; }
.spacer { flex: 1; }
.icon-btn {
  display: grid; place-items: center;
  width: 34px; height: 34px; border-radius: 9px;
  border: none; background: none; font-size: 15px; color: var(--text-soft); cursor: pointer; transition: 0.13s;
}
.icon-btn:hover { background: var(--panel-2); color: var(--text); }
.ctx-btn.off { visibility: hidden; } /* 保留占位：无会话时隐藏但不跳动 */
.btn-primary {
  background: linear-gradient(180deg, #4c80ff, #3565ea);
  color: #fff; font-weight: 600; font-size: 13px;
  padding: 8px 15px; border-radius: 9px; border: none; cursor: pointer;
  box-shadow: 0 2px 8px rgba(53, 101, 234, 0.25); transition: 0.15s;
}
.btn-primary:hover { background: linear-gradient(180deg, #5a8aff, #3b6ef6); }
.conn {
  display: flex; align-items: center; gap: 6px;
  font-size: 11px; font-family: var(--mono); color: var(--text-dim);
  margin-left: 6px; padding: 4px 10px;
  border: 1px solid var(--line); border-radius: 999px; background: var(--panel-2);
  flex-shrink: 0;
}
.conn i { width: 7px; height: 7px; border-radius: 50%; background: var(--green); box-shadow: 0 0 0 3px rgba(23, 154, 97, 0.12); }

/* 下拉 */
.drop {
  position: absolute; top: calc(var(--topbar-h) - 2px);
  background: #fff; border: 1px solid var(--line); border-radius: 13px;
  box-shadow: 0 24px 60px rgba(16, 24, 40, 0.14);
  z-index: 40; overflow: hidden;
}
.proj-drop { left: 72px; width: min(320px, 90vw); }
.sess-drop { left: 50%; transform: translateX(-50%); width: min(440px, 92vw); display: flex; flex-direction: column; max-height: 66vh; }
.drop-head { font-size: 10px; font-family: var(--mono); letter-spacing: 0.1em; color: var(--text-dim); text-transform: uppercase; padding: 13px 16px 5px; }
.drop-search { padding: 10px 12px 8px; border-bottom: 1px solid var(--line); }
.drop-search input {
  width: 100%; box-sizing: border-box;
  border: 1px solid var(--line); border-radius: 9px; padding: 8px 12px;
  font: inherit; font-size: 13px; outline: none;
}
.drop-search input:focus { border-color: var(--blue); box-shadow: 0 0 0 3px rgba(47, 111, 237, 0.09); }
.drop-list { overflow-y: auto; padding: 8px; }
.drop-row {
  display: flex; align-items: center; gap: 10px; width: 100%;
  padding: 9px 12px; border: none; background: none; border-radius: 9px;
  cursor: pointer; text-align: left; font: inherit; color: var(--text-soft);
}
.drop-row:hover { background: var(--panel-2); }
.drop-row.on { background: rgba(47, 111, 237, 0.08); }
.drop-row .tk { font-size: 9.5px; font-family: var(--mono); color: var(--text-dim); background: var(--panel-2); border-radius: 999px; padding: 1px 7px; flex-shrink: 0; }
.proj-drop .tk { background: rgba(47, 111, 237, 0.08); color: var(--blue); }
.drop-row.on .row-name { color: var(--blue); font-weight: 600; }
.row-ico { font-size: 14px; flex-shrink: 0; }
.row-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; }
.row-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.row-name { font-size: 13px; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.row-sub { font-size: 11px; color: var(--text-dim); }
.drop-empty { color: var(--text-dim); font-size: 12px; padding: 16px; text-align: center; }

/* toast */
.toast {
  position: fixed; right: 20px; bottom: 20px;
  background: var(--text); color: #fff; font-size: 12.5px;
  padding: 9px 16px; border-radius: 10px; z-index: 90;
  box-shadow: 0 24px 60px rgba(16, 24, 40, 0.2);
}
.toast-enter-active, .toast-leave-active { transition: opacity 0.2s, transform 0.2s; }
.toast-enter-from, .toast-leave-to { opacity: 0; transform: translateY(8px); }

@media (max-width: 640px) {
  .topbar { flex-wrap: wrap; gap: 6px 8px; padding: 6px 10px; height: auto; min-height: 56px; }
  .conn { display: none; }
  /* 窄屏：品牌只留图标；文件/上下文入口在会话页 sm-ops 提供 */
  .brand .brand-label { display: none; }
  .brand { margin-right: 0; padding: 5px 6px; }
  .icon-btn { width: 32px; height: 32px; }
  .icon-btn[title="文件预览"], .icon-btn[title="上下文"] { display: none; }
  /* 面包屑换行到第二行，全宽均分两列 */
  .crumb { order: 3; width: 100%; gap: 6px; margin-top: 2px; }
  .crumb-item { flex: 1; max-width: none; padding: 6px 10px; gap: 6px; font-size: 12px; justify-content: center; }
  .crumb-item .caret { display: inline; }
  .btn-primary { padding: 7px 11px; font-size: 12px; }
}
</style>
