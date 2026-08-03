// E2E：真实浏览器几何/样式/console 断言（用真实数据会话）。
// 文本可读断言体系——布局/几何/崩溃问题可由 agent 自行发现，无需人工看图。

import { test, expect, type Page } from "@playwright/test";
import { E2E_SESSION_ID } from "./global-setup";

const SESSION_ID = E2E_SESSION_ID;

test.describe.configure({ mode: "serial" });

// 每个测试后关闭会话（释放 lock，防止残留）
test.afterEach(async ({ page }) => {
  await page.evaluate(async (sid: string) => {
    await fetch(`/api/sessions/${sid}/close`, { method: "POST" }).catch(() => {});
  }, SESSION_ID);
});

// 打开真实会话：顶栏会话下拉（单栏布局，E2E home 单项目）
async function openRealSession(page: Page) {
  await page.goto("/");
  await page.locator(".crumb-item.sess").click();
  const row = page.locator(`.sess-drop .drop-row`, { hasText: SESSION_ID }).first();
  await row.click();
}

test("页面加载：无 console 错误 + 单栏 + 空状态工作台 + body 不滚动", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });

  await page.goto("/");
  await expect(page.locator(".topbar")).toBeVisible();
  await expect(page.locator(".crumb")).toBeVisible();
  await expect(page.locator(".conn")).toBeVisible();
  // 单栏：无三栏 grid（侧栏是 fixed 抽屉，不在布局流中）
  await expect(page.locator(".content")).toBeVisible();
  await expect(page.locator(".empty")).toBeVisible();
  // 无会话时 ▤ 占位隐藏（visibility:hidden 但占位）
  const ctxVis = await page.locator(".ctx-btn").evaluate((el) => getComputedStyle(el).visibility);
  expect(ctxVis).toBe("hidden");
  const ctxW = await page.locator(".ctx-btn").boundingBox();
  expect(ctxW!.width).toBeGreaterThan(0);
  const bodyScrolls = await page.evaluate(() => {
    const se = document.scrollingElement!;
    return se.scrollHeight > se.clientHeight + 2;
  });
  expect(bodyScrolls).toBe(false);
  expect(errors).toEqual([]);
});

test("单栏几何：输入框常驻 + 顶栏稳定（toast 不位移）", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");

  // 无会话：空状态工作台 + 输入框不出现（会话页才有输入框）
  await expect(page.locator(".empty")).toBeVisible();

  // 顶栏稳定：flash（无会话点重连 → toast）前后 conn/新建 位置不变
  const pos = async () => page.evaluate(() => ({
    conn: Math.round(document.querySelector(".conn")!.getBoundingClientRect().x),
    nbtn: Math.round(document.querySelector(".btn-primary")!.getBoundingClientRect().x),
  }));
  const p1 = await pos();
  await page.locator(".icon-btn[title=重连]").click();
  await expect(page.locator(".toast")).toBeVisible();
  await page.waitForTimeout(120);
  const p2 = await pos();
  expect(p1).toEqual(p2);
  // conn 在最右（新建按钮右侧）
  expect(p1.conn).toBeGreaterThan(p1.nbtn);

  // 打开会话 → 输入框常驻可见
  await openRealSession(page);
  await expect(page.locator(".tool-card").first()).toBeVisible({ timeout: 15_000 });
  const input = page.locator("textarea");
  await expect(input).toBeVisible();
  const ib = await input.boundingBox();
  expect(ib!.y + ib!.height).toBeLessThanOrEqual(800);
  expect(errors).toEqual([]);
});

test("指标行：信息在左 + 状态徽标贴右", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openRealSession(page);
  await expect(page.locator(".tool-card").first()).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(".sess-metrics")).toBeVisible();
  // 左侧信息（模型/输入/输出）与右侧状态徽标几何分离
  const g = await page.evaluate(() => {
    const q = (s: string) => document.querySelector(s)!.getBoundingClientRect().x;
    const first = document.querySelector(".sess-metrics .sm") as HTMLElement;
    return {
      left: first.getBoundingClientRect().x,
      stateWrap: q(".sm-state-wrap"),
      state: q(".sm-state"),
    };
  });
  expect(g.stateWrap).toBeGreaterThan(g.left + 100);
  expect(g.state).toBeGreaterThan(g.left + 100);
  // 状态徽标文字：WORK_LABEL 全集（历史会话可能停在工具事件 → 执行工具）
  const label = await page.locator(".sm-state").textContent();
  expect(["空闲", "运行中", "等待模型", "思考中", "生成中", "执行工具", "子代理", "压缩中", "错误"]).toContain(label);
  expect(errors).toEqual([]);
});

