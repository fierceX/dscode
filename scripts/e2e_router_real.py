#!/usr/bin/env python3
"""Real-key e2e for mink-router with a local forwarding proxy.

The proxy captures every request/response while forwarding to the real
DeepSeek API, so we get both wire capture and real model behavior.

Enable with:

    MINK_ROUTER_E2E_REAL=1 DEEPSEEK_API_KEY=sk-... python3 scripts/e2e_router_real.py
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
import http.client
import ssl
import shutil
import urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

captured = []
save_dir = None


def ensure_save_dir():
    global save_dir
    if save_dir is None:
        ts = time.strftime("%Y%m%d-%H%M%S")
        save_dir = os.path.join(ROOT, "target", "e2e-real", ts)
        os.makedirs(save_dir, exist_ok=True)
    return save_dir


def real_target():
    base = os.environ.get("DEEPSEEK_BASE_URL", "https://api.deepseek.com/v1").rstrip("/")
    parsed = urllib.parse.urlparse(base)
    return parsed.scheme, parsed.hostname, parsed.port or (443 if parsed.scheme == "https" else 80), parsed.path


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        scheme, host, port, _base_path = real_target()
        # Mink already includes the full path (e.g. /v1/chat/completions) in
        # self.path because the proxy is configured as Mink's base URL.
        target_path = self.path
        captured.append(
            {
                "path": self.path,
                "body": body.decode("utf-8", errors="replace"),
            }
        )

        conn_cls = http.client.HTTPSConnection if scheme == "https" else http.client.HTTPConnection
        conn = conn_cls(host, port, timeout=120)
        headers = {
            "Content-Type": "application/json",
            "Authorization": f"Bearer {os.environ['DEEPSEEK_API_KEY']}",
            "Content-Length": str(len(body)),
        }
        conn.request("POST", target_path, body=body, headers=headers)
        resp = conn.getresponse()
        data = resp.read()
        captured[-1]["status"] = resp.status
        captured[-1]["response"] = data.decode("utf-8", errors="replace")

        # Persist the wire capture for later analysis.
        out_dir = ensure_save_dir()
        idx = len(captured) - 1
        with open(os.path.join(out_dir, f"request-{idx}.json"), "w", encoding="utf-8") as f:
            f.write(captured[-1]["body"])
        with open(os.path.join(out_dir, f"response-{idx}.jsonl"), "w", encoding="utf-8") as f:
            f.write(captured[-1]["response"])

        self.send_response(resp.status)
        for key, value in resp.getheaders():
            if key.lower() in ("content-length", "connection", "transfer-encoding"):
                continue
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        conn.close()

    def log_message(self, *args):
        pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def start_server():
    captured.clear()
    srv = Server(("127.0.0.1", 0), ProxyHandler)
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
    cmd = ["cargo", "run", "-q", "-p", "mink-router", "--example", "router_e2e"]
    proc = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True, timeout=300)
    if proc.returncode != 0:
        print(proc.stdout)
        print(proc.stderr)
        raise RuntimeError(f"router_e2e failed rc={proc.returncode}")


def find_session(home):
    for dp, dn, fn in os.walk(home):
        if "events.jsonl" in fn and "conversation.jsonl" in fn:
            return dp
    return None


def parse_toml_value(raw):
    value = raw.split("#", 1)[0].strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
        value = value[1:-1]
    return value


def load_key_from_minkrc():
    """Read api_key/base_url from ~/.minkrc without printing secrets."""
    path = os.path.expanduser("~/.minkrc")
    if not os.path.exists(path):
        return
    section = None
    api_key = None
    base_url = None
    with open(path, encoding="utf-8") as f:
        for line in f:
            stripped = line.strip()
            if stripped.startswith("[") and stripped.endswith("]"):
                section = stripped[1:-1].strip()
                continue
            if section != "provider":
                continue
            if stripped.startswith("api_key") and "=" in stripped:
                api_key = parse_toml_value(stripped.split("=", 1)[1])
            elif stripped.startswith("base_url") and "=" in stripped:
                base_url = parse_toml_value(stripped.split("=", 1)[1])
    if api_key and not os.environ.get("DEEPSEEK_API_KEY"):
        os.environ["DEEPSEEK_API_KEY"] = api_key
    if base_url and not os.environ.get("DEEPSEEK_BASE_URL"):
        os.environ["DEEPSEEK_BASE_URL"] = base_url


def main():
    load_key_from_minkrc()
    if os.environ.get("MINK_ROUTER_E2E_REAL") != "1":
        print("SKIP: set MINK_ROUTER_E2E_REAL=1 to run real-key e2e")
        return 0
    if not os.environ.get("DEEPSEEK_API_KEY"):
        print("SKIP: DEEPSEEK_API_KEY is not set")
        return 0

    srv, port = start_server()
    tmp = tempfile.mkdtemp(prefix="mink-router-real-")
    home = os.path.join(tmp, "home")
    cwd = os.path.join(tmp, "cwd")
    os.makedirs(home)
    os.makedirs(cwd)
    with open(os.path.join(cwd, "AGENTS.md"), "w", encoding="utf-8") as f:
        f.write("# Real Router AGENTS\n")

    prompts = [
        "请帮我看看当前项目里都有哪些文件以及它们之间的关系",
        "修复这个 bug",
        "写一个网站",
    ]
    try:
        for i, prompt in enumerate(prompts):
            run_example(home, cwd, port, "router-real", prompt, "router-flash-weak", True)
        print(f"captured {len(captured)} real requests")
        print(f"saved to: {ensure_save_dir()}")
        for i, req in enumerate(captured):
            print(f"  req{i}: status={req.get('status')} path={req['path']}")
            assert req.get("status") == 200, f"real API returned {req.get('status')}"
            body = json.loads(req["body"])
            sys_prompt = body["messages"][0]["content"]
            print("    persona:", "Reasoning-mode persona" in sys_prompt)
            print("    guide:", any(
                isinstance(m.get("content"), str) and "Router:" in m["content"]
                for m in body["messages"]
            ))

        session_dir = find_session(home)
        if session_dir is None:
            raise RuntimeError("session dir not found")
        events = open(os.path.join(session_dir, "events.jsonl"), encoding="utf-8").read()
        print("--- session dir:", session_dir)
        print("  prefix_snapshot count:", events.count('"type":"prefix_snapshot"'))
        print("  old prefab files:",
              os.path.exists(os.path.join(session_dir, "prefab-prefix.json")))
        print("REAL-KEY E2E PASSED")
        return 0
    finally:
        srv.shutdown()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
