<script setup lang="ts">
// Transcript：conversation 轮次驱动渲染 + 滚动 + 懒加载（轮次为单位）
import { ref, watch, nextTick, computed } from "vue";
import { appState, prependOlder } from "../../lib/store";
import { api } from "../../lib/api";
import { conversationToEvents } from "../../lib/toolFormat";
import ThinkingBlock from "./ThinkingBlock.vue";
import ToolCard from "./ToolCard.vue";
import TextOutput from "./results/TextResult.vue";

const scrollEl = ref<HTMLElement | null>(null);
const items = computed(() => appState.sessionState?.items ?? []);

// divider：会话时间 · 项目（对话流顶部锚点）
const divider = computed(() => {
  const state = appState.sessionState;
  if (!state || items.value.length === 0) return null;
  const summary = appState.sessions.find((s) => s.id === state.sessionId);
  if (!summary) return null;
  const d = new Date(summary.updated_at);
  const now = new Date();
  const time = Number.isNaN(d.getTime())
    ? "—"
    : d.toDateString() === now.toDateString()
      ? `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`
      : `${d.getMonth() + 1}月${d.getDate()}日`;
  const proj = (appState.currentWorkspace ?? summary.cwd).split("/").filter(Boolean).pop() ?? "";
  return `${time} · ${proj}`;
});

// 复制消息文本（仅正常消息：user / agent text）
const copied = ref("");
const copyText = async (text: string) => {
  try {
    await navigator.clipboard.writeText(text);
    copied.value = text; // 完整文本匹配，避免多条消息同时显示 ✓
    setTimeout(() => (copied.value = ""), 1200);
  } catch { /* clipboard 不可用时静默 */ }
};
const loadingOlder = ref(false);
const hasOlder = ref(true);
const showTopHint = ref(false);
/** 用户是否主动滚动过（wheel/touch/scrollbar）：主动后停止自动跟随，
 * 滚回底部附近时恢复跟随 */
let userScrolled = false;
const NEAR_BOTTOM_PX = 60;

// 新内容/流式更新滚到底；懒加载前插（尾部 key+text 不变）不滚动
let prevLastKey = -1;
let prevLastText = "";
watch(
  () => [
    items.value.length,
    items.value[items.value.length - 1]?.kind,
    (items.value[items.value.length - 1] as { text?: string })?.text,
    (items.value[items.value.length - 1] as { result?: string })?.result,
  ],
  async () => {
    const last = items.value[items.value.length - 1] as { key?: number; text?: string } | undefined;
    const lastKey = last?.key ?? -1;
    const lastText = last?.text ?? "";
    if (lastKey === prevLastKey && lastText === prevLastText) return; // 前插：尾部未变
    prevLastKey = lastKey;
    prevLastText = lastText;
    await nextTick();
    // 自动跟随：用户未主动滚动时始终聚焦最新输出；
    // 用户滚动过后不打扰（除非已滚回底部附近 → 恢复跟随）
    if (scrollEl.value) {
      const el = scrollEl.value;
      if (userScrolled) {
        if (el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_PX) userScrolled = false;
      } else {
        el.scrollTop = el.scrollHeight;
      }
    }
  },
);

// 上拉懒加载：滚动到顶加载更早的 20 轮（conversation 分页，轮次为单位）
const loadOlder = async () => {
  if (loadingOlder.value || !hasOlder.value) return;
  const state = appState.sessionState;
  if (!state || state.items.length === 0) return;
  const firstKey = (state.items[0] as { key?: number }).key ?? 0;
  const firstRow = Math.floor(firstKey / 100); // conversation 行号（key=行号*100+子序号）
  if (firstRow <= 1) { hasOlder.value = false; showTopHint.value = true; return; }
  loadingOlder.value = true;
  const before = scrollEl.value?.scrollHeight ?? 0;
  try {
    const resp = await api.conversation(state.sessionId, { limit: 20, beforeSeq: firstRow });
    if (resp.code === 200 && Array.isArray(resp.data) && resp.data.length > 0) {
      const converted = (resp.data as Record<string, unknown>[]).flatMap(conversationToEvents);
      prependOlder(converted as never);
      await nextTick();
      if (scrollEl.value) scrollEl.value.scrollTop = scrollEl.value.scrollHeight - before;
    } else {
      hasOlder.value = false;
      showTopHint.value = true;
    }
  } catch {
    hasOlder.value = false;
  } finally {
    loadingOlder.value = false;
  }
};

const onScroll = (e: Event) => {
  const el = scrollEl.value;
  if (!el) return;
  // 用户主动滚动：标记（懒加载的自动 scrollTop 赋值也触发 scroll 事件，
  // 但程序赋值时 userScrolled 语义由 watch 管理——这里只标记用户手势）
  if (!(e as WheelEvent).isTrusted) return;
  userScrolled = true;
  if (el.scrollTop < 80) loadOlder();
};
</script>