test("真实会话：历史渲染 + 工具卡片几何 + 折叠策略 + Edit 结构化", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await openRealSession(page);
  await expect(page.locator(".tool-card").first()).toBeVisible({ timeout: 15_000 });

  // 严格顺序断言：从服务端 conversation 预测期望可见块序列，与 DOM 顺序逐一对比。
  // 用浏览器内 fetch（与页面同源同环境，避免 page.request 走代理差异）
  const convRows = await page.evaluate(async (sid: string) => {
    const r = await fetch(`/api/sessions/${sid}/conversation?limit=20&tail=true`);
    return ((await r.json()) as { data: Record<string, any>[] }).data;
  }, SESSION_ID);
  const expected: string[] = [];
  for (const row of convRows) {
    const content = row.content;
    if (row.role === "user" && typeof content === "string") expected.push("user");
    else if (row.role === "assistant" && Array.isArray(content)) {
      for (const c of content) {
        if (c.type === "thinking") {
          const t = String(c.thinking ?? c.content ?? "");
          if (t.trim()) expected.push("thinking");
        }
        else if (c.type === "text") expected.push("text");
        else if (c.type === "tool_use") expected.push("tool");
      }
    }
  }

  const order = await page.evaluate(() => {
    return [...document.querySelectorAll(".transcript > *")].map((el) => {
      const c = el.className || "";
      if (c.includes("tool-card")) return "tool";
      if (c.includes("thinking-panel")) return "thinking";
      if (c.includes("msg user")) return "user";
      if (c.includes("msg error")) return "error";
      if (c.includes("signal")) return "signal";
      if (c.includes("msg system")) return "system";
      if (c.includes("msg agent") || c.includes("md-body")) return "text";
      return "other";
    });
  });
  const actual = order.filter((k) => ["user", "thinking", "text", "tool"].includes(k));
  // DOM 可见块顺序必须与 conversation 轮次顺序一致（分组渲染 bug 会使此失败）
  expect(actual).toEqual(expected);

  // transcript 是唯一滚动容器
  const scrollInfo = await page.evaluate(() => {
    const t = document.querySelector(".transcript") as HTMLElement;
    const se = document.scrollingElement!;
    return {
      overflow: getComputedStyle(t).overflowY,
      scrollable: t.scrollHeight > t.clientHeight,
      bodyScrollable: se.scrollHeight > se.clientHeight + 2,
    };
  });
  expect(scrollInfo.overflow).toBe("auto");
  expect(scrollInfo.bodyScrollable).toBe(false);

  // 工具卡片几何：布局稳定后高度/宽度有效（poll 等待折叠 details 布局完成）
  const card = page.locator(".tool-card").first();
  await expect(card).toBeVisible();
  await expect
    .poll(async () => (await card.boundingBox())?.height ?? 0, { timeout: 5_000 })
    .toBeGreaterThan(20);
  const box = await card.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThan(200);

  // 工具名贴左（margin-right:auto 钉死）
  const geo = await card.evaluate((el) => {
    const name = el.querySelector(".t-name") as HTMLElement;
    return { cardX: el.getBoundingClientRect().x, nameX: name.getBoundingClientRect().x };
  });
  expect(geo.nameX - geo.cardX).toBeLessThan(50); // 固定偏移=箭头+padding，非居中

  // 折叠策略：历史（idle）卡片默认折叠；点击展开
  const details = page.locator(".tool-card").first();
  expect(await details.getAttribute("open")).toBeNull();
  await details.locator("summary").click();
  await expect(details).toHaveAttribute("open", "");

  // Agent 消息头像（M 渐变方块）与 user 头像
  const agentAv = await page.locator(".msg.agent .av").count();
  const userAv = await page.locator(".msg.user .av").count();
  expect(agentAv).toBeGreaterThan(0);
  expect(userAv).toBeGreaterThan(0);

  // Edit 卡片：结构化 hunk 渲染（path + replace/insert hunk + add/del 行），无 ANSI 乱码
  const editCards = page.locator(".tool-card").filter({
    has: page.locator(".t-name", { hasText: "Edit" }),
  });
  if (await editCards.count()) {
    const first = editCards.first();
    await first.locator("summary").click();
    await expect.poll(async () => await first.locator(".e-hunk, .e-path").count()).toBeGreaterThan(0);
    const text = await first.textContent();
    expect(text).not.toContain("\u001b");
  }
  expect(errors).toEqual([]);
});

