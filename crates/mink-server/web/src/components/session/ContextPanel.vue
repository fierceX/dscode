<script setup lang="ts">
// 上下文面板：右滑 4 tabs（计划/Todo/Artifacts/用量）；用量来自实时 sessionState
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { fmtK } from "../../lib/fmt";
import { api } from "../../lib/api";
import { appState, uiState } from "../../lib/store";
import FilePanel from "./FilePanel.vue";

const props = defineProps<{ sessionId: string }>();

interface TodoItem { id: string; content: string; status: string }
const plan = ref<{ plan: string | null; draft: string | null } | null>(null);
const todos = ref<{ revision?: number; items?: TodoItem[] } | null>(null);
const artifacts = ref<{ id: string; tool?: string }[] | null>(null);
const loaders = { plan: false, todo: false, art: false };

async function loadPlan() {
  if (plan.value || loaders.plan) return;
  loaders.plan = true;
  const resp = await api.plan(props.sessionId, appState.currentProjectKey ?? undefined);
  plan.value = resp.data as { plan: string | null; draft: string | null };
}
async function loadTodo() {
  if (todos.value || loaders.todo) return;
  loaders.todo = true;
  const resp = await api.todo(props.sessionId, appState.currentProjectKey ?? undefined);
  todos.value = ((resp.data as { todos?: typeof todos.value }).todos ?? null) as typeof todos.value;
}
async function loadArt() {
  if (artifacts.value || loaders.art) return;
  loaders.art = true;
  const resp = await api.artifacts(props.sessionId, appState.currentProjectKey ?? undefined);
  artifacts.value = (resp.data as { artifacts?: { id: string; tool?: string }[] }).artifacts ?? [];
}

watch(() => uiState.ctxTab, (tab) => {
  if (tab === "plan") loadPlan();
  else if (tab === "todo") loadTodo();
  else if (tab === "art") loadArt();
}, { immediate: true });
// PC 端（>640px）隐藏文件 tab：窗口放大后残留 file 状态切回 plan
const mobileMedia = window.matchMedia("(max-width: 640px)");
const onMediaChange = (e: MediaQueryListEvent) => {
  if (!e.matches && uiState.ctxTab === "file") uiState.ctxTab = "plan";
};
mobileMedia.addEventListener("change", onMediaChange);
watch(() => props.sessionId, () => {
  plan.value = todos.value = artifacts.value = null;
  loaders.plan = loaders.todo = loaders.art = false;
  const t = uiState.ctxTab;
  if (t === "plan") loadPlan();
  else if (t === "todo") loadTodo();
  else if (t === "art") loadArt();
});
onMounted(() => { uiState.ctxTab = "plan"; loadPlan(); });
onUnmounted(() => mobileMedia.removeEventListener("change", onMediaChange));

const close = () => { uiState.ctxOpen = false; };
const st = computed(() => appState.sessionState);
// 用量统计
const usage = computed(() => {
  const stt = appState.sessionState;
  const total = (stt?.tokensIn ?? 0) + (stt?.cacheReadTokens ?? 0);
  const cacheRate = total ? Math.round(((stt?.cacheReadTokens ?? 0) / total) * 100) : 0;
  const maxCtx = stt?.maxContextTokens ?? 0;
  return {
    model: stt?.model || "—",
    turns: stt?.items.filter((i) => i.kind === "user").length ?? 0,
    toolCalls: stt?.items.filter((i) => i.kind === "tool").length ?? 0,
    in: fmtK(stt?.tokensIn ?? 0),
    out: fmtK(stt?.tokensOut ?? 0),
    cache: fmtK(stt?.cacheReadTokens ?? 0),
    cacheRate,
    ctx: fmtK(stt?.contextTokens ?? 0),
    ctxPct: maxCtx ? Math.round(((stt?.contextTokens ?? 0) / maxCtx) * 100) : null,
    maxCtx: maxCtx ? fmtK(maxCtx) : null,
    cost: `¥${((stt?.costMicros ?? 0) / 1_000_000).toFixed(4)}`,
    belief: (stt?.belief ?? 0).toFixed(2),
  };
});
const todoCounts = computed(() => {
  const items = todos.value?.items ?? [];
  return { doing: items.filter((i) => i.status === "in_progress").length, todo: items.filter((i) => i.status === "pending").length };
});
</script>

