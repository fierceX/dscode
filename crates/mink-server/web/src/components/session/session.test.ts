// Vue 组件测试：ToolCard 分类型渲染 + 折叠策略 + 流式 markdown

import { describe, expect, it, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";
import ToolCard from "./ToolCard.vue";
import ThinkingBlock from "./ThinkingBlock.vue";
import { appState, attachSession } from "../../lib/store";
import type { ToolItem, ThinkingItem } from "../../lib/types";

const session = () => ({ project_key: "proj", corrupt: false, id: "s1", title: "t", alias: null, cwd: "/tmp", created_at: "", updated_at: "", modified_secs: 0, status: "free" as const, path: "" });

beforeEach(() => {
  appState.currentSessionId = null;
  appState.sessionState = null;
  vi.restoreAllMocks();
});

describe("ToolCard", () => {
  it("Bash 卡片：着色类/摘要/CMD 徽章/退出码/命令条", () => {
    attachSession(session());
    appState.sessionState!.running = false;
    const item: ToolItem = {
      kind: "tool", id: "c1", name: "Bash", color: "exec", view: "command",
      summary: "ls -la", input: "{}",
      result: "src/\ntarget/", resultKind: "CMD", success: true, exitCode: 0, failed: false, key: 1,
    };
    const w = mount(ToolCard, { props: { item } });
    expect(w.classes()).toContain("tc-exec");
    expect(w.text()).toContain("Bash");
    expect(w.text()).toContain("ls -la");
    expect(w.text()).toContain("CMD");
    expect(w.text()).toContain("exit 0");
    expect(w.text()).toContain("src/");
  });

  it("工具卡片始终折叠（即使 running 中），头部 summary 显示核心参数", () => {
    attachSession(session());
    appState.sessionState!.running = true; // turn 进行中
    const item: ToolItem = {
      kind: "tool", id: "c1", name: "Glob", color: "search", view: "search",
      summary: "*.rs", input: "*.rs", key: 1,
    };
    const w = mount(ToolCard, { props: { item } });
    expect(w.find("details").attributes("open")).toBeUndefined(); // 折叠
    expect(w.text()).toContain("*.rs"); // 核心参数在头部可见
  });

  it("失败结果：err 类 + tc-failed", () => {
    attachSession(session());
    const item: ToolItem = {
      kind: "tool", id: "c1", name: "Grep", color: "search", view: "search",
      summary: "TODO src", input: "{}",
      result: "Error: nothing found", resultKind: "SEARCH", success: false, failed: true, key: 1,
    };
    const w = mount(ToolCard, { props: { item } });
    expect(w.classes()).toContain("tc-failed");
    expect(w.find(".t-result").classes()).toContain("err");
  });

  it("TodoWrite：markdown 任务列表 + presentation 变更摘要", () => {
    attachSession(session());
    const item: ToolItem = {
      kind: "tool", id: "c1", name: "TodoWrite", color: "todo", view: "todo",
      summary: "更新 Todo +1", input: "{}",
      result: "- [x] 任务一\n- [ ] 任务二",
      resultKind: "CTRL", success: true, failed: false, key: 1,
      presentation: { data: { changes: [{ change: "completed" }, { change: "added" }] } },
    };
    const w = mount(ToolCard, { props: { item } });
    expect(w.text()).toContain("新增 1 · 完成 1");
    expect(w.text()).toContain("任务一");
    expect(w.text()).toContain("任务二");
  });

  it("Edit 调用：hunk 头 + 内容行", () => {
    attachSession(session());
    const item: ToolItem = {
      kind: "tool", id: "c1", name: "Edit", color: "file", view: "diff",
      summary: "demo.txt", input: "{}", key: 1,
    };
    // 无结果时 diff view 渲染 EditCall（非原始 .t-call）
    const w = mount(ToolCard, { props: { item } });
    expect(w.find(".e-raw").exists()).toBe(true);
  });
});

describe("ThinkingBlock 折叠策略", () => {
  it("idle 折叠 / running 展开 / 结束折叠", async () => {
    attachSession(session());
    const item: ThinkingItem = { kind: "thinking", text: "第一段" };
    const w = mount(ThinkingBlock, { props: { item } });
    expect(w.find("details").attributes("open")).toBeUndefined();
    appState.sessionState!.running = true;
    await flushPromises();
    expect(w.find("details").attributes("open")).toBeDefined();
    appState.sessionState!.running = false;
    await flushPromises();
    expect(w.find("details").attributes("open")).toBeUndefined();
  });

  it("流式推进（后续 text 到达）自动折叠思考块", async () => {
    attachSession(session());
    appState.sessionState!.running = true;
    const item: ThinkingItem = { kind: "thinking", text: "思考中", key: 1 };
    const w = mount(ThinkingBlock, { props: { item } });
    expect(w.find("details").attributes("open")).toBeDefined(); // 展开
    // 追加 text 事件 → 最后项不再是 thinking
    appState.sessionState!.items = [
      { kind: "thinking", text: "思考中", key: 1 },
      { kind: "text", text: "回答", key: 2 },
    ] as any;
    await flushPromises();
    expect(w.find("details").attributes("open")).toBeUndefined(); // 已折叠
  });

  it("markdown 渲染思考内容", () => {
    attachSession(session());
    appState.sessionState!.running = true;
    const item: ThinkingItem = { kind: "thinking", text: "# 标题\n**粗体**" };
    const w = mount(ThinkingBlock, { props: { item } });
    expect(w.find(".tp-body h1").text()).toBe("标题");
    expect(w.find(".tp-body strong").text()).toBe("粗体");
  });
});