test("懒加载：滚动到顶触发 before_seq 请求", async ({ page }) => {
  const beforeRequests: string[] = [];
  page.on("request", (req) => {
    if (req.url().includes("before_seq")) beforeRequests.push(req.url());
  });

  await openRealSession(page);
  await expect(page.locator(".tool-card").first()).toBeVisible({ timeout: 15_000 });

  // 滚到顶触发懒加载
  const beforeScroll = await page.locator(".transcript").evaluate((el) => el.scrollTop);
  await page.locator(".transcript").evaluate((el) => {
    el.scrollTop = 0;
    el.dispatchEvent(new Event("scroll"));
  });
  await expect.poll(() => beforeRequests.length).toBeGreaterThan(0);
  // 加载后焦点保持（不自动滚到底部）
  await page.waitForTimeout(500);
  const scrollState = await page.locator(".transcript").evaluate((el) => ({
    top: el.scrollTop,
    max: el.scrollHeight - el.clientHeight,
  }));
  expect(scrollState.top).toBeGreaterThanOrEqual(0);
  expect(scrollState.top).toBeLessThan(scrollState.max - 100); // 未滚到底部
});

test("实时广播链路：发送 → turn_error 显示 → 输入恢复", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");

  // 用临时会话测试（sk-fake 发送会写 conversation——不污染真实会话）
  const name = `e2e-live-${Date.now()}`;
  const created = await page.evaluate(async (n) => {
    const r = await fetch("/api/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: n }),
    });
    return ((await r.json()) as { data: { id: string } }).data;
  }, name);
  expect(created.id).toBeTruthy();
  try {
    await page.evaluate(async (id: string) => {
      await fetch(`/api/sessions/${id}/open`, { method: "POST" });
    }, created.id);
    await page.goto(`/?session=${created.id}`);
    await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });

    // 发送消息（sk-fake → runtime 产生 turn_start → turn_error → turn_final 广播）
    await page.locator("textarea").fill("hi");
    await page.getByRole("button", { name: "发送" }).click();

    // ① 用户消息本地立即显示（广播流不含 user_input）
    await expect(page.locator(".msg.user").last()).toContainText("hi", { timeout: 5_000 });
    // ② turn 进行中：状态徽标离开"空闲"（turn_start 广播到达 → waiting/thinking/…）
    await expect(page.locator(".sm-state")).not.toHaveText("空闲", { timeout: 10_000 });
    // turn_error 事件经广播到达并渲染（错误消息块出现）
    await expect(page.locator(".msg.error").first()).toBeVisible({ timeout: 20_000 });
    // 输入框恢复可用（turn_final 后 running=false）
    await expect(page.locator("textarea")).toBeEnabled({ timeout: 10_000 });
    expect(errors).toEqual([]);
  } finally {
    // 清理临时会话（不残留）
    await page.evaluate(async (id: string) => {
      await fetch(`/api/sessions/${id}`, { method: "DELETE" });
    }, created.id);
  }
});

