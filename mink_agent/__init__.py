"""
mink SDK — Python wrapper for agent execution (optionally sandboxed).

Sandboxing is handled entirely by the Rust ``mink-core`` binary internally.
The Python layer does NOT construct sandbox commands — it just launches
``mink-core --agent-jsonl`` and passes sandbox configuration via the
``MINK_LIMITS`` environment variable.

* **Linux**: nsjail / bubblewrap (auto-detected, Rust re-exec).
* **macOS**: sandbox-exec (built-in, Rust re-exec).

Usage::

    from mink_agent import SandboxConfig, AgentSession

    session = AgentSession(SandboxConfig(
        api_key="sk-...",
        read_dirs=["/project/src"],
        write_dirs=["/project/src"],
    ))
    result = session.run("Refactor src/handler.rs")
    print(result["text"])
    session.close()
"""

from __future__ import annotations

import importlib.resources as _resources
import json
import math
import os
import queue
import signal
import shutil
import subprocess
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterator, Optional

_CAPTURE_LIMIT_BYTES = 1_000_000
# ── Exceptions ───────────────────────────────────────────────────────

class AgentError(RuntimeError):
    """Raised when the agent process fails."""


@dataclass
class AgentStreamEvent:
    """Normalized stream event for UI/server integrations.

    The raw Rust protocol currently emits ``thinking`` and ``text`` events.
    This wrapper maps them to explicit delta event names while preserving the
    original payload in ``raw``.
    """

    type: str
    channel: Optional[str] = None
    content: str = ""
    raw: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        data = dict(self.raw)
        data["type"] = self.type
        if self.channel is not None:
            data["channel"] = self.channel
        if self.content:
            data["content"] = self.content
        return data


# ── Helpers ──────────────────────────────────────────────────────────

def _find_binary() -> str:
    """Locate the bundled ``mink-core`` binary.

    Resolution order:
    1. ``MINK_BINARY`` environment override.
    2. Package-internal ``_binary/mink-core`` (bundled wheel).
    3. ``mink-core`` on ``PATH``.
    4. ``./mink-core`` in the current working directory.
    """
    override = os.environ.get("MINK_BINARY")
    if override:
        override_path = os.path.abspath(override)
        if not os.path.isfile(override_path):
            raise FileNotFoundError(f"MINK_BINARY does not exist: {override}")
        if not os.access(override_path, os.X_OK):
            raise PermissionError(f"MINK_BINARY is not executable: {override}")
        return override_path

    # Bundled binary inside the package
    try:
        ref = _resources.files("mink_agent") / "_binary" / "mink-core"
        if ref.is_file():
            bin_path = str(ref)
            os.chmod(bin_path, 0o755)
            return bin_path
    except (TypeError, AttributeError, OSError):
        pass

    # On PATH
    which = shutil.which("mink-core")
    if which:
        return which

    # CWD fallback
    cwd_bin = os.path.join(os.getcwd(), "mink-core")
    if os.path.isfile(cwd_bin):
        return cwd_bin

    raise FileNotFoundError(
        "mink-core binary not found. "
        "Install mink-agent from the correct platform wheel "
        "or place the mink-core binary on PATH."
    )


def _default_home() -> str:
    """Return the default ``MINK_HOME`` path."""
    base = str(Path.home())
    os.makedirs(base, exist_ok=True)
    return base


def _state_dir(home: str) -> str:
    """Return mink's state directory under a Rust-style MINK_HOME root."""
    return os.path.join(home, ".mink")


# ── SandboxConfig ────────────────────────────────────────────────────

@dataclass
class InlineSkill:
    """Inline runtime skill sent through the Agent JSONL request."""

    name: str
    description: str
    content: str
    exposure: str = "model_addressable"
    revision: Optional[str] = None

