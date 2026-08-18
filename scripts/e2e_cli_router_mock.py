#!/usr/bin/env python3
"""CLI/TUI-ready mock e2e for --router integration.

Run from workspace root after building the CLI:

    cargo build -p mink-cli --bin mink
    python3 scripts/e2e_cli_router_mock.py
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import http.server
import socketserver
import shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "mink")
captured = []


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        captured.append(body.decode("utf-8", errors="replace"))
        payload = (
            "data: {\"id\":\"chatcmpl-cli\",\"object\":\"chat.completion.chunk\","
            "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n"
            "data: {\"id\":\"chatcmpl-cli\",\"object\":\"chat.completion.chunk\","
            "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],"
            "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n"
            "data: [DONE]\n\n"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def start_server():
    captured.clear()
    srv = Server(("127.0.0.1", 0), Handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, port


def run_cli(home, cwd, port, session, prompt):
    env = os.environ.copy()
    env["MINK_HOME"] = home
    env["HOME"] = home
    cmd = [
        BIN,
        "--base-url", f"http://127.0.0.1:{port}/v1",
        "--api-key", "test-key",
        "--model", "deepseek-v4-flash",
        "--router",
        "--prefab=router-flash-weak",
        "--session", session,
        "--print",
        prompt,
    ]
    proc = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=180)
    if proc.returncode != 0:
        print(proc.stdout)
        print(proc.stderr)
        raise RuntimeError(f"mink cli failed rc={proc.returncode}")


def main():
    if not os.path.exists(BIN):
        print("build first: cargo build -p mink-cli --bin mink")
        return 1
    srv, port = start_server()
    tmp = tempfile.mkdtemp(prefix="mink-cli-router-")
    home = os.path.join(tmp, "home")
    cwd = os.path.join(tmp, "cwd")
    os.makedirs(home)
    os.makedirs(cwd)
    with open(os.path.join(cwd, "AGENTS.md"), "w", encoding="utf-8") as f:
        f.write("# CLI Router AGENTS\n")

    try:
        run_cli(home, cwd, port, "cli-router", "请帮我看看当前项目里都有哪些文件以及它们之间的关系")
        assert len(captured) == 1, "expected one captured request"
        obj = json.loads(captured[0])
        sys_prompt = obj["messages"][0]["content"]
        assert "Before acting, decide the task type" in sys_prompt
        assert any(
            isinstance(m.get("content"), str) and "Router:" in m["content"]
            for m in obj["messages"]
        )
        tools = [
            t.get("name") or t.get("function", {}).get("name")
            for t in obj["tools"]
        ]
        assert "Bash" in tools
        assert "Write" not in tools
        print("CLI --router mock e2e passed")
        return 0
    finally:
        srv.shutdown()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
