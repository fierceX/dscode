#!/usr/bin/env python3
"""Mock e2e for mink-router: capture LLM requests and inspect session dirs.

No real API key is required. Run from the workspace root:

    python3 scripts/e2e_router_mock.py
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import http.server
import socketserver
import shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

captured = []


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        captured.append(
            {
                "path": self.path,
                "headers": dict(self.headers),
                "body": body.decode("utf-8", errors="replace"),
            }
        )
        payload = (
            "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\","
            "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n"
            "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\","
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


def run_example(home, cwd, port, session, prompt, prefab, narrow):
    env = os.environ.copy()
    env["MOCK_BASE_URL"] = f"http://127.0.0.1:{port}/v1"
    env["MINK_HOME"] = home
    env["CWD"] = cwd
    env["SESSION"] = session
    env["PROMPT"] = prompt
    env["PREFAB"] = prefab
    env["NARROW_TOOLS"] = "1" if narrow else "0"
    cmd = [
        "cargo",
        "run",
        "-q",
        "-p",
        "mink-router",
        "--example",
        "router_e2e",
    ]
    proc = subprocess.run(
        cmd, cwd=ROOT, env=env, capture_output=True, text=True, timeout=180
    )
    if proc.returncode != 0:
        print(proc.stdout)
        print(proc.stderr)
        raise RuntimeError(f"router_e2e failed rc={proc.returncode}")


def find_session(home):
    for dp, dn, fn in os.walk(home):
        if "events.jsonl" in fn and "conversation.jsonl" in fn:
            return dp
    return None


def analyze_requests(label):
    print(f"--- {label}: captured {len(captured)} requests")
    for i, req in enumerate(captured):
        obj = json.loads(req["body"])
        sys_prompt = obj.get("messages", [{}])[0].get("content", "")
        tool_names = [
            t.get("name") or t.get("function", {}).get("name")
            for t in obj.get("tools", [])
        ]
        has_router_persona = "Reasoning-mode persona" in sys_prompt
        has_guide = any(
            isinstance(m.get("content"), str) and "Router:" in m["content"]
            for m in obj.get("messages", [])
        )
        print(
            f"  req{i}: persona={has_router_persona} guide={has_guide} "
            f"tools={tool_names}"
        )
    return captured


def main():
    if not os.path.exists(os.path.join(ROOT, "Cargo.toml")):
        print("Run from workspace root")
        return 1

    srv, port = start_server()
    tmp = tempfile.mkdtemp(prefix="mink-router-mock-")
    home = os.path.join(tmp, "home")
    cwd = os.path.join(tmp, "cwd")
    os.makedirs(home)
    os.makedirs(cwd)
    with open(os.path.join(cwd, "AGENTS.md"), "w", encoding="utf-8") as f:
        f.write("# Mock Router AGENTS\n")

    prompts = [
        "请帮我看看当前项目里都有哪些文件以及它们之间的关系",
        "修复这个 bug",
        "写一个网站",
    ]
    try:
        for i, prompt in enumerate(prompts):
            run_example(home, cwd, port, "router-e2e", prompt, "router-flash-weak", True)
        analyze_requests("prefab+router+narrow")

        session_dir = find_session(home)
        if session_dir is None:
            print("ERROR: session dir not found")
            return 1
        events = open(os.path.join(session_dir, "events.jsonl"), encoding="utf-8").read()
        conv = open(os.path.join(session_dir, "conversation.jsonl"), encoding="utf-8").read()
        print("--- session dir:", session_dir)
        print("  prefix_snapshot count:", events.count('"type":"prefix_snapshot"'))
        print("  has prefab warmup:", "Read the workspace-root AGENTS.md" in conv)
        print("  has old prefab files:",
              os.path.exists(os.path.join(session_dir, "prefab-prefix.json")))

        # Basic assertions
        assert len(captured) == len(prompts), "request count mismatch"
        first = json.loads(captured[0]["body"])
        first_sys = first["messages"][0]["content"]
        persona_present = (
            "Reasoning-mode persona" in first_sys
            or "Before acting, decide the task type" in first_sys
        )
        assert persona_present, "flash persona should be present via prefab or router"
        # First prompt is weak -> should contain Router guide
        assert any(
            isinstance(m.get("content"), str) and "Router:" in m["content"]
            for m in first["messages"]
        )
        # With narrow tools enabled, first request should not contain full Edit/Write
        first_tools = [
            t.get("name") or t.get("function", {}).get("name")
            for t in first["tools"]
        ]
        assert "Write" not in first_tools
        assert "Bash" in first_tools
        print("ALL MOCK E2E ASSERTIONS PASSED")
        return 0
    finally:
        srv.shutdown()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
