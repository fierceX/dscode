#!/usr/bin/env python3

import argparse
import json
import os
import re
from pathlib import Path


def load_messages(path: Path):
    messages = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        messages.append(json.loads(line))
    return messages


def shorten(text: str, limit: int = 88) -> str:
    text = re.sub(r"\s+", " ", text).strip()
    if len(text) <= limit:
        return text
    return text[: limit - 1].rstrip() + "…"


def sanitize_text(text: str, repo_name: str) -> str:
    home = str(Path.home())
    cwd = str(Path.cwd())
    text = text.replace(cwd, f"/workspace/{repo_name}")
    text = text.replace(home, "/home/user")
    text = re.sub(r"/workspace/[^/\\s]+", f"/workspace/{repo_name}", text)
    text = re.sub(r"call_[A-Za-z0-9_]+", "call_xxx", text)
    text = re.sub(r"@r\\d+", "@rN", text)
    return text.strip()


def summarize_tool_use(name: str, payload: dict) -> str:
    if name == "Read":
        return f"Read({payload.get('path', '')})"
    if name == "Edit":
        patch = payload.get("patch", "")
        head = patch.splitlines()[1] if "\n" in patch else patch
        return f"Edit.patch({shorten(head or 'patch')})"
    if name == "Bash":
        return f"Bash({shorten(payload.get('command', ''))})"
    if name == "Python":
        return "Python(extract structured result)"
    if name == "Glob":
        return f"Glob({payload.get('pattern', '*')})"
    if name == "Grep":
        return f'Grep("{payload.get("pattern", "")}")'
    return f"{name}()"


def summarize_tool_result(text: str) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if not lines:
        return ""
    first = sanitize_text(lines[0], "mink")
    return shorten(first, 92)


def build_steps(messages, repo_name: str):
    steps = []
    last_prompt = ""
    for message in messages:
        role = message.get("role")
        content = message.get("content", [])
        if isinstance(content, str):
            content = [{"type": "text", "text": content}]

        if role == "user":
            for item in content:
                if item.get("type") == "text":
                    text = sanitize_text(item.get("text", ""), repo_name)
                    if text:
                        last_prompt = shorten(text, 72)
                        steps.append(
                            {
                                "kind": "prompt",
                                "mode": "instant",
                                "line": f"<span class=\"prompt\">&gt;</span> {last_prompt}",
                            }
                        )
                elif item.get("type") == "tool_result":
                    summary = summarize_tool_result(item.get("content", ""))
                    if summary:
                        steps.append(
                            {
                                "kind": "tool_result",
                                "mode": "instant",
                                "line": sanitize_text(summary, repo_name),
                            }
                        )

        elif role == "assistant":
            for item in content:
                item_type = item.get("type")
                if item_type == "thinking":
                    text = sanitize_text(item.get("thinking", ""), repo_name)
                    if text:
                        steps.append(
                            {
                                "kind": "thinking",
                                "mode": "typed",
                                "prefixHtml": '<span class="warn">▶ thinking</span> | ',
                                "text": shorten(text, 108),
                            }
                        )
                elif item_type == "text":
                    text = sanitize_text(item.get("text", ""), repo_name)
                    if text:
                        steps.append(
                            {
                                "kind": "text",
                                "mode": "typed",
                                "prefixHtml": '<span class="prompt">&gt;</span> ',
                                "text": shorten(text, 108),
                            }
                        )
                elif item_type == "tool_use":
                    name = item.get("name", "Tool")
                    line = summarize_tool_use(name, item.get("input", {}))
                    steps.append(
                        {
                            "kind": "tool",
                            "mode": "instant",
                            "line": f'<span class="ok">▼ ✓ {name}</span> {sanitize_text(line, repo_name)}',
                        }
                    )
    return steps


def enrich_steps(steps, session_label: str):
    out = []
    belief = 0.89
    output_k = 58.8
    turn = 12
    retry = 90

    for idx, step in enumerate(steps):
        kind = step["kind"]
        if kind == "thinking":
            status = "[thinking]"
            belief = min(0.94, belief + 0.005)
            output_k += 0.1
        elif kind == "tool":
            status = "[tool]"
            output_k += 0.1
        elif kind == "tool_result":
            status = "[tool]"
            output_k += 0.1
        elif kind == "text":
            status = "[idle]" if idx == len(steps) - 1 else "[writing]"
            belief = min(0.95, belief + 0.004)
            output_k += 0.1
        else:
            status = "[idle]"

        if "0 matches" in step.get("line", ""):
            belief = max(0.86, belief - 0.01)
        if "Result:" in step.get("line", ""):
            status = "[guard]"
            belief = max(0.82, belief - 0.05)

        out.append(
            {
                "statusLeft": session_label,
                "statusCenter": f"B:{belief:.2f} T:{turn} R:{retry} I:13.31M(98%) O:{output_k:.1f}K",
                "statusRight": f"C:231.1K(23%) {status}",
                "input": "",
                **{k: v for k, v in step.items() if k != "kind"},
            }
        )
    return out


def main():
    parser = argparse.ArgumentParser(description="Build homepage hero replay JSON from a real mink conversation.")
    parser.add_argument("input", help="Path to conversation.jsonl")
    parser.add_argument("-o", "--output", default="docs/assets/hero-replay.json", help="Output JSON path")
    parser.add_argument("--repo-name", default="mink", help="Repository label used for sanitization")
    parser.add_argument("--session-label", default="flash @mink", help="Status bar left label")
    parser.add_argument("--max-steps", type=int, default=36, help="Maximum number of replay steps to emit")
    args = parser.parse_args()

    messages = load_messages(Path(args.input))
    steps = build_steps(messages, args.repo_name)
    steps = steps[: args.max_steps]
    replay = {
        "meta": {
            "sessionLabel": args.session_label,
            "maxVisibleRows": 14,
        },
        "steps": enrich_steps(steps, args.session_label),
    }

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(replay, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
