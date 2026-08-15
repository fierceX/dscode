import { describe, expect, it } from "vitest";
import { reduceEvent, prependEvents } from "./reducer";
import { conversationToEvents, parsePatch } from "./toolFormat";
import { fmtK } from "./fmt";
import { emptySession } from "./types";
import protocolFixture from "../../../protocol-fixtures/agent-events.json";

const ev = (type: string, extra: Record<string, unknown> = {}) => ({ type, ...extra });

describe("reduceEvent", () => {
  it("thinking 流式片段合并为一段", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("thinking", { content: "第一段" }));
    s = reduceEvent(s, ev("thinking", { content: "第二段" }));
    expect(s.items).toHaveLength(1);
    expect(s.items[0]).toMatchObject({ kind: "thinking", text: "第一段第二段" });
  });

  it("text 流式片段合并，thinking/text 互不串扰", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("thinking", { content: "思考" }));
    s = reduceEvent(s, ev("text", { content: "回答" }));
    s = reduceEvent(s, ev("text", { content: "继续" }));
    expect(s.items.map((i) => i.kind)).toEqual(["thinking", "text"]);
    expect(s.items[1]).toMatchObject({ kind: "text", text: "回答继续" });
  });

  it("tool_call 建卡（着色/摘要），tool_result 按 tool_use_id 填充", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Bash", id: "c1", input: { command: "ls" } }));
    s = reduceEvent(
      s,
      ev("tool_result", { tool_use_id: "c1", name: "Bash", content: "src/\n", result_kind: "Command", success: true, exit_code: 0 }),
    );
    expect(s.items).toHaveLength(1);
    const tool = s.items[0];
    expect(tool.kind).toBe("tool");
    if (tool.kind === "tool") {
      expect(tool.name).toBe("Bash");
      expect(tool.color).toBe("exec");
      expect(tool.summary).toBe("ls");
      expect(tool.result).toBe("src/\n");
      expect(tool.resultKind).toBe("CMD");
      expect(tool.exitCode).toBe(0);
      expect(tool.failed).toBe(false);
    }
  });

  it("tool_result 失败态（success=false）", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Grep", id: "c1", input: { pattern: "x" } }));
    s = reduceEvent(s, ev("tool_result", { tool_use_id: "c1", content: "Error: not found", success: false }));
    const tool = s.items[0];
    if (tool.kind === "tool") expect(tool.failed).toBe(true);
  });

  it("实时 tool_result 缺少 tool_use_id 时 fail closed，旧重放仍兼容", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Read", id: "c1", input: {} }));
    s = reduceEvent(s, ev("tool_result", { stream_sequence: 2, content: "must not attach" }));
    expect((s.items[0] as { result?: string }).result).toBeUndefined();
    s = reduceEvent(s, ev("tool_result", { seq: 3, content: "legacy replay" }));
    expect((s.items[0] as { result?: string }).result).toBe("legacy replay");
  });

  it("跨工具间隔：结果填充最近未完成的工具", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Glob", id: "c1", input: { pattern: "*.rs" } }));
    s = reduceEvent(s, ev("tool_call", { name: "Bash", id: "c2", input: { command: "ls" } }));
    s = reduceEvent(s, ev("tool_result", { tool_use_id: "c2", content: "out" }));
    const items = s.items;
    expect((items[0] as { result?: string }).result).toBeUndefined();
    expect((items[1] as { result?: string }).result).toBe("out");
  });

  it("turn 生命周期：running 置位与复位", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("turn_started"));
    expect(s.running).toBe(true);
    s = reduceEvent(s, ev("turn_final", { outcome: { status: "ok", error: null } }));
    expect(s.running).toBe(false);
  });

  it("中断语义：stop reason=interrupted", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("stop", { reason: "interrupted" }));
    expect(s.items.at(-1)).toMatchObject({ kind: "system", text: "— 已中断 —" });
  });

  it("usage 累计 tokens", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("usage", { input_tokens: 10, output_tokens: 20 }));
    s = reduceEvent(s, ev("usage", { input_tokens: 5, output_tokens: 3 }));
    expect(s.tokensIn).toBe(15);
    expect(s.tokensOut).toBe(23);
  });

  it("sub-agent status/output 合并为结构化 transcript item", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("sub_agent_status", { session_id: "abc12345", status: "running", in_tokens: 4 }));
    s = reduceEvent(s, ev("sub_agent_output", { session_id: "abc12345", status: "completed", thinking: "t", text: "ok", in_tokens: 4, out_tokens: 2 }));
    expect(s.items).toHaveLength(1);
    expect(s.items[0]).toMatchObject({ kind: "sub_agent", sessionId: "abc12345", status: "completed", thinking: "t", text: "ok", inTokens: 4, outTokens: 2 });
  });

  it("sub-agent output 后的 final status 保留 thinking/text", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("sub_agent_output", { session_id: "abc12345", status: "completed", thinking: "t", text: "ok", in_tokens: 4, out_tokens: 2 }));
    s = reduceEvent(s, ev("sub_agent_status", { session_id: "abc12345", status: "ok", in_tokens: 4, out_tokens: 2 }));
    expect(s.items[0]).toMatchObject({ kind: "sub_agent", status: "ok", thinking: "t", text: "ok" });
  });

  it("title_update 严格读取嵌套 stats", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("title_update", { model: "pro", stats: { total_input_tokens: 7, total_output_tokens: 3, total_cache_read_tokens: 5, current_context_tokens: 12, max_context_tokens: 100, cost: { known_nano_cny: 6000, unpriced_requests: 2 }, belief: 0.8 } }));
    expect(s).toMatchObject({ model: "pro", tokensIn: 7, tokensOut: 3, cacheReadTokens: 5, contextTokens: 12, maxContextTokens: 100, costMicros: 6, unpricedRequests: 2, belief: 0.8 });
  });

  it("stop 插入唯一结束标记，turn_final 只提交权威状态", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("turn_started"));
    s = reduceEvent(s, ev("stop", { reason: "end_turn" }));
    s = reduceEvent(s, ev("turn_final", { outcome: { status: "ok", error: null } }));
    expect(s.items.filter((item) => item.kind === "system")).toHaveLength(1);
    expect(s.running).toBe(false);
  });

  it("系统事件（tool_surface 等）不渲染", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_surface", { active: [] }));
    s = reduceEvent(s, ev("tool_capability_resolution", { bindings: [] }));
    s = reduceEvent(s, ev("prompt_workflow_resolution", { active_workflows: [] }));
    s = reduceEvent(s, ev("session_start", { session_id: "s1" }));
    expect(s.items).toHaveLength(0);
  });

  it("artifact 提取", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Grep", id: "c1", input: {} }));
    s = reduceEvent(s, ev("tool_result", { tool_use_id: "c1", content: "很长... artifact://grep-0001" }));
    const tool = s.items[0];
    if (tool.kind === "tool") expect(tool.artifact).toBe("grep-0001");
  });
});

