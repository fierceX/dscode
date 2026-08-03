<script setup lang="ts">
// Modal 壳：原生 dialog（showModal 原生焦点/ESC/backdrop），Vue Teleport 到 body。
import { onMounted, ref } from "vue";
import type { Component } from "vue";

const props = defineProps<{
  title: string;
  content: Component;
  contentProps: Record<string, unknown>;
  onClose: () => void;
}>();

const dialog = ref<HTMLDialogElement | null>(null);

onMounted(() => {
  if (dialog.value && !dialog.value.open) dialog.value.showModal();
});

const close = () => {
  dialog.value?.close();
  props.onClose();
};
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="modal" @close="props.onClose">
      <div class="modal-head">
        <span class="modal-title">{{ title }}</span>
        <button class="close" @click="close" aria-label="关闭">×</button>
      </div>
      <div class="modal-body">
        <component :is="content" v-bind="contentProps" />
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
/* 右侧抽屉：不遮挡中间交互区 */
.modal {
  background: var(--panel); border: none; border-left: 1px solid var(--line);
  border-radius: 0; width: min(480px, 92vw); height: 100vh;
  margin: 0; padding: 0; box-shadow: -12px 0 40px rgba(16, 24, 40, 0.2);
  color: var(--text);
  position: fixed; top: 0; right: 0; bottom: 0; left: auto; /* 覆盖 UA inset:0 的 left:0 */
  max-height: none;
  animation: drawer-in 0.18s ease;
}
@keyframes drawer-in {
  from { transform: translateX(24px); opacity: 0; }
  to { transform: translateX(0); opacity: 1; }
}
.modal::backdrop { background: rgba(16, 24, 40, 0.22); backdrop-filter: blur(1px); }
.modal-head { display: flex; align-items: center; padding: 13px 18px; border-bottom: 1px solid var(--line); font-weight: 600; font-size: 14px; }
.modal-title { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.close { background: none; border: none; color: var(--text-dim); font-size: 18px; padding: 0 4px; border-radius: var(--radius-xs); }
.close:hover { color: var(--text); background: var(--panel-2); }
.modal-body { padding: 16px 18px; overflow-y: auto; font-size: 13px; }
</style>
