import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  retries: 0,
  globalSetup: "./e2e/global-setup.ts",
  globalTeardown: "./e2e/global-teardown.ts",
  use: {
    // 生产形态：页面与 SSE 都由 mink-server 提供（同源，无 vite proxy 变量）
    baseURL: "http://127.0.0.1:18821",
    trace: "retain-on-failure",
  },
});