describe("共享 Core/SSE 协议 fixture", () => {
  it("覆盖并发工具结果、artifact/presentation、sub-agent 与 final outcome", () => {
    let s = emptySession("fixture", "fixture");
    for (const [index, event] of protocolFixture.entries()) {
      const raw = event.type === "final"
        ? { ...event, type: "turn_final", stream_sequence: index + 100 }
        : { ...event, stream_sequence: index + 100 };
      s = reduceEvent(s, raw as never);
    }
    const tools = s.items.filter((item) => item.kind === "tool");
    expect(tools).toHaveLength(2);
    expect(tools[0]).toMatchObject({ id: "read-1", success: true, presentation: { kind: "todo" } });
    expect(tools[1]).toMatchObject({ id: "grep-1", artifact: "grep-0001", artifacts: [{ id: "grep-0001" }] });
    expect(s.items.find((item) => item.kind === "sub_agent")).toMatchObject({ status: "ok", text: "done" });
    expect(s.running).toBe(false);
    expect(s.lastSeq).toBe(110);
  });
});

describe("prependEvents（懒加载前插）", () => {
  it("新批前插不破坏顺序，首项 key 更新为更早 seq", () => {
    let s = emptySession("s1", "t");
    // 当前状态：尾部内容（seq 100..102）
    s = reduceEvent(s, ev("text", { content: "旧", seq: 100 }));
    s = reduceEvent(s, ev("text", { content: "更旧", seq: 101 }));
    // 新批（seq 90..92）前插
    s = prependEvents(s, [
      ev("user_input", { content: "q", seq: 90 }),
      ev("tool_call", { name: "Bash", id: "c1", input: {}, seq: 91 }),
      ev("tool_result", { tool_use_id: "c1", content: "out", seq: 92 }),
    ] as never);
    expect(s.items[0]).toMatchObject({ kind: "user", key: 90 });
    const keys = s.items.map((i) => (i as { key?: number }).key);
    expect(keys[0]).toBe(90);
    // tool_result 配对到同批 tool_call（不新增项）；100/101 流式合并为一条
    expect(keys).toEqual([90, 91, 100]);
    expect((s.items[1] as { result?: string }).result).toBe("out");
  });
});

