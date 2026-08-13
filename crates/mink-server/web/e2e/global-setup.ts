// E2E 全局准备：
// 1. 创建临时 home（mkdtemp）——E2E 数据全部在临时目录，测完删除，不碰真实会话
// 2. 构造自包含 conversation fixture，保证顺序/几何/懒加载测试可复现
// 3. 启动 mink-server（临时 home + 端口 18821——非默认端口，不与用户运行实例冲突）
import { mkdirSync, writeFileSync, rmSync, realpathSync } from "node:fs";
import { execSync } from "node:child_process";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

const BACKEND_PORT = 18821;
export const E2E_SESSION_ID = "e2e-session";
export const E2E_HOME = join(tmpdir(), "mink-e2e-home");
const WEB_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPO_DIR = resolve(WEB_DIR, "../../..");

export default async function globalSetup() {
  // 清理残留（上次运行失败时的）
  rmSync(E2E_HOME, { recursive: true, force: true });
  mkdirSync(E2E_HOME, { recursive: true });

  const E2E_CWD = "/tmp/mink-e2e-cwd";
  mkdirSync(join(E2E_CWD, "src"), { recursive: true });
  const projectKey = minkProjectKey(realpathSync(E2E_CWD));
  const sessDir = join(E2E_HOME, ".mink", "projects", projectKey, E2E_SESSION_ID);
  mkdirSync(sessDir, { recursive: true });
  writeFileSync(join(sessDir, "conversation.jsonl"), conversationFixture());
  writeFileSync(
    join(sessDir, "session.json"),
    JSON.stringify({
      id: E2E_SESSION_ID,
      alias: "e2e-template",
      title: "e2e",
      cwd: E2E_CWD,
      created_at: "",
      updated_at: "",
      parent: null,
      first_prompt: null,
      summary: null,
    }),
  );
  mkdirSync(join(sessDir, "artifacts"), { recursive: true });

  // 创建 cwd 与测试文件（文件预览面板 E2E：md 渲染 + 代码着色）
  writeFileSync(join(E2E_CWD, "README.md"), "# e2e project\n\n**Mink** 测试项目。\n\n```bash\nmake test && echo \"done\"\n```\n");
  writeFileSync(
    join(E2E_CWD, "src", "main.ts"),
    "import { createApp } from \"vue\";\nconst app = createApp({});\n// 注释\napp.mount(\"#app\");\n",
  );

  // 构建前端产物（生产形态：页面由 mink-server ServeDir 同源提供）
  execSync("npm run build", { cwd: WEB_DIR, stdio: "ignore" });
  execSync("cargo build -p mink-server", { cwd: REPO_DIR, stdio: "ignore" });

  // 启动 server（临时 home + 非默认端口）
  const server = spawn(
    join(REPO_DIR, "target/debug/mink-server"),
    [],
    {
      env: {
        ...process.env,
        DEEPSEEK_API_KEY: "sk-fake",
        MINK_HOME: E2E_HOME,
        // E2E 始终服务磁盘 web/dist 最新产物（嵌入产物随 cargo build，前端改动后未重建会过期）
        MINK_SERVER_DEV_WEB: "1",
        MINK_SERVER_PORT: String(BACKEND_PORT),
        // 短 turn 超时：sk-fake 请求外网 LLM 不可控——8s 超时兜底产生 turn_error（确定性）
        MINK_SERVER_TURN_TIMEOUT: "8",
      },
      stdio: "ignore",
    },
  );
  // 记录 server pid（teardown 精准杀，避免 lsof|xargs 误杀浏览器进程）
  try {
    writeFileSync(join(tmpdir(), "mink-e2e-server.pid"), String(server.pid ?? ""));
  } catch { /* ignore */ }
  await waitFor(`http://127.0.0.1:${BACKEND_PORT}/health`);
}

function minkProjectKey(cwd: string): string {
  const normalized = cwd.replaceAll("\\", "/");
  const hash = createHash("sha256").update(normalized).digest("hex").slice(0, 16);
  const readable = normalized
    .replace(/^\/+/, "")
    .replace(/[^A-Za-z0-9._-]/g, "-")
    .replace(/--+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 48) || "root";
  return `${readable}--${hash}`;
}

function conversationFixture(): string {
  const rows: Record<string, unknown>[] = [];
  for (let turn = 0; turn < 12; turn++) {
    const toolId = `read-${turn}`;
    rows.push({ role: "user", content: `fixture question ${turn}` });
    rows.push({
      role: "assistant",
      content: [
        { type: "thinking", thinking: `inspect fixture ${turn}` },
        { type: "text", text: `fixture answer ${turn}` },
        { type: "tool_use", id: toolId, name: "Read", input: { path: "README.md" } },
      ],
    });
    rows.push({
      role: "user",
      content: [{ type: "tool_result", tool_use_id: toolId, content: "# e2e project" }],
    });
  }
  return rows.map((row) => JSON.stringify(row)).join("\n") + "\n";
}


async function waitFor(url: string) {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch { /* retry */ }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("backend not ready");
}
