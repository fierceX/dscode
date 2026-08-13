<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../../lib/api";
import { appState } from "../../lib/store";
import type { FileItem } from "../../lib/api";
import { openModal } from "./modalController";
import FileModal from "./FileModal.vue";

const props = defineProps<{ sessionId: string }>();
const path = ref("");
const items = ref<FileItem[]>([]);
const error = ref("");

onMounted(load);
async function load() {
  error.value = "";
  const resp = await api.files(props.sessionId, path.value, false, appState.currentProjectKey ?? undefined);
  if (resp.code !== 200) { error.value = resp.message; items.value = []; }
  else items.value = resp.data?.items ?? [];
}
const enter = (name: string) => { path.value = [path.value, name].filter(Boolean).join("/"); load(); };
const back = () => { const parts = path.value.split("/").filter(Boolean); parts.pop(); path.value = parts.join("/"); load(); };
const view = async (name: string) => {
  const filePath = [path.value, name].filter(Boolean).join("/");
  const resp = await api.files(props.sessionId, filePath, true, appState.currentProjectKey ?? undefined);
  openModal(FileModal, { path: filePath, content: resp.code === 200 ? resp.data?.content ?? "" : `读取失败: ${resp.message}` }, filePath);
};
</script>

<template>
  <div v-if="path" class="f-back" role="button" tabindex="0" @click="back">← 上级目录</div>
  <div v-if="error" class="empty">{{ error }}</div>
  <div class="file-tree">
    <div v-for="item in items" :key="item.name" class="f-row" role="button" tabindex="0"
      :class="{ 'f-dir': item.dir, 'f-file': !item.dir }"
      @click="item.dir ? enter(item.name) : view(item.name)">
      {{ item.dir ? "📁" : "📄" }} {{ item.name }}
    </div>
  </div>
</template>