describe("实时 key 命名空间", () => {
  it("历史 seq 与实时 stream_sequence 相同也不会产生 keyed-list 冲突", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("user_input", { content: "history", seq: 1 }));
    s = reduceEvent(s, ev("text", { content: "live", stream_sequence: 1 }));
    expect(s.items.map((item) => item.key)).toEqual([1, "live:1"]);
    expect(s.lastSeq).toBe(1);
  });

  it("使用 turn_id + core sequence 去重并保留诊断坐标", () => {
    let s = emptySession("s1", "t");
    const live = ev("text", { content: "once", turn_id: "turn-7", sequence: 4, stream_sequence: 22 });
    s = reduceEvent(s, live);
    s = reduceEvent(s, live);
    expect(s.items).toHaveLength(1);
    expect(s.items[0]).toMatchObject({ text: "once", key: "live:22" });
    expect(s.lastTurnId).toBe("turn-7");
    expect(s.lastCoreSequence).toBe(4);
  });
});

describe("conversationToEvents（完整轮次 → 事件）", () => {
  it("assistant 消息展开为 thinking/text/tool_call，key 唯一", () => {
    
    const events = conversationToEvents({
      seq: 10,
      role: "assistant",
      content: [
        { type: "thinking", thinking: "思考" },
        { type: "text", text: "回答" },
        { type: "tool_use", id: "c1", name: "Bash", input: { command: "ls" } },
      ],
    });
    expect(events.map((e) => e.type)).toEqual(["thinking", "text", "tool_call"]);
    const keys = events.map((e) => e.seq);
    expect(new Set(keys).size).toBe(3); // 1000,1001,1002 唯一
    expect(events[2]).toMatchObject({ name: "Bash", id: "c1" });
  });

  it("user 消息：文本 → user_input；tool_result 数组 → tool_result", () => {
    
    const e1 = conversationToEvents({ seq: 11, role: "user", content: "你好" });
    expect(e1).toHaveLength(1);
    expect(e1[0]).toMatchObject({ type: "user_input", content: "你好" });
    const e2 = conversationToEvents({
      seq: 12,
      role: "user",
      content: [{ type: "tool_result", tool_use_id: "c1", content: "out" }],
    });
    expect(e2[0]).toMatchObject({ type: "tool_result", tool_use_id: "c1", content: "out" });
  });
});

describe("实时 AgentEvent 流（无 input/带 summary）", () => {
  it("tool_call 用 summary 填充头部与调用区（不显示 {}）", () => {
    let s = emptySession("s1", "t");
    // 模拟服务端广播帧（AgentEvent → JSON）
    s = reduceEvent(s, ev("tool_call", { name: "Glob", summary: "*.rs", seq: 1 }));
    const tool = s.items[0];
    expect(tool.kind).toBe("tool");
    if (tool.kind === "tool") {
      expect(tool.summary).toBe("*.rs");
      expect(tool.input).toBe("*.rs"); // input 缺失 → summary 兜底
    }
    // tool_result（无 result_kind/success → 按 name 兜底 kind）
    s = reduceEvent(s, ev("tool_result", { name: "Glob", content: "src/lib.rs", exit_code: 0, seq: 2 }));
    const updated = s.items[0];
    if (updated.kind === "tool") {
      expect(updated.resultKind).toBe("SEARCH");
      expect(updated.result).toContain("src/lib.rs");
    }
  });
});