@dataclass
class SandboxConfig:
    """Configuration for a sandboxed agent session.

    Parameters
    ----------
    mink_home:
        Home root passed to Rust as ``MINK_HOME``. Defaults to the user's home
        directory, so SDK sessions use ``~/.mink/sessions`` by default. Also
        read from the ``MINK_HOME`` environment variable.
    mission_file:
        Path to a MISSION.md file.  When set, replaces the default system
        prompt sections with those defined in the file.  Each ``# heading``
        in the file maps to a prompt section.
    mission_content:
        Inline MISSION.md content (alternative to ``mission_file``).  When set,
        the content is passed directly via the SDK protocol JSONL request,
        avoiding temporary file I/O.  Provide either ``mission_file`` or
        ``mission_content``, not both.
    read_dirs:
        Directories the agent is allowed to read from.
        Relative paths are resolved against the current working directory.
    write_dirs:
        Directories the agent is allowed to write to.
    allow_network:
        Whether network access is allowed (LLM API requires this).
    enabled_tools:
        Exact tool selection. ``None`` uses the default tool set; an empty list
        disables all tools. Include ``"PythonSandbox"`` explicitly to enable it.
    skills:
        Selected skill names to inject into the system prompt. These names must
        exist in mink's normal skill discovery paths or built-in skills.
    inline_skills:
        Runtime skills embedded directly in the SDK request. Use this for
        private deployment instructions that should not be read from disk.
    skill_discovery_policy:
        ``"defaults"`` loads runtime, project/user filesystem, and built-in
        skills. ``"runtime_only"`` and ``"explicit_only"`` load only SDK/Rust
        injected runtime skills/providers.
    max_memory_mb:
        Maximum memory for the sandboxed process (nsjail cgroup only).
    max_pids:
        Maximum number of processes (nsjail cgroup only).
    timeout_secs:
        Hard timeout for the entire agent run.
    tool_timeout:
        Default timeout (seconds) for a single Bash/Python/custom tool call.
    tool_timeout_max:
        Upper limit (seconds) for a single Bash/Python/custom tool call.
        Explicit per-call timeouts above this value fail closed; default 600.
    llm_first_event_timeout:
        Seconds to wait for the first model stream event.
    llm_idle_timeout:
        Seconds to wait between model stream events.
    llm_wait_heartbeat:
        Seconds between waiting notices; set to 0 to disable notices.
    sandbox_backend:
        ``"auto"`` | ``"nsjail"`` | ``"bwrap"`` | ``"sandbox-exec"`` | ``"off"``.
        Passed to the Rust binary via ``MINK_LIMITS`` — the Rust side
        handles backend auto-detection and sandbox construction internally.
        Set to ``"off"`` to disable sandboxing entirely (no re-exec).
    api_key:
        DeepSeek API key.  Also read from the ``DEEPSEEK_API_KEY`` env var.
    api_url:
        DeepSeek API base URL.  Also read from ``DEEPSEEK_BASE_URL``.
    model:
        Model name override (e.g. ``"deepseek-chat"``).
    signal_policy:
        Signal response policy: ``"off"``, ``"evidence"``, ``"state_ops"``,
        ``"restart"``, or ``"full"``. ``None`` inherits
        ``MINK_SIGNAL_POLICY``; if unset, mink defaults to ``"full"``.
    cwd:
        Working directory for the agent (default: current working directory).
    session_layout:
        Session storage layout passed to ``mink-core``. ``"home"`` is the SDK
        default and stores sessions under ``mink_home/.mink/sessions``;
        ``"isolated"`` uses ``mink_home`` itself as the session directory.
    """

    # Paths
    mink_home: Optional[str] = None
    mission_file: Optional[str] = None
    mission_content: Optional[str] = None

    # File-system
    read_dirs: list[str] = field(default_factory=list)
    write_dirs: list[str] = field(default_factory=list)

    # Tool control
    allow_network: bool = True
    enabled_tools: Optional[list[str]] = None  # 精确工具选择，None=默认工具集
    edit_mode: str = "hashline"
    edit_fuzzy_match: bool = True
    edit_fuzzy_threshold: float = 0.95
    edit_enforce_seen_lines: bool = False
    skills: list[str] = field(default_factory=list)
    inline_skills: list[InlineSkill] = field(default_factory=list)
    skill_discovery_policy: str = "defaults"
    # PythonSandbox tool configuration; enable it through enabled_tools.
    python_sandbox_wasm_path: str = "cpython-wasi/python.wasm"
    python_sandbox_stdlib_dir: str = "cpython-wasi"
    python_sandbox_read_dirs: list[str] = field(default_factory=list)
    python_sandbox_write_dirs: list[str] = field(default_factory=list)
    python_sandbox_package_dirs: list[str] = field(default_factory=list)
    python_sandbox_timeout: int = 30

    # Resource limits
    max_memory_mb: int = 1024
    max_pids: int = 64
    timeout_secs: int = 600
    tool_timeout: int = 600
    tool_timeout_max: int = 600
    sub_agent_timeout: int = 300
    llm_first_event_timeout: int = 60
    llm_idle_timeout: int = 90
    llm_wait_heartbeat: int = 30
    max_tokens: int = 81920
    max_turns: int = 40
    max_context: int = 1_000_000
    context_compact_pct: int = 94
    context_reserve_tokens: int = 64_000
    context_compact_tail_tokens: int = 256_000
    context_compact_max_output_tokens: int = 8_192
    context_compact_input_reduction: bool = False
    max_search_files: int = 5000
    max_search_results: int = 1000
    verbose: bool = False
    stream_events: bool = True

    # Backend
    sandbox_backend: str = "auto"

    # API
    api_key: str = ""
    api_url: str = ""
    model: str = ""

    # Session
    session_id: str = ""
    session_layout: str = "home"

    # Signal system
    signal_policy: Optional[str] = None

    # Working directory
    cwd: Optional[str] = None


