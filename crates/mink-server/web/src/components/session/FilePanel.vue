<script setup lang="ts">
// 文件预览面板：左目录树（导航）+ 右预览（md 渲染 + 代码着色，仅预览不可编辑）
import { computed, ref, watch } from "vue";
import { api } from "../../lib/api";
import type { FileItem } from "../../lib/api";
import { uiState } from "../../lib/store";
import { renderMarkdown } from "../../lib/markdown";

const props = defineProps<{ sessionId: string; embedded?: boolean }>();
const path = ref("");
const items = ref<FileItem[]>([]);
const error = ref("");
const current = ref<{ name: string; lang: string; content: string } | null>(null);
const loading = ref(false);

const crumb = computed(() => {
  const parts = path.value.split("/").filter(Boolean);
  const out: { name: string; path: string }[] = [{ name: "根目录", path: "" }];
  let acc = "";
  for (const p of parts) { acc = [acc, p].filter(Boolean).join("/"); out.push({ name: p, path: acc }); }
  return out;
});

async function load() {
  error.value = "";
  loading.value = true;
  const resp = await api.files(props.sessionId, path.value);
  loading.value = false;
  if (resp.code !== 200) { error.value = resp.message; items.value = []; }
  else items.value = resp.data?.items ?? [];
}
const enter = (name: string) => { path.value = [path.value, name].filter(Boolean).join("/"); load(); };
const go = (p: string) => { path.value = p; load(); };

async function openFile(name: string) {
  const filePath = [path.value, name].filter(Boolean).join("/");
  const resp = await api.files(props.sessionId, filePath, true);
  current.value = {
    name: filePath,
    lang: /\.(md|markdown)$/i.test(name) ? "md" : /\.rs$/i.test(name) ? "rust" : /\.(ts|tsx)$/i.test(name) ? "ts" : /\.(css|scss)$/i.test(name) ? "css" : "text",
    content: resp.code === 200 ? (resp.data?.content ?? "") : `读取失败: ${resp.message}`,
  };
}

const close = () => { uiState.fileOpen = false; };
watch(() => props.sessionId, () => { path.value = ""; current.value = null; load(); }, { immediate: true });

