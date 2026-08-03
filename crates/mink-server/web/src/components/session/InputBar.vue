<script setup lang="ts">
import { ref, computed } from "vue";
import { appState, applyEvent } from "../../lib/store";
import { api } from "../../lib/api";

const input = ref("");
const busy = ref(false);
const errorHint = ref("");
const running = computed(() => appState.sessionState?.running ?? false);

const send = async () => {
  const text = input.value.trim();
  const id = appState.sessionState?.sessionId;
  if (!text || !id || busy.value || running.value) return;
  busy.value = true;
  errorHint.value = "";
  input.value = "";
  try {
    const resp = await api.sendTurn(id, text);
    if (resp.code !== 200) {
      const hint = resp.message.includes("too many running")
        ? "其他会话正在运行，请等待或先中断/关闭它们"
        : resp.message.includes("locked")
          ? "该会话正被 TUI/其他窗口使用"
          : resp.message;
      errorHint.value = `发送失败: ${hint}`;
      input.value = text; // 恢复输入，不丢失内容
      return;
    }
    // 本地立即追加用户消息（广播流不含 user_input——刷新后由 conversation 提供）
    applyEvent({ type: "user_input", content: text });
    if (appState.sessionState) appState.sessionState.running = true;
  } catch (e) {
    errorHint.value = `发送失败: ${String(e)}`;
    input.value = text;
  } finally {
    busy.value = false;
  }
};
const interrupt = async () => {
  const id = appState.sessionState?.sessionId;
  if (!id) return;
  try { await api.interrupt(id); } catch (e) { errorHint.value = `中断失败: ${String(e)}`; }
};
const onKeydown = (e: KeyboardEvent) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }
  else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); interrupt(); }
};
</script>

<template>
  <div class="input-bar">
    <div v-if="errorHint" class="error-hint" role="alert">{{ errorHint }}</div>
    <textarea
      v-model="input" rows="1" :disabled="running"
      placeholder="输入消息，Enter 发送，Shift+Enter 换行，Ctrl+Enter 中断"
      @keydown="onKeydown"
    ></textarea>
    <button v-if="running" class="danger" @click="interrupt">中断</button>
    <button class="primary" :disabled="busy || running || !input.trim()" @click="send">发送</button>
  </div>
</template>

<style scoped>
.input-bar {
  display: flex; gap: 10px; padding: 12px 16px 16px;
  padding-left: max(24px, calc((100% - 840px) / 2));
  padding-right: max(24px, calc((100% - 840px) / 2));
  align-items: flex-end; border-top: 1px solid var(--line); flex-wrap: wrap;
}
.error-hint { width: 100%; color: var(--red); background: rgba(214, 69, 93, 0.06); border: 1px solid rgba(214, 69, 93, 0.22); border-radius: 8px; padding: 8px 12px; font-size: 12px; font-family: var(--mono); word-break: break-all; }
textarea { flex: 1; resize: none; min-height: 44px; max-height: 160px; }
button { height: 44px; padding: 0 18px; }
@media (max-width: 768px) {
  .input-bar { gap: 8px; }
  /* 窄屏：输入框独占一行，中断/发送按钮在下方均分一行——避免三元素挤压错位 */
  textarea { flex: 1 1 100%; font-size: 16px; }
  button { flex: 1; padding: 0 14px; }
}
</style>
