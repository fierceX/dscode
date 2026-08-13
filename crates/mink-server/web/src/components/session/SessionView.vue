<script setup lang="ts">
import { computed, ref } from "vue";
import { fmtK } from "../../lib/fmt";
import { appState } from "../../lib/store";
import { closeSessionView, openSession } from "../../lib/sessionController";
import ContextPanel from "./ContextPanel.vue";
import FilePanel from "./FilePanel.vue";
import { uiState } from "../../lib/store";
import Transcript from "./Transcript.vue";
import InputBar from "./InputBar.vue";

const st = computed(() => appState.sessionState);
const openCtx = (tab: string) => {
  if (!st.value?.sessionId) return;
  uiState.ctxTab = tab;
  uiState.ctxOpen = true;
};
const openFiles = () => {
  if (st.value?.sessionId) uiState.fileOpen = true;
};

// 数值定宽格式化（输入/输出：k 单位一位小数，宽度恒定）
const cost = computed(() => ((st.value?.costMicros ?? 0) / 1_000_000).toFixed(4));
const belief = computed(() => (st.value?.belief ?? 0).toFixed(2));
// 移动端费用紧凑：$0.021 / $0.5 / $1.23
const costMicrosCompact = computed(() => {
  const usd = (st.value?.costMicros ?? 0) / 1_000_000;
  if (usd >= 1) return usd.toFixed(2);
  if (usd >= 0.01) return usd.toFixed(3).replace(/0$/, "");
  return usd.toFixed(4).replace(/0+$/, "") || "0";
});

// Agent 状态徽标（TUI WorkState 全称 + 色）
const WORK_LABEL: Record<string, string> = {
  idle: "空闲", waiting: "等待模型", thinking: "思考中", generating: "生成中",
  tool: "执行工具", "sub-agent": "子代理", compacting: "压缩中", error: "错误",
};
// 缓存命中率（cacheRead / (input + cacheRead)）与上下文占用率
const cachePct = computed(() => {
  const total = (st.value?.tokensIn ?? 0) + (st.value?.cacheReadTokens ?? 0);
  if (!total) return 0;
  return Math.round(((st.value?.cacheReadTokens ?? 0) / total) * 100);
});
const ctxPct = computed(() => {
  const max = st.value?.maxContextTokens ?? 0;
  if (!max) return null;
  return Math.round(((st.value?.contextTokens ?? 0) / max) * 100);
});
const workLabel = computed(() => {
  const w = st.value?.workState ?? "idle";
  return WORK_LABEL[w] ?? (st.value?.running ? "运行中" : "空闲");
});

/** 重连（顶栏 ⟳ 复用）：关闭旧 SSE → 复用 openSession（幂等 open + 重拉 conversation + 新订阅） */
const reconnect = async () => {
  const id = st.value?.sessionId;
  if (!id) return;
  const summary = appState.sessions.find((s) => s.id === id);
  if (!summary) return;
  try { await openSession(summary); } catch (e) { console.error("[reconnect] failed:", e); }
};
</script>

<template>
  <div class="session-page">
    <!-- 会话实时指标行（对话区顶部，信息在左 / 状态徽标贴右） -->
    <div class="sess-metrics">
      <span v-if="st?.model" class="sm sm-model">{{ st.model }}</span>
      <!-- 桌面：全称标识 -->
      <span class="sm sm-full">输入 <b>{{ fmtK(st?.tokensIn ?? 0) }}</b><em v-if="cachePct > 0">缓存{{ cachePct }}%</em></span>
      <span class="sm sm-full">输出 <b>{{ fmtK(st?.tokensOut ?? 0) }}</b></span>
      <span v-if="(st?.contextTokens ?? 0) > 0" class="sm sm-full">上下文 <b>{{ fmtK(st?.contextTokens ?? 0) }}</b><em v-if="ctxPct !== null">（{{ ctxPct }}%）</em></span>
      <span v-if="(st?.costMicros ?? 0) > 0" class="sm sm-full">费用 <b>¥{{ cost }}</b></span>
      <span v-if="(st?.belief ?? 0) > 0" class="sm sm-full">信念度 <b>{{ belief }}</b></span>
      <!-- 移动端：字母标识（TUI 风格缩写，数值向上换算） -->
      <span class="sm sm-abbr">I<b>{{ fmtK(st?.tokensIn ?? 0) }}</b><em v-if="cachePct > 0">C{{ cachePct }}%</em></span>
      <span class="sm sm-abbr">O<b>{{ fmtK(st?.tokensOut ?? 0) }}</b></span>
      <span v-if="(st?.contextTokens ?? 0) > 0" class="sm sm-abbr">Ctx<b>{{ fmtK(st?.contextTokens ?? 0) }}</b><em v-if="ctxPct !== null">({{ ctxPct }}%)</em></span>
      <span v-if="(st?.costMicros ?? 0) > 0" class="sm sm-abbr">¥<b>{{ costMicrosCompact }}</b></span>
      <span v-if="(st?.belief ?? 0) > 0" class="sm sm-abbr">B<b>{{ belief }}</b></span>
      <span class="sm-ops">
        <button class="op-desktop" title="计划" @click="openCtx('plan')">计划</button>
        <button class="op-desktop" title="Todo" @click="openCtx('todo')">Todo</button>
        <button class="op-desktop" title="Artifacts" @click="openCtx('art')">Artifacts</button>
        <button class="op-desktop" title="文件" @click="openFiles">文件</button>
        <button class="op-desktop" title="关闭会话" @click="closeSessionView()">关闭</button>
        <button class="op-more" title="更多" @click="openCtx('plan')">⋯</button>
      </span>
      <span class="sm-state-wrap">
        <span class="sm-dot" :class="{ running: st?.running }"></span>
        <span class="sm-state" :class="st?.workState">{{ workLabel }}</span>
      </span>
    </div>
    <Transcript />
    <InputBar />
    <ContextPanel v-if="st?.sessionId" :session-id="st.sessionId" />
    <FilePanel v-if="st?.sessionId" :session-id="st.sessionId" />
  </div>
