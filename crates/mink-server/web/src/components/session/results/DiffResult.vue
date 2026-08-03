<script setup lang="ts">
import { computed } from "vue";
import { classifyDiffLine } from "../../../lib/toolFormat";

const props = defineProps<{ content: string }>();
const lines = computed(() => props.content.split("\n").map((line) => ({ text: line, cls: classifyDiffLine(line) })));
</script>

<template>
  <pre class="t-diff"><span v-for="(l, i) in lines" :key="i" :class="`d-${l.cls}`">{{ l.text }}</span>
</pre>
</template>

<style scoped>
.t-diff { margin: 0; font-family: var(--mono); font-size: 12px; line-height: 1.55; overflow-x: auto; background: #f8fafb; border: 1px solid var(--line-soft); border-radius: var(--radius-sm); padding: 10px 12px; max-height: 340px; overflow-y: auto; }
.d-add { color: var(--green); }
.d-del { color: var(--red); }
.d-head { color: var(--blue); font-weight: 600; }
.d-ctx { color: var(--text-soft); }
</style>
