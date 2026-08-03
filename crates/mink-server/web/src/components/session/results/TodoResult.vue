<script setup lang="ts">
import { computed } from "vue";
import { parseTodoContent, classifyTodoLine, parseChanges, formatChanges } from "../../../lib/toolFormat";

const props = defineProps<{ content: string; presentation?: unknown }>();
const blocks = computed(() => parseTodoContent(props.content));
const changes = computed(() => formatChanges(parseChanges(props.presentation)));
const hasBlocks = computed(() => blocks.value.length > 0);

const lineSymbol = (line: string) =>
  line
    .replace(/^- added /, "＋ added ")
    .replace(/^- updated /, "～ updated ")
    .replace(/^- removed /, "－ removed ")
    .replace(/^Completed:/, "✓ Completed:")
    .replace(/^Activated:/, "◉ Activated:")
    .replace(/^Paused:/, "○ Paused:")
    .replace(/^Reopened:/, "↻ Reopened:");
</script>

<template>
  <div v-if="changes" class="t-changes">{{ changes }}</div>
  <template v-if="hasBlocks">
    <div v-for="(block, bi) in blocks" :key="bi">
      <!-- snapshot / current：revision + counts 头 -->
      <div v-if="block.kind === 'snapshot' || block.kind === 'current'" class="t-head-block">
        <span v-if="block.revision !== undefined" class="t-meta">revision {{ block.revision }}</span>
        <span v-if="block.counts" class="t-meta">{{ block.counts.pending }} pending · {{ block.counts.in_progress }} in_progress · {{ block.counts.completed }} completed</span>
      </div>
      <!-- event：变更行（着色） -->
      <div v-if="block.kind === 'event'" class="t-event">
        <div v-for="(line, i) in block.lines" :key="i" class="t-event-line" :class="`t-ev-${classifyTodoLine(line)}`">{{ lineSymbol(line) }}</div>
      </div>
      <!-- note（current-todos 提示） -->
      <div v-if="block.note" class="t-note">{{ block.note }}</div>
      <!-- 任务列表 -->
      <div v-if="block.tasks.length > 0" class="t-tasks">
        <div v-for="task in block.tasks" :key="task.id" class="t-task" :class="{ done: task.status === 'completed' }">
          <span class="t-status" :class="task.status">{{ task.status }}</span>
          <span class="t-task-text">{{ task.id }}: {{ task.text }}</span>
        </div>
      </div>
    </div>
  </template>
  <pre v-else class="t-raw">{{ content }}</pre>
</template>

<style scoped>
.t-changes { font-size: 11px; font-family: var(--mono); color: var(--text-dim); margin-bottom: 6px; }
.t-head-block { display: flex; gap: 12px; flex-wrap: wrap; margin-bottom: 6px; }
.t-meta { font-size: 11px; font-family: var(--mono); color: var(--text-dim); }
.t-tasks { display: flex; flex-direction: column; gap: 3px; }
.t-task { display: flex; gap: 8px; align-items: baseline; font-size: 12.5px; }
.t-task.done { opacity: 0.55; }
.t-task.done .t-task-text { text-decoration: line-through; }
.t-status { font-family: var(--mono); font-size: 10.5px; min-width: 86px; text-align: center; border-radius: 999px; padding: 1px 6px; flex-shrink: 0; }
.t-status.pending { background: rgba(181, 122, 28, 0.12); color: var(--yellow); }
.t-status.in_progress { background: var(--blue-soft); color: var(--blue); }
.t-status.completed { background: rgba(23, 154, 97, 0.1); color: var(--green); }
.t-status.active { background: var(--blue-soft); color: var(--blue); }
.t-event { display: flex; flex-direction: column; gap: 2px; }
.t-event-line { font-family: var(--mono); font-size: 12px; white-space: pre-wrap; }
.t-ev-add { color: var(--green); }
.t-ev-update { color: var(--yellow); }
.t-ev-remove { color: var(--red); }
.t-ev-label { color: var(--text-soft); font-weight: 600; }
.t-note { font-size: 12px; color: var(--text-dim); font-style: italic; margin: 4px 0; }
.t-raw { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: 12px; color: var(--text-soft); }
</style>