/* 迷你代码着色（先 escape 防 XSS；关键字/字符串/注释/数字/类型） */
function highlight(src: string): string {
  const e = src.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  return e
    .replace(/(\/\/.*$|\/\*[\s\S]*?\*\/|#.*$)/gm, '<span class="h-c">$1</span>')
    .replace(/("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)/g, '<span class="h-s">$1</span>')
    .replace(/\b(import|from|export|const|let|var|function|return|class|extends|new|async|await|if|else|for|while|use|pub|fn|struct|impl|match|self|type|where|as|mut|true|false|null|undefined)\b/g, '<span class="h-k">$1</span>')
    .replace(/\b(\d+\.?\d*)\b/g, '<span class="h-n">$1</span>')
    .replace(/\b([A-Z][A-Za-z0-9_]*)\b/g, '<span class="h-f">$1</span>');
}

/* md：marked 渲染后对代码块二次着色（代码文本来自 DOM textContent，安全重建） */
function renderMdWithHighlight(content: string): string {
  const html = renderMarkdown(content);
  const div = document.createElement("div");
  div.innerHTML = html;
  for (const pre of div.querySelectorAll("pre code")) {
    pre.innerHTML = highlight(pre.textContent ?? "");
  }
  return div.innerHTML;
}
</script>

<template>
  <Teleport v-if="!embedded" to="body">
    <transition name="fp">
      <div v-if="uiState.fileOpen" class="fp-mask" @click="close"></div>
    </transition>
    <transition name="fp">
      <aside v-if="uiState.fileOpen" class="fp-panel">
        <div class="fp-head">
          <span class="fp-title">文件预览</span>
          <span class="fp-path">{{ path || "根目录" }}</span>
          <button class="fp-close" title="关闭" @click="close">✕</button>
        </div>
        <div class="fp-body">
          <div class="fp-tree">
            <div v-if="error" class="fp-empty">{{ error }}</div>
            <div v-else-if="loading" class="fp-empty">加载中…</div>
            <template v-else>
              <div v-if="path" class="f-back" role="button" tabindex="0" @click="go(crumb[crumb.length - 2].path)">← 上级目录</div>
              <div v-for="item in items" :key="item.name" class="f-row" role="button" tabindex="0"
                :class="{ 'f-dir': item.dir, 'f-file': !item.dir, on: current?.name.endsWith('/' + item.name) }"
                @click="item.dir ? enter(item.name) : openFile(item.name)">
                <span class="f-ico">{{ item.dir ? "📁" : /\.(md|markdown)$/i.test(item.name) ? "📄" : /\.rs$/i.test(item.name) ? "🦀" : /\.(ts|tsx)$/i.test(item.name) ? "🟦" : "🎨" }}</span>
                {{ item.name }}
              </div>
              <div v-if="items.length === 0" class="fp-empty">（空目录）</div>
            </template>
          </div>
          <div class="fp-preview">
            <div v-if="!current" class="fp-empty">← 从左侧选择一个文件预览</div>
            <template v-else>
              <div v-if="current.lang === 'md'" class="fp-md" v-html="renderMdWithHighlight(current.content)"></div>
              <pre v-else class="fp-code" v-html="highlight(current.content)"></pre>
            </template>
          </div>
        </div>
      </aside>
    </transition>
  </Teleport>
  <!-- 嵌入模式（上下文面板文件 tab）：无 mask/独立头 -->
  <div v-else class="fp-embed">
            <div class="fp-body">
      <div class="fp-tree">
        <div v-if="error" class="fp-empty">{{ error }}</div>
        <div v-else-if="loading" class="fp-empty">加载中…</div>
        <template v-else>
          <div v-if="path" class="f-back" role="button" tabindex="0" @click="go(crumb[crumb.length - 2].path)">← 上级目录</div>
          <div v-for="item in items" :key="item.name" class="f-row" role="button" tabindex="0"
            :class="{ 'f-dir': item.dir, 'f-file': !item.dir, on: current?.name.endsWith('/' + item.name) }"
            @click="item.dir ? enter(item.name) : openFile(item.name)">
            <span class="f-ico">{{ item.dir ? "📁" : /\.(md|markdown)$/i.test(item.name) ? "📄" : /\.rs$/i.test(item.name) ? "🦀" : /\.(ts|tsx)$/i.test(item.name) ? "🟦" : "🎨" }}</span>
            {{ item.name }}
          </div>
          <div v-if="items.length === 0" class="fp-empty">（空目录）</div>
        </template>
      </div>
      <div class="fp-preview">
        <div v-if="!current" class="fp-empty">← 从左侧选择一个文件预览</div>
        <template v-else>
          <div v-if="current.lang === 'md'" class="fp-md" v-html="renderMdWithHighlight(current.content)"></div>
          <pre v-else class="fp-code" v-html="highlight(current.content)"></pre>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.fp-mask { position: fixed; inset: 0; background: rgba(16, 24, 40, 0.32); z-index: 70; }
.fp-panel {
  position: fixed; top: 0; right: 0; bottom: 0;
  width: min(760px, 94vw); z-index: 75;
  background: #fff; border-left: 1px solid var(--line);
  box-shadow: -20px 0 60px rgba(16, 24, 40, 0.16);
  display: flex; flex-direction: column;
}
.fp-head { display: flex; align-items: center; gap: 10px; padding: 16px 18px 10px; }
.fp-title { font-weight: 700; font-size: 14.5px; }
.fp-path { font-size: 11px; font-family: var(--mono); color: var(--text-dim); background: var(--panel-2); padding: 2px 9px; border-radius: 6px; flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.fp-close { margin-left: auto; width: 30px; height: 30px; border-radius: 8px; border: none; background: none; cursor: pointer; font-size: 14px; color: var(--text-soft); }
.fp-close:hover { background: var(--panel-2); }
.fp-body { flex: 1; display: grid; grid-template-columns: 200px minmax(0, 1fr); min-height: 0; }
.fp-tree { overflow-y: auto; border-right: 1px solid var(--line); padding: 6px 8px; background: var(--panel-2); }
.f-back { padding: 6px 9px; font-size: 12px; color: var(--blue); cursor: pointer; border-radius: 7px; margin-bottom: 2px; }
.f-back:hover { background: var(--panel-3); }
.f-row { display: flex; align-items: center; gap: 8px; padding: 7px 9px; border-radius: 8px; cursor: pointer; font-size: 12.5px; font-family: var(--mono); color: var(--text-soft); }
.f-row:hover { background: var(--panel-3); color: var(--text); }
.f-row.on { background: rgba(47, 111, 237, 0.08); color: var(--blue); font-weight: 600; }
.f-ico { font-size: 13px; width: 16px; text-align: center; }
.fp-preview { overflow-y: auto; padding: 20px 24px; background: #fbfcfd; }
.fp-empty { color: var(--text-dim); font-size: 13px; text-align: center; padding: 50px 20px; }
.fp-md { max-width: 640px; margin: 0 auto; font-size: 14px; line-height: 1.75; color: var(--text); }
.fp-md :deep(h1) { font-size: 21px; margin: 4px 0 14px; letter-spacing: -0.01em; padding-bottom: 9px; border-bottom: 1px solid var(--line); }
.fp-md :deep(h2) { font-size: 16px; margin: 20px 0 8px; }
.fp-md :deep(h3) { font-size: 14px; margin: 16px 0 6px; }
.fp-md :deep(p) { margin: 8px 0; }
.fp-md :deep(ul) { margin: 8px 0 8px 22px; }
.fp-md :deep(li) { margin: 3px 0; }
.fp-md :deep(code) { font-family: var(--mono); font-size: 12px; background: var(--panel-3); padding: 1.5px 6px; border-radius: 5px; }
.fp-md :deep(pre) { background: #f3f5f8; border: 1px solid var(--line); border-radius: 10px; padding: 12px 15px; overflow-x: auto; margin: 10px 0; }
.fp-md :deep(pre code) { background: none; padding: 0; display: block; font-size: 12px; line-height: 1.6; color: var(--text-soft); }
.fp-md :deep(blockquote) { border-left: 3px solid var(--blue); padding-left: 13px; color: var(--text-dim); margin: 8px 0; }
.fp-code { margin: 0; font-family: var(--mono); font-size: 12.5px; line-height: 1.7; color: var(--text-soft); overflow-x: auto; }
.fp-code :deep(.h-k) { color: #7c3aed; font-weight: 600; }
.fp-code :deep(.h-s) { color: #179a61; }
.fp-code :deep(.h-c) { color: #8b96a8; font-style: italic; }
.fp-code :deep(.h-n) { color: #b45309; }
.fp-code :deep(.h-f) { color: #1d4ed8; }
.fp-enter-active, .fp-leave-active { transition: transform 0.25s cubic-bezier(0.3, 0.9, 0.4, 1), opacity 0.25s; }
.fp-enter-from, .fp-leave-to { transform: translateX(104%); opacity: 0; }
@media (max-width: 640px) {
  .fp-body { grid-template-columns: 150px minmax(0, 1fr); }
  .fp-preview { padding: 14px; }
}
</style>
