<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../../lib/api";
import { appState } from "../../lib/store";

const props = defineProps<{ sessionId: string; artifactId: string }>();
const content = ref("加载中…");

onMounted(async () => {
  const resp = await api.artifact(props.sessionId, props.artifactId, appState.currentProjectKey ?? undefined);
  content.value = resp.code === 200 ? resp.data?.content ?? "" : `读取失败: ${resp.message}`;
});
</script>

<template><pre>{{ content }}</pre></template>