test("上下文面板：4 tabs + 右侧滑出 + 内容加载", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openRealSession(page);
  await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });
  // 顶栏 ▤ 打开面板（无会话时不可用，此处已打开会话）
  await page.locator(".icon-btn[title=上下文]").click();
  const panel = page.locator(".ctx-panel");
  await expect(panel).toBeVisible();
  // 右侧滑出（position fixed + right 0）
  const pos = await panel.evaluate((el) => {
    const c = getComputedStyle(el);
    return { position: c.position, right: c.right, height: parseFloat(c.height) };
  });
  expect(pos.position).toBe("fixed");
  expect(pos.right).toBe("0px");
  expect(pos.height).toBeGreaterThan(500);
  // 4 tabs
  await expect(panel.locator(".ctx-tabs button")).toHaveCount(5);
  // 计划 tab 内容（加载完成：无计划或正文）
  await expect(panel.locator(".ctx-sec")).toBeVisible({ timeout: 5_000 });
  // Todo tab 切换
  await panel.locator(".ctx-tabs button", { hasText: "Todo" }).click();
  await expect(panel.locator(".ctx-sec h6")).toContainText("Todo", { timeout: 5_000 });
  // 用量 tab：分组明细（4 组 metric）
  await panel.locator(".ctx-tabs button", { hasText: "用量" }).click();
  await expect(panel.locator(".ug-group")).toHaveCount(4);
  await expect(panel.locator(".ug-group").first()).toContainText("模型", { timeout: 5_000 });
  await expect(panel.locator(".ug-group", { hasText: "Token" })).toContainText("输入");
  expect(errors).toEqual([]);
});

test.describe("移动端适配（390×844）", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("单栏布局：无横向溢出 + 汉堡抽屉 + 会话可打开", async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.goto("/");

    // 无横向溢出（body 宽度不超视口）
    const overflow = await page.evaluate(() => {
      return document.scrollingElement!.scrollWidth > window.innerWidth + 2;
    });
    expect(overflow).toBe(false);

    // 汉堡按钮可见 → 点击打开抽屉（工作区+会话侧栏）
    await expect(page.locator(".hamburger")).toBeVisible();
    await page.locator(".hamburger").click();
    await expect(page.locator(".side-drawer.open")).toBeVisible();

    // 抽屉内会话列表（data-id 精确匹配）→ 内容区打开（单项目 home，无需选工作区）
    const sess = page.locator(`.side-drawer .sess-row[data-id="${SESSION_ID}"]`);
    await sess.click();
    await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });
    // 顶栏精简：品牌文字/conn/📄/▤ 隐藏（390px）
    const topHidden = await page.evaluate(() => ({
      label: getComputedStyle(document.querySelector(".brand-label")!).display,
      conn: getComputedStyle(document.querySelector(".conn")!).display,
      files: getComputedStyle(document.querySelector(".icon-btn[title=文件预览]")!).display,
      ctx: getComputedStyle(document.querySelector(".icon-btn[title=上下文]")!).display,
    }));
    expect(topHidden.label).toBe("none");
    expect(topHidden.conn).toBe("none");
    expect(topHidden.files).toBe("none");
    expect(topHidden.ctx).toBe("none");
    // 指标行字母标识：全称隐藏、I/O 缩写显示
    const abbr = await page.evaluate(() => ({
      full: getComputedStyle(document.querySelector(".sm-full")!).display,
      abbr: getComputedStyle(document.querySelector(".sm-abbr")!).display,
    }));
    expect(abbr.full).toBe("none");
    expect(abbr.abbr).not.toBe("none");
    // 移动端上下文面板：文件 tab 显示（5 tabs）
    // （op-more 位于指标行滚动区右端，Playwright actionability 可能超时——用 JS 点击）
    await page.locator(".sm-ops .op-more").evaluate((el) => (el as HTMLElement).click());
    await expect(page.locator(".ctx-panel .tab-file")).toBeVisible();
    await expect(page.locator(".ctx-panel .ctx-tabs button")).toHaveCount(5);
    await page.keyboard.press("Escape");
    await page.waitForTimeout(150);
    // 输入框可见且可聚焦（16px 防 iOS 缩放）
    const input = page.locator("textarea");
    await expect(input).toBeVisible();
    const fs = await input.evaluate((el) => getComputedStyle(el).fontSize);
    expect(parseFloat(fs)).toBeGreaterThanOrEqual(15);
    expect(errors).toEqual([]);
  });
});

