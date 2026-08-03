// E2E teardown：杀 server + 删除临时 home（测完即删）
import { readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const E2E_HOME = join(tmpdir(), "mink-e2e-home");

export default async function globalTeardown() {
  // 精准杀 server（pid 文件），不误杀连接 18821 的浏览器/runner
  try {
    const pid = readFileSync(join(tmpdir(), "mink-e2e-server.pid"), "utf-8").trim();
    if (pid) process.kill(Number(pid), "SIGKILL");
  } catch { /* ignore */ }
  try {
    rmSync(join(tmpdir(), "mink-e2e-server.pid"), { force: true });
  } catch { /* ignore */ }
  rmSync(E2E_HOME, { recursive: true, force: true });
}