<template>
  <Teleport to="body">
    <transition name="panel">
      <div v-if="uiState.ctxOpen" class="ctx-mask" @click="close"></div>
    </transition>
    <transition name="panel">
      <aside v-if="uiState.ctxOpen" class="ctx-panel">
        <div class="ctx-head">
          <span class="ctx-title">上下文</span>
          <button class="ctx-close" title="关闭" @click="close">✕</button>
        </div>
        <div class="ctx-tabs">
          <button :class="{ on: uiState.ctxTab === 'plan' }" @click="uiState.ctxTab = 'plan'">计划</button>
          <button :class="{ on: uiState.ctxTab === 'todo' }" @click="uiState.ctxTab = 'todo'">Todo</button>
          <button :class="{ on: uiState.ctxTab === 'art' }" @click="uiState.ctxTab = 'art'">Artifacts</button>
          <button :class="{ on: uiState.ctxTab === 'usage' }" @click="uiState.ctxTab = 'usage'">用量</button>
          <button class="tab-file" :class="{ on: uiState.ctxTab === 'file' }" @click="uiState.ctxTab = 'file'">文件</button>
        </div>
        <div class="ctx-body">
          <!-- 计划 -->
          <template v-if="uiState.ctxTab === 'plan'">
            <div class="ctx-sec">
              <h6>当前计划 · <span class="ok">{{ plan?.plan ? "已确认" : plan?.draft ? "草稿" : "无" }}</span></h6>
              <div v-if="plan?.plan" class="plan-block"><pre>{{ plan.plan }}</pre></div>
              <div v-else-if="plan?.draft" class="plan-block draft"><pre>{{ plan.draft }}</pre></div>
              <div v-else class="empty">（暂无计划）</div>
            </div>
          </template>
          <!-- Todo -->
          <template v-else-if="uiState.ctxTab === 'todo'">
            <div class="ctx-sec">
              <h6>Todo · 进行中 <b class="ok">{{ todoCounts.doing }}</b> / 待办 <b class="warn">{{ todoCounts.todo }}</b></h6>
              <div v-if="todos?.items?.length">
                <div v-for="item in todos.items" :key="item.id" class="todo-row">
                  <span class="t-status" :class="item.status">{{ item.status === "in_progress" ? "进行中" : item.status === "completed" ? "完成" : "待办" }}</span>
                  <span>{{ item.content }}</span>
                </div>
              </div>
              <div v-else class="empty">（暂无 Todo）</div>
            </div>
          </template>
          <!-- Artifacts -->
          <template v-else-if="uiState.ctxTab === 'art'">
            <div class="ctx-sec">
              <h6>Artifacts</h6>
              <div v-if="artifacts?.length">
                <div v-for="a in artifacts" :key="a.id" class="art-row">
                  <span class="art-id">artifact://{{ a.id }}</span>
                  <span class="art-tool">{{ a.tool ?? "" }}</span>
                </div>
              </div>
              <div v-else class="empty">（暂无 Artifacts）</div>
            </div>
          </template>
          <!-- 文件 -->
          <template v-else-if="uiState.ctxTab === 'file'">
            <FilePanel embedded :session-id="sessionId" />
          </template>
          <!-- 用量 -->
          <template v-else>
            <div class="ctx-sec">
              <h6>用量 · 当前会话</h6>
              <div class="ug">
                <div class="ug-group">
                  <div class="ug-title">模型</div>
                  <div class="metric">
                    <div class="m"><span>模型</span><b>{{ usage.model }}</b></div>
                    <div class="m"><span>轮次</span><b>{{ usage.turns }}</b></div>
                    <div class="m"><span>工具调用</span><b>{{ usage.toolCalls }}</b></div>
                  </div>
                </div>
                <div class="ug-group">
                  <div class="ug-title">Token</div>
                  <div class="metric">
                    <div class="m"><span>输入</span><b>{{ usage.in }}</b></div>
                    <div class="m"><span>输出</span><b>{{ usage.out }}</b></div>
                    <div class="m"><span>缓存命中</span><b>{{ usage.cache }}（{{ usage.cacheRate }}%）</b></div>
                  </div>
                </div>
                <div class="ug-group">
                  <div class="ug-title">上下文</div>
                  <div class="metric">
                    <div class="m"><span>当前</span><b>{{ usage.ctx }}<template v-if="usage.ctxPct !== null">（{{ usage.ctxPct }}%）</template></b></div>
                    <div class="m"><span>上限</span><b>{{ usage.maxCtx ?? "—" }}</b></div>
                  </div>
                </div>
                <div class="ug-group">
                  <div class="ug-title">费用</div>
                  <div class="metric">
                    <div class="m"><span>累计</span><b>{{ usage.cost }}</b></div>
                    <div class="m"><span>信念度</span><b>{{ usage.belief }}</b></div>
                  </div>
                </div>
              </div>
            </div>
          </template>
        </div>
      </aside>
    </transition>
  </Teleport>