describe("parsePatch（Edit patch 解析）", () => {

  it("标准格式：@path#tag + replace hunk + add/del 行", () => {
    const p = parsePatch("@src/App.vue#7FA9\nreplace 5..5:\n+  .x { color: red }\n-  .y { color: blue }");
    expect(p).not.toBeNull();
    expect(p!.path).toBe("src/App.vue");
    expect(p!.tag).toBe("7FA9");
    expect(p!.lines[0]).toMatchObject({ op: "replace", range: "5..5:" });
    expect(p!.lines[1]).toMatchObject({ op: "add", content: "  .x { color: red }" });
    expect(p!.lines[2]).toMatchObject({ op: "del", content: "  .y { color: blue }" });
  });

  it("insert after / delete 单行变体", () => {
    const p = parsePatch("@src/a.rs#0A3B\ninsert after 5:\n+fn f() {}\ndelete 3..5");
    expect(p!.lines[0]).toMatchObject({ op: "insert", range: "after 5:" });
    expect(p!.lines[2]).toMatchObject({ op: "delete", range: "3..5" });
  });

  it("缺 header：path 为空但 lines 仍解析（path 由 input.path 提供）", () => {
    const p = parsePatch("replace 2..2:\n+new\n+line");
    expect(p).not.toBeNull();
    expect(p!.path).toBe("");
    expect(p!.lines).toHaveLength(2);
  });

  it("空 patch / 非 patch 文本：返回 null（fail-closed 走 raw）", () => {
    expect(parsePatch("")).toBeNull();
    expect(parsePatch("只是一个普通文本")).toBeNull();
    expect(parsePatch("   ")).toBeNull();
  });

  it("原始 diff 风格（---/+++ 头 + 变更行）容错解析", () => {
    const p = parsePatch("--- a/src/x.ts\n+++ b/src/x.ts\n@@ -1,3 +1,3 @@\n-old\n+new");
    expect(p).not.toBeNull();
    expect(p!.lines.filter((l: any) => l.op === "add" || l.op === "del").length).toBeGreaterThan(0);
  });
});

describe("usage 事件 → 缓存命中与上下文", () => {
  it("累计缓存 tokens 并更新 context/max_context", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("usage", {
      input_tokens: 100, output_tokens: 20,
      cache_read_input_tokens: 80, cache_creation_input_tokens: 10,
      context_tokens: 8123, max_context: 65536,
    }));
    expect(s.cacheReadTokens).toBe(90);
    expect(s.contextTokens).toBe(8123);
    expect(s.maxContextTokens).toBe(65536);
    // 二次事件继续累计缓存
    s = reduceEvent(s, ev("usage", { input_tokens: 50, cache_read_input_tokens: 10 }));
    expect(s.cacheReadTokens).toBe(100);
  });

  it("缺失 cache/context 字段时保持原值", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("usage", { input_tokens: 10 }));
    expect(s.cacheReadTokens).toBe(0);
    expect(s.contextTokens).toBe(0);
  });
});

describe("实时 tool_call 带 input → Edit 结构化渲染链路", () => {
  it("tool_call input 对象保留完整 patch，EditCall 可解析", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", {
      name: "Edit",
      id: "call_x",
      input: {
        path: "src/a.rs",
        patch: "@src/a.rs#0A3B\nreplace 1..1:\n+fn f() {}\n",
      },
    }));
    const tool = s.items.find((i) => i.kind === "tool");
    expect(tool).toBeDefined();
    const input = (tool as any).input as string;
    expect(input).toContain('"patch"');
    expect(input).toContain("@src/a.rs#0A3B");
    // 与 parsePatch 链路联通
  
    const obj = JSON.parse(input);
    expect(parsePatch(obj.patch)).not.toBeNull();
  });

  it("无 input 的 tool_call（summary 兜底）不崩溃", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Read", id: "c2" }));
    const tool = s.items.find((i) => i.kind === "tool");
    expect(tool).toBeDefined();
  });
});

describe("parsePatch unified diff 兼容（用户实测场景）", () => {
  it("---/+++/@@ 头转为结构化行，不再原样显示", () => {
    const p = parsePatch(
      "--- a/Users/x/a.ts\n+++ b/Users/x/a.ts\n@@ -120,6 +120,8 @@\n .ctx { color: red }\n+.load-hint { color: blue }\n-.old { color: gray }",
    );
    expect(p).not.toBeNull();
    const ops = p!.lines.map((l) => l.op);
    expect(ops[0]).toBe("head"); // --- 头
    expect(ops[1]).toBe("head"); // +++ 头
    expect(ops[2]).toBe("hunk"); // @@ 头 → hunk 徽标
    expect(ops).toContain("add");
    expect(ops).toContain("del");
    // ---/+++/@@ 已结构化（context 仅剩空格开头的 diff 上下文行）
    const rawHead = p!.lines.filter((l) => l.op === "context" && /^(---|\+\+\+|@@)/.test(l.content));
    expect(rawHead).toEqual([]);
  });
});

