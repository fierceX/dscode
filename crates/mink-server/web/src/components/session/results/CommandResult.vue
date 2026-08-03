<script setup lang="ts">
defineProps<{ content: string; exitCode?: number; summary?: string }>();
</script>

<template>
  <div class="t-out">
    <div v-if="summary" class="t-cmd">$ {{ summary }}</div>
    <pre class="t-body">{{ content }}</pre>
  </div>
  <div v-if="exitCode !== undefined" class="t-exit" :class="exitCode === 0 ? 'ok' : 'err'">exit {{ exitCode }}</div>
</template>

<style scoped>
.t-out { display: flex; flex-direction: column; gap: 8px; }
.t-cmd { font-family: var(--mono); font-size: 12px; color: var(--text-soft); background: var(--panel-2); border-radius: var(--radius-xs); padding: 5px 10px; overflow-x: auto; white-space: nowrap; }
.t-body { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: 12px; color: var(--text-soft); background: #f8fafb; border: 1px solid var(--line-soft); border-radius: var(--radius-sm); padding: 10px 12px; max-height: 340px; overflow-y: auto; }
.t-exit { font-size: 11px; font-family: var(--mono); }
.t-exit.ok { color: var(--text-dim); }
.t-exit.err { color: var(--red); }
</style>
