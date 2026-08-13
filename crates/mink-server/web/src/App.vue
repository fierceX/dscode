<script setup lang="ts">
// App：单栏对话优先 + 左侧会话抽屉；桌面/移动端统一抽屉交互（⌘B 切换）
import { onMounted, onUnmounted, ref } from "vue";
import TopBar from "./components/TopBar.vue";
import SessionSidebar from "./components/SessionSidebar.vue";
import SessionView from "./components/session/SessionView.vue";
import EmptyState from "./components/EmptyState.vue";
import { appState, savedSessionId } from "./lib/store";
import { openSession } from "./lib/sessionController";
import { api } from "./lib/api";

const mobileSidebar = ref(false);
const toggleDrawer = () => { mobileSidebar.value = !mobileSidebar.value; };
const closeDrawer = () => { mobileSidebar.value = false; };

// ⌘B / Ctrl+B 切换会话抽屉
const onKey = (e: KeyboardEvent) => {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b") {
    e.preventDefault();
    toggleDrawer();
  }
};
onMounted(() => window.addEventListener("keydown", onKey));
onUnmounted(() => window.removeEventListener("keydown", onKey));
onMounted(async () => {
  const resp = await api.listSessions();
  if (resp.code === 200 && Array.isArray(resp.data)) {
    appState.sessions = resp.data;
  }
  // 恢复会话：?session=<id> 优先（E2E/分享链接），否则读取上次会话（重开浏览器自动重连）
  const params = new URLSearchParams(location.search);
  const stored = savedSessionId();
  const [storedProject, storedId] = stored?.includes("\n") ? stored.split("\n", 2) : [undefined, stored];
  const targetId = params.get("session") ?? storedId;
  const targetProject = params.get("project") ?? storedProject;
  if (targetId) {
    const found = appState.sessions.find((s) => s.id === targetId && (!targetProject || s.project_key === targetProject));
    if (found) {
      appState.currentWorkspace = found.cwd;
      openSession(found).catch((e) => { console.error("[App] restore session failed:", e); });
    }
  }
});
</script>

<template>
  <div class="app-shell">
    <TopBar @toggle-sidebar="mobileSidebar = !mobileSidebar" />
    <main class="content">
      <SessionView v-if="appState.currentSessionId" />
      <EmptyState v-else @browse="mobileSidebar = true" />
    </main>
    <!-- 左侧会话抽屉（桌面/移动端一致） -->
    <div class="side-drawer" :class="{ open: mobileSidebar }">
      <SessionSidebar class="sess-panel" />
    </div>
    <div v-if="mobileSidebar" class="drawer-mask" @click="closeDrawer"></div>
  </div>
</template>

<style scoped>
.app-shell { display: flex; flex-direction: column; height: 100%; }
.content {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0; /* flex 收缩关键：输入框常驻底部 */
  background: var(--bg);
}
.placeholder {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-dim);
  font-size: 13.5px;
  text-align: center;
  padding: 0 24px;
}
/* 侧栏抽屉（fixed 浮层，桌面/移动端一致） */
.side-drawer {
  position: fixed;
  top: 0; left: 0; bottom: 0;
  width: min(320px, 86vw);
  z-index: 60;
  display: flex;
  flex-direction: column;
  background: var(--bg-elevated);
  border-right: 1px solid var(--line);
  box-shadow: 12px 0 40px rgba(16, 24, 40, 0.25);
  transform: translateX(-102%);
  transition: transform 0.22s ease;
  overflow-y: auto;
  padding-top: var(--topbar-h);
}
.side-drawer.open { transform: translateX(0); }
.drawer-mask {
  position: fixed; inset: 0; z-index: 55;
  background: rgba(16, 24, 40, 0.35);
}
</style>
