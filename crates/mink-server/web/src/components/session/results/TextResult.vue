<script setup lang="ts">
import { computed } from "vue";
import { renderMarkdown } from "../../../lib/markdown";

const props = defineProps<{ item: { text: string } }>();
const html = computed(() => renderMarkdown(props.item?.text ?? ""));
</script>

<template>
  <div class="md-body" v-html="html"></div>
</template>