</template>

<style scoped>
.session-page { display: flex; flex-direction: column; height: 100%; min-height: 0; }
/* 指标行：信息左固定（flex-shrink:0 + 数值定宽），状态徽标 margin-left:auto 贴右 */
.sess-metrics {
  display: flex; align-items: center; gap: 14px;
  padding: 8px max(24px, calc((100% - 840px) / 2)); border-bottom: 1px solid var(--line);
  background: rgba(255, 255, 255, 0.85); backdrop-filter: blur(8px);
  font-family: var(--mono); font-size: 11px; color: var(--text-dim);
  flex-shrink: 0; white-space: nowrap; overflow-x: auto;
}
.sess-metrics .sm { flex-shrink: 0; }
.sess-metrics .sm-abbr { display: none; }
.sess-metrics .sm b { font-weight: 600; color: var(--text-soft); }
.sess-metrics .sm em { font-style: normal; color: var(--green); margin-left: 4px; }
.sess-metrics .sm-model { color: var(--blue); font-weight: 700; }
.sm-ops { margin-left: auto; display: flex; gap: 4px; flex-shrink: 0; }
.sm-ops button {
  font-size: 11px; padding: 3px 9px; border-radius: 7px;
  border: 1px solid var(--line); background: #fff; color: var(--text-soft); cursor: pointer;
}
.sm-ops .op-more { display: none; }
.sm-ops button:hover { border-color: var(--blue); color: var(--blue); }
.sm-state-wrap { display: flex; align-items: center; gap: 7px; flex-shrink: 0; }
.sm-dot {
  width: 8px; height: 8px; border-radius: 50%;
  background: var(--green); box-shadow: 0 0 0 3px rgba(23, 154, 97, 0.14);
}
.sm-dot.running { animation: sm-pulse 1.6s ease-in-out infinite; }
@keyframes sm-pulse { 50% { box-shadow: 0 0 0 6px rgba(23, 154, 97, 0.06); } }
.sm-state {
  padding: 1px 9px; border-radius: 999px;
  font-size: 10px; font-weight: 600; letter-spacing: 0.02em;
  background: var(--panel-2); color: var(--text-dim);
}
.sm-state.running, .sm-state.waiting, .sm-state.compacting { background: rgba(181, 122, 28, 0.1); color: #b57a1c; }
.sm-state.thinking, .sm-state.generating { background: rgba(47, 111, 237, 0.09); color: var(--blue); }
.sm-state.tool, .sm-state.sub-agent { background: rgba(124, 92, 255, 0.1); color: #7c5cff; }
.sm-state.error { background: rgba(214, 69, 93, 0.1); color: var(--red); }
@media (max-width: 640px) {
  .sess-metrics { padding: 7px 12px; gap: 10px; overflow-x: auto; }
  /* 窄屏精简：隐藏模型/输出/信念度，保留输入+缓存/上下文/费用/状态 */
  .sess-metrics .sm-model, .sess-metrics .sm-full { display: none; }
  .sess-metrics .sm-abbr { display: inline-flex; }
  .sm-ops { gap: 3px; }
  /* 窄屏：5 个详情按钮合并为一个 ⋯（面板内 tabs 承接） */
  .sm-ops .op-desktop { display: none; }
  .sm-ops .op-more { display: inline-block; padding: 4px 9px; font-size: 14px; font-weight: 700; }
}
</style>