describe("parsePatch 带行号快照行（用户实测场景 2）", () => {
  it("N: 前缀剥离行号，作为内容行渲染", () => {
    const p = parsePatch(
      "@crates/x/EditCall.vue#4123\n31: <pre v-else>raw</pre>\n32:</template>\n--- a/Users/x/a.vue\n+++ b/Users/x/a.vue\n@@ -40,8 +40,9 @@\n .e-hunk { color: blue }\n+.e-head { color: gray }\n-.e-old { color: red }",
    );
    expect(p).not.toBeNull();
    expect(p!.path).toContain("EditCall.vue");
    // 行号行 → context 且内容无行号前缀
    const lineNo = p!.lines.find((l) => l.content.includes("<pre v-else>"));
    expect(lineNo).toBeDefined();
    expect(lineNo!.op).toBe("context");
    expect(lineNo!.content).not.toMatch(/^\d+: /);
    // ---/+++/@@ 结构化
    const ops = p!.lines.map((l) => l.op);
    expect(ops).toContain("head");
    expect(ops).toContain("hunk");
    expect(ops).toContain("add");
    expect(ops).toContain("del");
  });
});

describe("fmtK 向上换算（k/M/G）", () => {
  it("各级换算", () => {
    expect(fmtK(999)).toBe("999");
    expect(fmtK(1500)).toBe("1.5k");
    expect(fmtK(1234000)).toBe("1.23M");
    expect(fmtK(2500000000)).toBe("2.5G");
    expect(fmtK(1000)).toBe("1k");
    expect(fmtK(1000000)).toBe("1M");
  });
});

describe("parsePatch 新协议（[PATH#TAG] + PUT/CUT）", () => {
  it("用户实测：{input: \"[path#tag]\\nPUT N.=M:\\n+行\"}", () => {
    const p = parsePatch(
      "[crates/mink-server/src/session/config.rs#8EBC]\nPUT 23.=34:\n+impl ServerConfig {\n+    pub fn load() {}\n+}",
    );
    expect(p).not.toBeNull();
    expect(p!.path).toBe("crates/mink-server/src/session/config.rs");
    expect(p!.tag).toBe("8EBC");
    expect(p!.lines[0]).toMatchObject({ op: "put", range: "23.=34:" });
    expect(p!.lines[1]).toMatchObject({ op: "add", content: "impl ServerConfig {" });
  });

  it("PUT >N 后插 / PUT <N 前插 / CUT 删除", () => {
    const p = parsePatch(
      "[src/a.ts#0A3B]\nPUT >5:\n+  new\nCUT 10.=12\nPUT <1:\n+head",
    );
    expect(p!.lines.map((l) => l.op)).toEqual(["put", "add", "cut", "put", "add"]);
    expect(p!.lines[0].range).toBe(">5:");
    expect(p!.lines[2].range).toBe("10.=12");
    expect(p!.lines[3].range).toBe("<1:");
  });

  it("旧协议 @path#tag + replace 仍兼容", () => {
    const p = parsePatch("@src/a.rs#0A3B\nreplace 2..2:\n+x");
    expect(p!.lines[0]).toMatchObject({ op: "replace", range: "2..2:" });
  });
});

describe("tool_result 更新同一卡片（滚动触发前置）", () => {
  it("tool_call append 后 tool_result 更新同一 item 的 result 字段", () => {
    let s = emptySession("s1", "t");
    s = reduceEvent(s, ev("tool_call", { name: "Read", id: "c1", input: {} }));
    const tool = s.items[s.items.length - 1];
    expect((tool as any).kind).toBe("tool");
    const keyBefore = (tool as any).key;
    const textBefore = (tool as any).text;
    s = reduceEvent(s, ev("tool_result", { tool_use_id: "c1", content: "file content" }));
    const updated = s.items[s.items.length - 1];
    // 同一 item：key/text 不变，result 变化（watch 需感知 result）
    expect((updated as any).key).toBe(keyBefore);
    expect((updated as any).text ?? "").toBe(textBefore ?? "");
    expect((updated as any).result).toContain("file content");
  });
});