</template>

<style scoped>
.ctx-mask { position: fixed; inset: 0; background: rgba(16, 24, 40, 0.32); z-index: 70; }
.ctx-panel {
  position: fixed; top: 0; right: 0; bottom: 0;
  width: min(380px, 92vw); z-index: 75;
  background: #fff; border-left: 1px solid var(--line);
  box-shadow: -20px 0 60px rgba(16, 24, 40, 0.14);
  display: flex; flex-direction: column;
}
.ctx-head { display: flex; align-items: center; padding: 18px 18px 10px; }
.ctx-title { font-weight: 700; font-size: 14.5px; }
.ctx-close { margin-left: auto; width: 30px; height: 30px; border-radius: 8px; border: none; background: none; cursor: pointer; font-size: 14px; color: var(--text-soft); }
.ctx-close:hover { background: var(--panel-2); }
.ctx-tabs { display: flex; gap: 4px; padding: 0 16px 12px; }
.ctx-tabs button {
  font-size: 12.5px; padding: 6px 13px; border-radius: 999px; border: none;
  background: none; color: var(--text-dim); font-weight: 500; cursor: pointer;
}
.ctx-tabs button.on { background: rgba(47, 111, 237, 0.09); color: var(--blue); font-weight: 600; }
/* 文件 tab 仅移动端显示（PC 用顶栏 📄 独立面板，避免面板内套面板可视区过小） */
.tab-file { display: none; }
@media (max-width: 640px) {
  .tab-file { display: inline-block; }
}
.ctx-body { flex: 1; overflow-y: auto; padding: 6px 18px 20px; }
.ctx-sec h6 { font-size: 10px; font-family: var(--mono); letter-spacing: 0.1em; color: var(--text-dim); text-transform: uppercase; margin: 14px 0 8px; }
.ctx-sec h6 .ok { color: var(--blue); }
.ctx-sec h6 .warn { color: var(--yellow, #b57a1c); }
.plan-block { border: 1px solid rgba(47, 111, 237, 0.3); background: rgba(47, 111, 237, 0.07); border-radius: 11px; padding: 12px 14px; }
.plan-block.draft { border-color: rgba(181, 122, 28, 0.35); background: rgba(181, 122, 28, 0.07); }
.plan-block pre { margin: 0; font-size: 12px; font-family: var(--mono); white-space: pre-wrap; color: var(--text-soft); }
.todo-row { display: flex; gap: 9px; padding: 8px 0; border-bottom: 1px dashed var(--line); font-size: 13px; align-items: baseline; }
.t-status { font-size: 10px; font-family: var(--mono); padding: 1px 8px; border-radius: 999px; flex-shrink: 0; }
.t-status.in_progress { background: rgba(47, 111, 237, 0.09); color: var(--blue); }
.t-status.completed { background: rgba(23, 154, 97, 0.1); color: var(--green); }
.t-status.pending { background: rgba(181, 122, 28, 0.1); color: #b57a1c; }
.art-row { display: flex; gap: 9px; padding: 8px 9px; border-radius: 8px; font-family: var(--mono); font-size: 11.5px; }
.art-row:hover { background: var(--panel-2); }
.art-id { color: var(--blue); }
.art-tool { color: var(--text-dim); }
.ug { display: flex; flex-direction: column; gap: 14px; }
.ug-group { border: 1px solid var(--line); border-radius: 10px; padding: 10px 13px; }
.ug-title { font-size: 10px; font-family: var(--mono); letter-spacing: 0.1em; text-transform: uppercase; color: var(--text-dim); margin-bottom: 6px; }
.metric { display: flex; flex-direction: column; gap: 7px; font-size: 12px; font-family: var(--mono); }
.metric .m { display: flex; justify-content: space-between; padding: 4px 2px; border-bottom: 1px dashed var(--line); color: var(--text-dim); }
.metric .m b { font-weight: 600; color: var(--text-soft); }
.empty { color: var(--text-dim); font-size: 12.5px; padding: 12px 0; }
.panel-enter-active, .panel-leave-active { transition: transform 0.25s cubic-bezier(0.3, 0.9, 0.4, 1), opacity 0.25s; }
.panel-enter-from, .panel-leave-to { transform: translateX(104%); opacity: 0; }
</style>