<template>
  <div class="transcript" ref="scrollEl" @scroll.passive="onScroll">
    <div v-if="loadingOlder" class="load-hint">加载更早…</div>
    <div v-else-if="showTopHint && items.length > 0" class="load-hint">— 已到最早 —</div>
    <div v-if="divider" class="divider">{{ divider }}</div>
    <!-- 单 v-for 按事件顺序渲染（不能按 kind 分组——否则顺序错乱） -->
    <template v-for="(item, i) in items" :key="(item as any).key || `i${i}`">
      <ThinkingBlock v-if="item.kind === 'thinking' && (item as any).text?.trim()" :item="item" />
      <ToolCard v-else-if="item.kind === 'tool'" :item="item" />
      <div v-else-if="item.kind === 'text'" class="msg agent">
        <span class="av">M</span>
        <div class="msg-content">
          <div class="mbody"><TextOutput :item="item" /></div>
          <button v-if="(item as any).text" class="copy-btn" :class="{ done: copied === (item as any).text }" :title="copied === (item as any).text ? '已复制' : '复制'" @click="copyText((item as any).text ?? '')">
            <svg v-if="copied !== (item as any).text" class="ico" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
            <span v-else class="ok">✓</span>
          </button>
        </div>
      </div>
      <div v-else-if="item.kind === 'user'" class="msg user">
        <span class="av">👤</span>
        <div class="msg-content">
          <span class="bubble">{{ item.text }}</span>
          <button v-if="(item as any).text" class="copy-btn" :class="{ done: copied === (item as any).text }" :title="copied === (item as any).text ? '已复制' : '复制'" @click="copyText((item as any).text ?? '')">
            <svg v-if="copied !== (item as any).text" class="ico" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
            <span v-else class="ok">✓</span>
          </button>
        </div>
      </div>
      <div v-else-if="item.kind === 'error'" class="msg error">{{ item.text }}</div>
      <div v-else-if="item.kind === 'signal'" class="signal" :title="item.text">{{ item.text }}</div>
      <div v-else class="msg system">{{ item.text }}</div>
    </template>
  </div>
</template>

<style scoped>
.transcript {
  flex: 1; min-height: 0; overflow-y: auto;
  padding: 18px 24px;
  padding-left: max(24px, calc((100% - 840px) / 2));
  padding-right: max(24px, calc((100% - 840px) / 2));
  display: flex; flex-direction: column; gap: 13px;
}
/* 关键：flex 容器内容超出时不压缩子项（否则 details 被压到 2px——
 * summary 溢出被 overflow:hidden 裁剪，卡片视觉消失） */
.transcript > * { flex-shrink: 0; }
.divider {
  display: flex; align-items: center; gap: 14px;
  color: var(--text-dim); font-size: 11px; font-family: var(--mono);
  margin: 2px 0 4px; white-space: nowrap;
}
.divider::before, .divider::after { content: ""; flex: 1; height: 1px; background: var(--line); }
/* 消息内容容器：框住文本框与复制按钮（按钮流内位于文本下方，不盖文本） */
.msg-content { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.msg.agent .msg-content { flex: 1; align-items: flex-start; }
.msg.user .msg-content { align-self: flex-end; align-items: flex-end; }
.msg.user .msg-content .copy-btn { align-self: flex-start; } /* 气泡左下 */
.copy-btn {
  border: none; background: none; padding: 1px 3px;
  cursor: pointer; color: var(--text-dim);
  line-height: 1;
}
.copy-btn .ico { font-size: 13px; line-height: 1; }
.copy-btn .ok { font-size: 13px; font-weight: 700; }
.copy-btn:hover { color: var(--blue); }
.copy-btn.done { color: var(--green); }
.load-hint { align-self: center; color: var(--text-dim); font-size: 11px; font-family: var(--mono); letter-spacing: 0.05em; padding: 2px 0; }
.msg.error { align-self: stretch; color: var(--red); background: rgba(214, 69, 93, 0.06); border: 1px solid rgba(214, 69, 93, 0.22); font-family: var(--mono); font-size: 12px; white-space: pre-wrap; border-radius: var(--radius); padding: 9px 14px; }
.msg { display: flex; gap: 10px; align-items: flex-start; animation: rise-in 0.14s ease; }
.msg .av {
  width: 30px; height: 30px; border-radius: 9px; flex-shrink: 0;
  display: grid; place-items: center; font-size: 13px;
  line-height: 1;
}
.msg.user .av { background: var(--panel-3); font-size: 14px; margin-top: 7px; } /* 单行输入：头像中心与气泡中心对齐 */
.msg.agent .av {
  background: linear-gradient(135deg, #4f8cff, #7c5cff); color: #fff;
  font-weight: 700; font-size: 12px; box-shadow: 0 3px 10px rgba(84, 106, 255, 0.25);
  /* 视觉中心对齐第一行文字（方块 30px vs 行高约 20px） */
  margin-top: -5px;
}
.msg .mbody { min-width: 0; flex: 1; }
.msg.user { align-self: flex-end; justify-content: flex-end; max-width: 80%; }
.msg.user .bubble {
  background: var(--blue-soft); border: 1px solid rgba(47, 111, 237, 0.2);
  border-radius: var(--radius); padding: 9px 14px; white-space: pre-wrap;
}
.msg.agent { align-self: flex-start; max-width: 96%; }
.msg.system { align-self: flex-start; text-align: left; color: var(--text-dim); font-size: 11px; font-family: var(--mono); padding: 2px 4px; }
.signal {
  align-self: flex-start;
  color: var(--yellow);
  background: rgba(181, 122, 28, 0.08);
  border: 1px solid rgba(181, 122, 28, 0.2);
  border-radius: 6px;
  font-size: 11px;
  font-family: var(--mono);
  padding: 3px 10px;
  max-width: 90%;
  word-break: break-all;
}
</style>