def _deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    """Recursively merge ``override`` into a copy of ``base``.

    Nested dictionaries are merged key-by-key so SDK-computed option groups
    (notably ``tools``) are preserved when a caller overrides only a subset
    via ``extra_options``. Non-dict values from ``override`` replace the base
    value.
    """
    merged = dict(base)
    for key, value in override.items():
        if isinstance(value, dict):
            if isinstance(merged.get(key), dict):
                merged[key] = _deep_merge(merged[key], value)
            else:
                merged[key] = _deep_merge({}, value)
        else:
            merged[key] = value
    return merged


# ── AgentSession ─────────────────────────────────────────────────────

class AgentSession:
    """A single-shot sandboxed agent session.

    Each call to :meth:`run` launches a sandboxed ``mink-core`` process,
    executes the prompt, collects results, and cleans up.
    """

    def __init__(self, config: SandboxConfig):
        self._config = config
        self._binary: str = _find_binary()
        self._home: Optional[str] = None
        self._proc: Optional[subprocess.Popen] = None
        self._run_lock = threading.Lock()

    # ── Public API ────────────────────────────────────────────────

    def run(
        self,
        prompt: str,
        *,
        extra_options: Optional[dict] = None,
        on_event: Optional[Callable[[dict[str, Any]], None]] = None,
    ) -> dict[str, Any]:
        """Execute a prompt in the sandbox and return the result.

        Returns a dict with keys:

        * ``text`` — the agent's text response
        * ``tool_calls`` — list of tool call events
        * ``thinking`` — the agent's reasoning content
        * ``exit_code`` — process exit code (0 = success)
        * ``error`` — error message if the agent failed
        * ``stderr`` — raw stderr output (for debugging)
        """
        text_parts: list[str] = []
        thinking_parts: list[str] = []
        tool_calls: list[dict] = []
        tool_results: list[dict] = []
        events: list[dict] = []
        final: Optional[dict] = None

        for event in self.raw_stream(prompt, extra_options=extra_options):
            if on_event is not None:
                on_event(event)
            events.append(event)
            event_type = event.get("type", "")
            if event_type in ("text", "answer_delta"):
                text_parts.append(event.get("content", ""))
            elif event_type in ("thinking", "thinking_delta"):
                thinking_parts.append(event.get("content", ""))
            elif event_type == "tool_call":
                tool_calls.append(event)
            elif event_type == "tool_result":
                tool_results.append(event)
            elif event_type == "final":
                final = event

        final = final or {}
        if (not text_parts and not thinking_parts) and final.get("conversation_path"):
            loaded_text, loaded_thinking = self._load_last_assistant_output(
                str(final.get("conversation_path"))
            )
            if loaded_text:
                text_parts.append(loaded_text)
            if loaded_thinking:
                thinking_parts.append(loaded_thinking)
        status = final.get("status")
        stderr_text = final.get("stderr", "")
        error_message = final.get("error")
        exit_code = int(final.get("exit_code", 1 if error_message else 0))
        if status not in (None, "ok") and not error_message:
            error_message = str(status)

        return {
            "text": "".join(text_parts),
            "thinking": "".join(thinking_parts),
            "tool_calls": tool_calls,
            "tool_results": tool_results,
            "events": events,
            "status": status,
            "billing_turn_id": final.get("billing_turn_id"),
            "session_id": final.get("session_id"),
            "session_ref": final.get("session_ref"),
            "home": final.get("home"),
            "events_path": final.get("events_path"),
            "conversation_path": final.get("conversation_path"),
            "artifacts_dir": final.get("artifacts_dir"),
            "summary_path": final.get("summary_path"),
            "usage_path": final.get("usage_path"),
            "usage_records": final.get("usage_records", []),
            "usage": final.get("usage", {}),
            "tool_call_count": final.get("tool_call_count", len(tool_calls)),
            "tool_error_count": final.get("tool_error_count", 0),
            "exit_code": exit_code,
            "error": error_message,
            "stderr": stderr_text,
        }

    def stream(
        self,
        prompt: str,
        *,
        extra_options: Optional[dict] = None,
    ) -> Iterator[dict[str, Any]]:
        """Execute a prompt and yield raw protocol events as dictionaries.

        This method is kept for backward compatibility. New integrations that
        need explicit answer/thinking separation should prefer
        :meth:`stream_events`.
        """
        yield from self.raw_stream(prompt, extra_options=extra_options)

    def raw_stream(
        self,
        prompt: str,
        *,
        extra_options: Optional[dict] = None,
    ) -> Iterator[dict[str, Any]]:
        """Execute a prompt and yield raw Rust JSONL protocol events."""
        if not self._run_lock.acquire(blocking=False):
            raise RuntimeError("AgentSession does not support concurrent runs")
        try:
            self._prepare()
            cmd = self._build_sandbox_cmd()
            request = self._build_request(prompt, extra_options)
            env = self._build_env()
            yield from self._stream_process(cmd, request, env)
        finally:
            self._run_lock.release()

    def stream_events(
        self,
        prompt: str,
        *,
        extra_options: Optional[dict] = None,
    ) -> Iterator[AgentStreamEvent]:
        """Execute a prompt and yield normalized stream events."""
        for event in self.raw_stream(prompt, extra_options=extra_options):
            yield self._normalize_event(event)

    def close(self) -> None:
        """Terminate the agent process if still running.

        Sends SIGTERM first, then SIGKILL after a short grace period.
        Safe to call multiple times.
        """
        proc = self._proc
        if proc is None:
            return
        if proc.poll() is not None:
            # Process already exited
            self._proc = None
            return

        self._terminate_process_tree(proc, grace_seconds=3)
        self._proc = None

    # ── Internals ──────────────────────────────────────────────────

    @staticmethod
    def _normalize_event(event: dict[str, Any]) -> AgentStreamEvent:
        event_type = event.get("type", "")
        content = event.get("content", "") or ""
        if event_type == "thinking":
            return AgentStreamEvent(
                type="thinking_delta",
                channel="thinking",
                content=content,
                raw=event,
            )
        if event_type == "text":
            return AgentStreamEvent(
                type="answer_delta",
                channel="answer",
                content=content,
                raw=event,
            )
        return AgentStreamEvent(
            type=event_type,
            channel=event.get("channel"),
            content=content,
            raw=event,
        )

    @staticmethod
    def _load_last_assistant_output(conversation_path: str) -> tuple[str, str]:
        text = ""
        thinking = ""
        try:
            with open(conversation_path, "r", encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    try:
                        msg = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if msg.get("role") != "assistant":
                        continue
                    msg_text = ""
                    msg_thinking = ""
                    content = msg.get("content", [])
                    if isinstance(content, list):
                        for item in content:
                            if not isinstance(item, dict):
                                continue
                            if item.get("type") == "text":
                                msg_text += item.get("text", "") or ""
                            elif item.get("type") == "thinking":
                                msg_thinking += item.get("thinking", "") or ""
                    elif isinstance(content, str):
                        msg_text = content
                    if msg_text or msg_thinking:
                        text = msg_text
                        thinking = msg_thinking
        except OSError:
            pass
        return text, thinking

    def _stream_process(
        self,
        cmd: list[str],
        request: str,
        env: dict[str, str],
    ) -> Iterator[dict[str, Any]]:
        """Run the mink-core process with the given command and yield JSONL events."""
        try:
            popen_kwargs: dict[str, Any] = {
                "args": cmd,
                "stdin": subprocess.PIPE,
                "stdout": subprocess.PIPE,
                "stderr": subprocess.PIPE,
                "env": env,
                "cwd": self._config.cwd,
                "text": True,
            }
            if os.name != "nt":
                popen_kwargs["start_new_session"] = True
            proc = subprocess.Popen(**popen_kwargs)
        except FileNotFoundError as e:
            raise RuntimeError(
                f"mink-core binary not found ({e}). "
                f"Ensure mink-core is installed and available."
            ) from e

        self._proc = proc
        stderr_parts: list[str] = []
        stderr_bytes = 0
        stderr_truncated = False
        stdout_queue: queue.SimpleQueue = queue.SimpleQueue()

        def append_stderr(chunk: str) -> None:
            nonlocal stderr_bytes, stderr_truncated
            encoded_len = len(chunk.encode("utf-8", errors="replace"))
            remaining = _CAPTURE_LIMIT_BYTES - stderr_bytes
            if remaining <= 0:
                stderr_truncated = True
                return
            if encoded_len <= remaining:
                stderr_parts.append(chunk)
                stderr_bytes += encoded_len
                return
            encoded = chunk.encode("utf-8", errors="replace")[:remaining]
            stderr_parts.append(encoded.decode("utf-8", errors="replace"))
            stderr_bytes = _CAPTURE_LIMIT_BYTES
            stderr_truncated = True

        def drain_stderr() -> None:
            if proc.stderr is None:
                return
            try:
                for chunk in proc.stderr:
                    append_stderr(chunk)
            except OSError:
                pass

        def drain_stdout() -> None:
            if proc.stdout is None:
                stdout_queue.put(None)
                return
            try:
                for line in proc.stdout:
                    stdout_queue.put(line)
            except OSError:
                pass
            finally:
                stdout_queue.put(None)

        stderr_thread = threading.Thread(target=drain_stderr, daemon=True)
        stdout_thread = threading.Thread(target=drain_stdout, daemon=True)
        stderr_thread.start()
        stdout_thread.start()

        try:
            try:
                if proc.stdin is not None:
                    proc.stdin.write(request)
                    proc.stdin.close()
            except BrokenPipeError:
                proc.wait(timeout=5)
                stderr_thread.join(timeout=1)
                raise AgentError(
                    f"Agent process exited early. exit_code={proc.returncode} "
                    f"stderr: {''.join(stderr_parts)}"
                )

            saw_final = False
            final_event: Optional[dict[str, Any]] = None
            timed_out = False
            post_final_exit_timeout = False
            deadline = time.monotonic() + max(1, int(self._config.timeout_secs))

            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    timed_out = True
                    if proc.poll() is None:
                        self._terminate_process_tree(proc, grace_seconds=3)
                    break
                try:
                    line = stdout_queue.get(timeout=min(0.1, remaining))
                except queue.Empty:
                    if proc.poll() is not None and not stdout_thread.is_alive():
                        break
                    continue
                if line is None:
                    break
                else:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        event = json.loads(line)
                    except json.JSONDecodeError:
                        continue

                    if event.get("type") == "final":
                        saw_final = True
                        final_event = event
                        break
                    yield event

            if proc.poll() is None:
                remaining_after_final = deadline - time.monotonic()
                if remaining_after_final <= 0:
                    post_final_exit_timeout = True
                    self._terminate_process_tree(proc, grace_seconds=3)
                else:
                    try:
                        proc.wait(timeout=min(5, remaining_after_final))
                    except subprocess.TimeoutExpired:
                        post_final_exit_timeout = True
                        self._terminate_process_tree(proc, grace_seconds=3)

            stdout_thread.join(timeout=1)
            stderr_thread.join(timeout=1)
            stderr_text = "".join(stderr_parts)
            if stderr_truncated:
                stderr_text += (
                    f"\n[... truncated stderr after {_CAPTURE_LIMIT_BYTES} bytes ...]"
                )

            if final_event is None:
                error_text = None
                if timed_out:
                    error_text = f"agent timed out after {self._config.timeout_secs} seconds"
                elif proc.returncode != 0:
                    error_text = f"agent exited with code {proc.returncode}"
                final_event = {
                    "type": "final",
                    "version": 3,
                    "status": "failed" if proc.returncode else "ok",
                    "error": error_text,
                }
            final_event["exit_code"] = proc.returncode
            final_event["stderr"] = stderr_text
            if post_final_exit_timeout:
                final_event["status"] = "failed"
                final_event["error"] = (
                    final_event.get("error")
                    or "agent did not exit after final event"
                )
            if not saw_final and stderr_text and final_event.get("error") is None and proc.returncode:
                final_event["error"] = stderr_text
            yield final_event
        finally:
            if proc.poll() is None:
                self._terminate_process_tree(proc, grace_seconds=3)
            for pipe in (proc.stdin, proc.stdout, proc.stderr):
                try:
                    if pipe is not None:
                        pipe.close()
                except OSError:
                    pass
            stdout_thread.join(timeout=1)
            stderr_thread.join(timeout=1)
            self._proc = None

    @staticmethod
    def _terminate_process_tree(
        proc: subprocess.Popen,
        *,
        grace_seconds: int,
    ) -> None:
        """Terminate the agent process and child processes where supported."""
        if proc.poll() is not None:
            return
        try:
            if os.name != "nt":
                os.killpg(proc.pid, signal.SIGTERM)
            else:
                proc.terminate()
            proc.wait(timeout=grace_seconds)
        except ProcessLookupError:
            return
        except subprocess.TimeoutExpired:
            try:
                if os.name != "nt":
                    os.killpg(proc.pid, signal.SIGKILL)
                else:
                    proc.kill()
                proc.wait()
            except ProcessLookupError:
                return

    def _prepare(self) -> None:
        """Ensure the home directory exists."""
        cfg = self._config
        self._home = (
            cfg.mink_home
            or os.environ.get("MINK_HOME")
            or _default_home()
        )
        os.makedirs(self._home, exist_ok=True)

    def _build_env(self) -> dict[str, str]:
        """Build environment variables for the agent process."""
        env = os.environ.copy()
        env["MINK_HOME"] = self._home or _default_home()
        if self._config.signal_policy is not None:
            signal_policy = self._config.signal_policy.strip().lower()
            if signal_policy not in ("off", "evidence", "state_ops", "restart", "full"):
                raise ValueError(
                    "signal_policy must be one of: off, evidence, state_ops, restart, full"
                )
            env["MINK_SIGNAL_POLICY"] = signal_policy
        if self._config.api_key:
            env["DEEPSEEK_API_KEY"] = self._config.api_key
        if self._config.api_url:
            env["DEEPSEEK_BASE_URL"] = self._config.api_url

        # ── Pass sandbox config via MINK_LIMITS (Rust handles the rest) ──
        sb = self._build_sandbox_limits()
        if sb is not None:
            env["MINK_LIMITS"] = json.dumps(sb)

        return env

    def _build_sandbox_limits(self) -> Optional[dict]:
        """Build the MINK_LIMITS JSON dict for Rust's SandboxConfig.

        Returns None when sandbox is fully disabled (backend == "off").
        """
        cfg = self._config
        if cfg.sandbox_backend == "off":
            return None

        limits: dict = {
            "enabled": True,
            "backend": cfg.sandbox_backend,
            "read_dirs": cfg.read_dirs,
            "write_dirs": cfg.write_dirs,
            "allow_network": cfg.allow_network,
            "max_memory_mb": cfg.max_memory_mb,
            "max_pids": cfg.max_pids,
            "timeout_secs": cfg.timeout_secs,
        }
        return limits

    def _build_request(self, prompt: str, extra_options: Optional[dict]) -> str:
        """Build the Agent JSONL request string."""
        self._validate_request_config()
        provider: dict[str, Any] = {}
        generation: dict[str, Any] = {}
        context: dict[str, Any] = {}
        tools: dict[str, Any] = {}
        session: dict[str, Any] = {}
        output: dict[str, Any] = {}
        if self._config.model:
            provider["model"] = self._config.model
        if self._config.max_tokens != 81920:
            generation["max_tokens"] = self._config.max_tokens
        if self._config.max_turns != 40:
            generation["max_turns"] = self._config.max_turns
        if self._config.max_context != 1_000_000:
            context["max_context"] = self._config.max_context
        if self._config.context_compact_pct != 94:
            context["context_compact_pct"] = self._config.context_compact_pct
        if self._config.context_reserve_tokens != 64_000:
            context["context_reserve_tokens"] = self._config.context_reserve_tokens
        if self._config.context_compact_tail_tokens != 256_000:
            context["context_compact_tail_tokens"] = self._config.context_compact_tail_tokens
        if self._config.context_compact_max_output_tokens != 8_192:
            context["context_compact_max_output_tokens"] = (
                self._config.context_compact_max_output_tokens
            )
        if self._config.context_compact_input_reduction:
            context["context_compact_input_reduction"] = True
        if self._config.tool_timeout != 600:
            tools["tool_timeout"] = self._config.tool_timeout
        if self._config.tool_timeout_max != 600:
            tools["tool_timeout_max"] = self._config.tool_timeout_max
        if self._config.sub_agent_timeout != 300:
            tools["sub_agent_timeout"] = self._config.sub_agent_timeout
        if self._config.llm_first_event_timeout != 60:
            generation["llm_first_event_timeout"] = self._config.llm_first_event_timeout
        if self._config.llm_idle_timeout != 90:
            generation["llm_idle_timeout"] = self._config.llm_idle_timeout
        if self._config.llm_wait_heartbeat != 30:
            generation["llm_wait_heartbeat"] = self._config.llm_wait_heartbeat
        if self._config.verbose:
            output["verbose"] = True
        if not self._config.stream_events:
            output["stream_events"] = False
        if self._config.enabled_tools is not None:
            tools["enabled_tools"] = self._config.enabled_tools
        if self._config.edit_mode != "hashline":
            tools["edit_mode"] = self._config.edit_mode
        if not self._config.edit_fuzzy_match:
            tools["edit_fuzzy_match"] = False
        if self._config.edit_fuzzy_threshold != 0.95:
            tools["edit_fuzzy_threshold"] = self._config.edit_fuzzy_threshold
        if self._config.edit_enforce_seen_lines:
            tools["edit_enforce_seen_lines"] = True
        if self._config.skills:
            tools["skills"] = self._config.skills
        if self._config.inline_skills:
            tools["inline_skills"] = [
                self._inline_skill_to_dict(skill)
                for skill in self._config.inline_skills
            ]
        if self._config.skill_discovery_policy != "defaults":
            tools["skill_discovery_policy"] = self._config.skill_discovery_policy
        if self._config.session_layout:
            session["session_layout"] = self._config.session_layout
        options: dict[str, Any] = {
            name: group
            for name, group in (
                ("provider", provider),
                ("generation", generation),
                ("context", context),
                ("tools", tools),
                ("session", session),
                ("output", output),
            )
            if group
        }
        if self._config.signal_policy is not None:
            options["signal"] = {"policy": self._config.signal_policy}
        if extra_options:
            options = _deep_merge(options, extra_options)
        signal = options.get("signal")
        if isinstance(signal, dict) and "policy" in signal:
            signal_policy = signal["policy"]
            if not isinstance(signal_policy, str):
                raise ValueError("signal.policy must be a string")
            signal_policy = signal_policy.strip().lower()
            if signal_policy not in ("off", "evidence", "state_ops", "restart", "full"):
                raise ValueError(
                    "signal_policy must be one of: off, evidence, state_ops, restart, full"
                )
            signal["policy"] = signal_policy

        req: dict[str, Any] = {"version": 3, "prompt": prompt}
        if self._config.mission_content:
            req["mission"] = self._config.mission_content
        if self._config.session_id:
            req["session_id"] = self._config.session_id
        if options:
            req["options"] = options
        return json.dumps(req) + "\n"

    def _validate_request_config(self) -> None:
        """Validate SDK-side options before launching mink-core."""
        cfg = self._config
        # Model names are passed through to the Rust resolver: known aliases
        # map to defaults, anything else is used verbatim (custom base_url).
        positive_fields = {
            "timeout_secs": cfg.timeout_secs,
            "tool_timeout": cfg.tool_timeout,
            "sub_agent_timeout": cfg.sub_agent_timeout,
            "llm_first_event_timeout": cfg.llm_first_event_timeout,
            "llm_idle_timeout": cfg.llm_idle_timeout,
            "max_tokens": cfg.max_tokens,
            "max_turns": cfg.max_turns,
        }
        for name, value in positive_fields.items():
            if value <= 0:
                raise ValueError(f"{name} must be greater than 0")
        if cfg.tool_timeout_max < 5:
            raise ValueError("tool_timeout_max must be at least 5")
        if cfg.max_context < 0:
            raise ValueError("max_context must be zero or greater")
        if not 1 <= cfg.context_compact_pct <= 100:
            raise ValueError("context_compact_pct must be between 1 and 100")
        compaction_positive_fields = {
            "context_reserve_tokens": cfg.context_reserve_tokens,
            "context_compact_tail_tokens": cfg.context_compact_tail_tokens,
            "context_compact_max_output_tokens": cfg.context_compact_max_output_tokens,
        }
        for name, value in compaction_positive_fields.items():
            if value <= 0:
                raise ValueError(f"{name} must be greater than 0")
        if cfg.llm_wait_heartbeat < 0:
            raise ValueError("llm_wait_heartbeat must be zero or greater")
        if cfg.session_layout not in ("project", "home", "direct", "isolated"):
            raise ValueError("session_layout must be 'project', 'home', 'direct', or 'isolated'")
        if cfg.edit_mode not in ("hashline", "replace"):
            raise ValueError("edit_mode must be 'hashline' or 'replace'")
        for name, value in {
            "edit_fuzzy_match": cfg.edit_fuzzy_match,
            "edit_enforce_seen_lines": cfg.edit_enforce_seen_lines,
        }.items():
            if type(value) is not bool:
                raise ValueError(f"{name} must be a boolean")
        if (
            isinstance(cfg.edit_fuzzy_threshold, bool)
            or not isinstance(cfg.edit_fuzzy_threshold, (int, float))
            or not math.isfinite(float(cfg.edit_fuzzy_threshold))
            or not (0.0 <= float(cfg.edit_fuzzy_threshold) <= 1.0)
        ):
            raise ValueError("edit_fuzzy_threshold must be a finite number in 0.0..=1.0")
        for skill in cfg.skills:
            self._validate_skill_name(skill, "skill")
        valid_exposures = {"model_discoverable", "model_addressable", "host_only"}
        valid_policies = {"defaults", "runtime_only", "explicit_only"}
        if cfg.skill_discovery_policy not in valid_policies:
            raise ValueError(
                "skill_discovery_policy must be 'defaults', 'runtime_only', or 'explicit_only'"
            )
        for skill in cfg.inline_skills:
            if not isinstance(skill, InlineSkill):
                raise ValueError("inline_skills must contain InlineSkill instances")
            self._validate_skill_name(skill.name, "inline skill")
            if not isinstance(skill.content, str) or not skill.content.strip():
                raise ValueError("inline skill content must be a non-empty string")
            if skill.exposure not in valid_exposures:
                raise ValueError(
                    "inline skill exposure must be 'model_discoverable', "
                    "'model_addressable', or 'host_only'"
                )

    @staticmethod
    def _validate_skill_name(name: Any, label: str) -> None:
        if not isinstance(name, str) or not name.strip():
            raise ValueError(f"{label} name must be a non-empty string")
        if (
            name.strip() != name
            or name.startswith(".")
            or "/" in name
            or "\\" in name
            or ".." in name
        ):
            raise ValueError(
                f"{label} name must not contain whitespace padding, path separators, "
                "leading dots, or '..'"
            )

    @staticmethod
    def _inline_skill_to_dict(skill: InlineSkill) -> dict[str, Any]:
        data: dict[str, Any] = {
            "name": skill.name,
            "description": skill.description,
            "content": skill.content,
            "exposure": skill.exposure,
        }
        if skill.revision is not None:
            data["revision"] = skill.revision
        return data

    def _build_sandbox_cmd(self) -> list[str]:
        """Build the full command line: sandbox wrapper + mink-core binary."""
        cmd = self._build_sandbox_cmd_inner()
        self._append_mission_flag(cmd)
        return cmd

    def _append_mission_flag(self, cmd: list[str]) -> None:
        """Append --mission <path> if configured (file-based only; inline mission goes via request JSON)."""
        cfg = self._config
        if not cfg.mission_file:
            return

        sandbox_active = cfg.sandbox_backend.strip().lower() != "off"

        if sandbox_active:
            # Sandbox: copy under MINK_HOME/.mink for guaranteed accessibility.
            dest = os.path.join(
                _state_dir(self._home or _default_home()),
                f"_mission-{uuid.uuid4().hex}.md",
            )
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if os.path.abspath(cfg.mission_file) != os.path.abspath(dest):
                shutil.copy2(cfg.mission_file, dest)
            cmd.extend(["--mission", dest])
        else:
            cmd.extend(["--mission", cfg.mission_file])

    def _build_sandbox_cmd_inner(self) -> list[str]:
        """Launch mink-core directly — sandboxing is handled by Rust internally."""
        cmd = [self._binary, "--agent-jsonl"]
        cfg = self._config

        # The --config TOML must match mink-cli's MinkConfigFile schema:
        # grouped sections with nested [tools.edit], never top-level flat keys.
        generation = {
            "max_tokens": cfg.max_tokens,
            "max_turns": cfg.max_turns,
            "llm_first_event_timeout": cfg.llm_first_event_timeout,
            "llm_idle_timeout": cfg.llm_idle_timeout,
            "llm_wait_heartbeat": cfg.llm_wait_heartbeat,
        }
        tools = {
            "tool_timeout": cfg.tool_timeout,
            "tool_timeout_max": cfg.tool_timeout_max,
            "sub_agent_timeout": cfg.sub_agent_timeout,
            "max_search_files": cfg.max_search_files,
            "max_search_results": cfg.max_search_results,
            "edit": {
                "mode": cfg.edit_mode,
                "fuzzy_match": cfg.edit_fuzzy_match,
                "fuzzy_threshold": cfg.edit_fuzzy_threshold,
                "enforce_seen_lines": cfg.edit_enforce_seen_lines,
            },
        }
        sections = {"generation": generation, "tools": tools}

        # PythonSandbox configuration; activation is controlled by enabled_tools.
        sp = {}
        if cfg.python_sandbox_wasm_path != "cpython-wasi/python.wasm":
            sp["wasm_path"] = cfg.python_sandbox_wasm_path
        if cfg.python_sandbox_stdlib_dir != "cpython-wasi":
            sp["stdlib_dir"] = cfg.python_sandbox_stdlib_dir
        if cfg.python_sandbox_timeout != 30:
            sp["timeout"] = cfg.python_sandbox_timeout
        if cfg.python_sandbox_read_dirs:
            sp["read_dirs"] = cfg.python_sandbox_read_dirs
        if cfg.python_sandbox_write_dirs:
            sp["write_dirs"] = cfg.python_sandbox_write_dirs
        if cfg.python_sandbox_package_dirs:
            sp["package_dirs"] = cfg.python_sandbox_package_dirs
        if sp:
            sections["sandbox_python"] = sp

        import json as _json

        lines: list[str] = []
        for section, values in sections.items():
            scalars = {k: v for k, v in values.items() if not isinstance(v, dict)}
            sub_tables = {k: v for k, v in values.items() if isinstance(v, dict)}
            if scalars or not sub_tables:
                lines.append(f"[{section}]")
                for key, value in scalars.items():
                    lines.append(f"{key} = {_json.dumps(value)}")
            for sub_name, sub_values in sub_tables.items():
                lines.append(f"[{section}.{sub_name}]")
                for key, value in sub_values.items():
                    lines.append(f"{key} = {_json.dumps(value)}")
        cmd.extend(["--config", "\n".join(lines)])
        return cmd


# ── Convenience ──────────────────────────────────────────────────────

def quick_run(
    prompt: str,
    *,
    read_dirs: Optional[list[str]] = None,
    write_dirs: Optional[list[str]] = None,
    **kwargs,
) -> dict[str, Any]:
    """One-shot convenience: create a session, run, close.

    >>> result = quick_run("Hello!", read_dirs=["/tmp"])
    >>> print(result["text"])
    """
    session = AgentSession(SandboxConfig(
        read_dirs=read_dirs or [],
        write_dirs=write_dirs or [],
        **kwargs,
    ))
    try:
        return session.run(prompt)
    finally:
        session.close()