test("文件预览面板：树 + md 渲染 + 代码着色", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openRealSession(page);
  await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });
  await page.locator(".icon-btn[title=文件预览]").click();
  const panel = page.locator(".fp-panel");
  await expect(panel).toBeVisible();
  const pos = await panel.evaluate((el) => getComputedStyle(el).position);
  expect(pos).toBe("fixed");

  // 打开 README.md → md 渲染（h1 + 代码块 + 代码块内着色 span）
  await panel.locator(".f-row", { hasText: "README.md" }).click();
  await expect(panel.locator(".fp-md h1")).toHaveText("e2e project", { timeout: 5_000 });
  await expect(panel.locator(".fp-md pre")).toBeVisible();
  const hl = await panel.locator(".fp-md pre .h-k, .fp-md pre .h-s").count();
  expect(hl).toBeGreaterThan(0);

  // 进入 src 目录 → main.ts 代码着色
  await panel.locator(".f-row", { hasText: "src" }).click();
  await panel.locator(".f-row", { hasText: "main.ts" }).click();
  await expect(panel.locator(".fp-code")).toBeVisible({ timeout: 5_000 });
  const spans = await panel.locator(".fp-code .h-k, .fp-code .h-s, .fp-code .h-c").count();
  expect(spans).toBeGreaterThan(3);
  expect(errors).toEqual([]);
});

test("P3：状态徽标流转 + 会话列表 tokens", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  // 会话抽屉：tokens 徽标（E2E 会话无 usage → 不显示；用实时广播会话验证状态流转）
  await page.evaluate(async (n) => {
    const r = await fetch("/api/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: n, cwd: "/tmp/mink-e2e-cwd" }),
    });
    return (await r.json()) as { data: { id: string } };
  }, `e2e-ws-${Date.now()}`);
  // 发送 → 状态流转：等待模型 → 思考中/生成中 → 错误
  await page.goto("/");
  await page.locator(".crumb-item.sess").click();
  const row = page.locator(`.sess-drop .drop-row`, { hasText: "e2e-ws-" }).first();
  await row.click();
  await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });
  await page.locator("textarea").fill("hi");
  await page.getByRole("button", { name: "发送" }).click();
  // 状态徽标出现运行态文字（等待模型/思考中/执行工具/生成中之一）
  await expect(page.locator(".sm-state")).not.toHaveText("空闲", { timeout: 15_000 });
  const seen = await page.locator(".sm-state").textContent();
  expect(["等待模型", "思考中", "执行工具", "生成中", "运行中"]).toContain(seen);
  expect(errors).toEqual([]);
});

test("Home：点击品牌图标回到空状态工作台", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openRealSession(page);
  await expect(page.locator(".sess-metrics")).toBeVisible({ timeout: 15_000 });
  // 点击左上角品牌 → Home（EmptyState）
  await page.locator(".brand").click();
  await expect(page.locator(".empty")).toBeVisible();
  await expect(page.locator(".sess-metrics")).toHaveCount(0);
  // 会话下拉恢复"选择会话"（已离开会话视图，列表保留）
  await expect(page.locator(".crumb-item.sess .crumb-label")).toHaveText("选择会话");
  expect(errors).toEqual([]);
});

test("内容区对齐：指标行/消息/输入框同列 840 居中", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.setViewportSize({ width: 1280, height: 800 });
  await openRealSession(page);
  await expect(page.locator(".tool-card").first()).toBeVisible({ timeout: 15_000 });
  const xs = await page.evaluate(() => {
    const q = (s: string) => {
      const el = document.querySelector(s);
      return el ? Math.round(el.getBoundingClientRect().x) : -1;
    };
    return {
      metrics: q(".sess-metrics .sm"),
      inputbar: q(".input-bar textarea"),
      transcript: q(".transcript .msg"),
    };
  });
  // 三者都以 840 主列起始（(1280-840)/2 = 220）
  const expected = 220;
  console.log("GEOMETRY:", JSON.stringify(xs), "expected:", expected);
  expect(xs.metrics).toBe(expected);
  expect(xs.inputbar).toBe(expected);
  expect(xs.transcript).toBe(expected);
  expect(errors).toEqual([]);
});
