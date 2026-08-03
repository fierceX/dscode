<script setup lang="ts">
import { computed } from "vue";
import { renderMarkdown } from "../../../lib/markdown";

const props = defineProps<{ content: string }>();
const html = computed(() => renderMarkdown(props.content));
</script>

<template>
  <div class="md-body plan-body" v-html="html"></div>
</template>

<style scoped>
.plan-body { font-size: 13px; }
</style>
