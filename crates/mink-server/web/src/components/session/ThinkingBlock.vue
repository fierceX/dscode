<script setup lang="ts">
// 思考面板：折叠由 running 驱动（turn 中展开/结束折叠），用户手动可覆盖
import { computed, ref, watch } from "vue";
import { appState } from "../../lib/store";
import { renderMarkdown } from "../../lib/markdown";
import type { ThinkingItem } from "../../lib/types";

const props = defineProps<{ item: ThinkingItem }>();
const html = computed(() => renderMarkdown(props.item?.text ?? ""));
// 初始值按当前 running：流式新块（running 中创建）默认展开。
// @toggle 同步用户手动操作；watch(running) 在 running 变化时总是覆盖
// （turn 结束自动折叠；turn 中用户手动展开/折叠保持）。
const open = ref(appState.sessionState?.running ?? false);

// running 变化（turn 结束）折叠
watch(
  () => appState.sessionState?.running,
  (running) => { open.value = running ?? false; },
);
// "思考完毕"折叠：后续非 thinking 事件（text/tool 开始）到达时，
// 本块不再是最后项 → 自动收起（流式推进中旧思考收拢）
watch(
  () => appState.sessionState?.items[appState.sessionState?.items.length - 1]?.kind,
  (lastKind) => {
    if (lastKind && lastKind !== "thinking") open.value = false;
  },
);
const onToggle = (e: Event) => {
  open.value = (e.target as HTMLDetailsElement).open;
};
</script>

<template>
  <details class="thinking-panel" :open="open" @toggle="onToggle">
    <summary>思考过程</summary>
    <div class="tp-body md-body" v-html="html"></div>
  </details>
</template>

<style scoped>
.thinking-panel { align-self: stretch; border: 1px solid var(--line); border-radius: var(--radius); background: var(--panel); overflow: hidden; animation: rise-in 0.14s ease; }
.thinking-panel summary { padding: 8px 14px; cursor: pointer; color: var(--text-dim); font-size: 12px; font-family: var(--mono); list-style: none; user-select: none; }
.thinking-panel summary::before { content: "▾"; margin-right: 8px; opacity: 0.6; }
.thinking-panel:not([open]) summary::before { content: "▸"; }
.thinking-panel summary:hover { color: var(--text-soft); }
.tp-body { border-top: 1px solid var(--line); padding: 10px 14px; color: var(--text-soft); font-size: 13px; max-height: 360px; overflow-y: auto; }
</style>
