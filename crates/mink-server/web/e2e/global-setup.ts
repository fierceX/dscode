// E2E 全局准备：
// 1. 创建临时 home（mkdtemp）——E2E 数据全部在临时目录，测完删除，不碰真实会话
// 2. 从真实会话 conversation.jsonl 复制前 40 行作为模板会话（正常轮次结构，
//    避开 E2E 污染的后段）——保证顺序断言/几何断言/懒加载有真实结构数据
// 3. 启动 mink-server（临时 home + 端口 18821——非默认端口，不与用户运行实例冲突）
import { mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { execSync } from "node:child_process";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const BACKEND_PORT = 18821;
export const E2E_SESSION_ID = "e2e-session";
export const E2E_HOME = join(tmpdir(), "mink-e2e-home");

export default async function globalSetup() {
  // 清理残留（上次运行失败时的）
  rmSync(E2E_HOME, { recursive: true, force: true });
  mkdirSync(E2E_HOME, { recursive: true });

  // 构造临时会话：真实 conversation 前 40 行（正常轮次）+ session.json。
  // project_key 必须与 cwd 匹配（mink 规则：路径 / → -）——runtime 按 cwd 解析项目，
  // 目录放错 project 会导致 UseOrCreate 找不到而新建会话。
  const E2E_CWD = "/tmp/mink-e2e-cwd";
  const realConv = "/Users/xialuyu/.mink/projects/-Users-xialuyu-Documents-code-dscode-new/20260801-023646-3510/conversation.jsonl";
  const sessDir = join(E2E_HOME, ".mink", "projects", "-tmp-mink-e2e-cwd", E2E_SESSION_ID);
  mkdirSync(sessDir, { recursive: true });
  const lines = readFileSync(realConv, "utf-8").split("\n").filter(Boolean);
  writeFileSync(join(sessDir, "conversation.jsonl"), lines.slice(0, 40).join("\n") + "\n");
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
  mkdirSync(join(E2E_CWD, "src"), { recursive: true });
  writeFileSync(join(E2E_CWD, "README.md"), "# e2e project\n\n**Mink** 测试项目。\n\n```bash\nmake test && echo \"done\"\n```\n");
  writeFileSync(
    join(E2E_CWD, "src", "main.ts"),
    "import { createApp } from \"vue\";\nconst app = createApp({});\n// 注释\napp.mount(\"#app\");\n",
  );

  // 构建前端产物（生产形态：页面由 mink-server ServeDir 同源提供）
  execSync("npm run build", { cwd: "/Users/xialuyu/Documents/code/dscode-new/crates/mink-server/web", stdio: "ignore" });

  // 启动 server（临时 home + 非默认端口）
  const server = spawn(
    "/Users/xialuyu/Documents/code/dscode-new/target/debug/mink-server",
    [],
    {
      env: {
        ...process.env,
        DEEPSEEK_API_KEY: "sk-fake",
        MINK_HOME: E2E_HOME,
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
