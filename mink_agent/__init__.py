"""
mink SDK — Python wrapper for agent execution (optionally sandboxed).

Sandboxing is handled entirely by the Rust ``mink`` binary internally.
The Python layer does NOT construct sandbox commands — it just launches
``mink --agent-jsonl`` and passes sandbox configuration via the
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
from typing import Any, Iterator, Optional


# ── Exceptions ───────────────────────────────────────────────────────

class AgentError(RuntimeError):
    """Raised when the agent process fails."""


# ── Helpers ──────────────────────────────────────────────────────────

def _find_binary() -> str:
    """Locate the bundled ``mink`` binary.

    Resolution order:
    1. ``MINK_BINARY`` environment override.
    2. Package-internal ``_binary/mink`` (bundled wheel).
    3. ``mink`` on ``PATH``.
    4. ``./mink`` in the current working directory.
    """
    override = os.environ.get("MINK_BINARY")
    if override:
        override_path = os.path.abspath(override)
        if not os.path.isfile(override_path):
            raise FileNotFoundError(f"MINK_BINARY does not exist: {override}")
        if not os.access(override_path, os.X_OK):
            raise PermissionError(f"MINK_BINARY is not executable: {override}")
        return override_path

    # 1. Bundled binary inside the package
    try:
        ref = _resources.files("mink_agent") / "_binary" / "mink"
        if ref.is_file():
            bin_path = str(ref)
            os.chmod(bin_path, 0o755)
            return bin_path
    except (TypeError, AttributeError, OSError):
        pass

    # 2. On PATH
    which = shutil.which("mink")
    if which:
        return which

    # 3. CWD fallback
    cwd_bin = os.path.join(os.getcwd(), "mink")
    if os.path.isfile(cwd_bin):
        return cwd_bin

    raise FileNotFoundError(
        "mink binary not found. "
        "Install mink-sdk from the correct platform wheel "
        "or place the mink binary on PATH."
    )


def _default_home() -> str:
    """Return the default ``MINK_HOME`` path."""
    base = os.path.join(Path.home(), ".mink")
    os.makedirs(base, exist_ok=True)
    return base


# ── SandboxConfig ────────────────────────────────────────────────────

@dataclass
class SandboxConfig:
    """Configuration for a sandboxed agent session.

    Parameters
    ----------
    mink_home:
        Session storage directory.  Defaults to ``~/.mink/``.
        Also read from the ``MINK_HOME`` environment variable.
    mission_file:
        Path to a MISSION.md file.  When set, replaces the default system
        prompt sections with those defined in the file.  Each ``# heading``
        in the file maps to a prompt section.
    mission_content:
        Inline MISSION.md content (alternative to ``mission_file``).  When set,
        the content is written to a temp file and passed to mink automatically.
        Provide either ``mission_file`` or ``mission_content``, not both.
    read_dirs:
        Directories the agent is allowed to read from.
        Relative paths are resolved against the current working directory.
    write_dirs:
        Directories the agent is allowed to write to.
    allow_bash:
        Whether the Bash tool is enabled.
    bash_allow_commands:
        Command-name whitelist for Bash.  Empty = use built-in deny list only.
    allow_python:
        Whether Python scripts may be executed via Bash.
    allow_network:
        Whether network access is allowed (LLM API requires this).
    allow_sub_agent:
        Whether the SubAgent tool is enabled.
    max_memory_mb:
        Maximum memory for the sandboxed process (nsjail cgroup only).
    max_pids:
        Maximum number of processes (nsjail cgroup only).
    timeout_secs:
        Hard timeout for the entire agent run.
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
    signal_mode:
        Signal system mode override: ``"full"`` enables belief tracking,
        injection, and recovery guards; ``"off"`` disables signal prompt and
        runtime signal intervention.  ``None`` inherits ``MINK_SIGNAL_MODE``;
        if unset, mink defaults to ``"full"``.
    cwd:
        Working directory for the agent (default: current working directory).
    """

    # Paths
    mink_home: Optional[str] = None
    mission_file: Optional[str] = None
    mission_content: Optional[str] = None

    # File-system
    read_dirs: list[str] = field(default_factory=list)
    write_dirs: list[str] = field(default_factory=list)

    # Tool control
    allow_bash: bool = True
    bash_allow_commands: list[str] = field(default_factory=list)
    allow_python: bool = True
    allow_network: bool = True
    allow_sub_agent: bool = True

    # Resource limits
    max_memory_mb: int = 1024
    max_pids: int = 64
    timeout_secs: int = 600
    tool_timeout: int = 600
    sub_agent_timeout: int = 300
    llm_first_event_timeout: int = 60
    llm_idle_timeout: int = 90
    llm_wait_heartbeat: int = 30
    max_tokens: int = 81920
    max_turns: int = 40
    verbose: bool = False

    # Backend
    sandbox_backend: str = "auto"

    # API
    api_key: str = ""
    api_url: str = ""
    model: str = ""

    # Session
    session_id: str = ""

    # Signal system
    signal_mode: Optional[str] = None

    # Working directory
    cwd: Optional[str] = None


# ── AgentSession ─────────────────────────────────────────────────────

class AgentSession:
    """A single-shot sandboxed agent session.

    Each call to :meth:`run` launches a sandboxed ``mink`` process,
    executes the prompt, collects results, and cleans up.
    """

    def __init__(self, config: SandboxConfig):
        self._config = config
        self._binary: str = _find_binary()
        self._home: Optional[str] = None
        self._proc: Optional[subprocess.Popen] = None
        self._run_lock = threading.Lock()

    # ── Public API ────────────────────────────────────────────────

    def run(self, prompt: str, *, extra_options: Optional[dict] = None) -> dict[str, Any]:
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

        for event in self.stream(prompt, extra_options=extra_options):
            events.append(event)
            event_type = event.get("type", "")
            if event_type == "text":
                text_parts.append(event.get("content", ""))
            elif event_type == "thinking":
                thinking_parts.append(event.get("content", ""))
            elif event_type == "tool_call":
                tool_calls.append(event)
            elif event_type == "tool_result":
                tool_results.append(event)
            elif event_type == "final":
                final = event

        final = final or {}
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
            "session_id": final.get("session_id"),
            "session_ref": final.get("session_ref"),
            "home": final.get("home"),
            "events_path": final.get("events_path"),
            "conversation_path": final.get("conversation_path"),
            "artifacts_dir": final.get("artifacts_dir"),
            "summary_path": final.get("summary_path"),
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
        """Execute a prompt and yield protocol events as dictionaries."""
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

    def _stream_process(
        self,
        cmd: list[str],
        request: str,
        env: dict[str, str],
    ) -> Iterator[dict[str, Any]]:
        """Run the mink process with the given command and yield JSONL events."""
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
                f"mink binary not found ({e}). "
                f"Ensure mink is installed and available."
            ) from e

        self._proc = proc
        stderr_parts: list[str] = []
        stdout_queue: queue.Queue[Optional[str]] = queue.Queue()

        def drain_stderr() -> None:
            if proc.stderr is None:
                return
            try:
                for chunk in proc.stderr:
                    stderr_parts.append(chunk)
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

            if final_event is None:
                error_text = None
                if timed_out:
                    error_text = f"agent timed out after {self._config.timeout_secs} seconds"
                elif proc.returncode != 0:
                    error_text = f"agent exited with code {proc.returncode}"
                final_event = {
                    "type": "final",
                    "version": 1,
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
        if self._config.signal_mode is not None:
            signal_mode = self._config.signal_mode.strip().lower()
            if signal_mode not in ("full", "off"):
                raise ValueError("signal_mode must be 'full' or 'off'")
            env["MINK_SIGNAL_MODE"] = signal_mode
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
            "allow_bash": cfg.allow_bash,
            "bash_allow_commands": cfg.bash_allow_commands,
            "allow_python": cfg.allow_python,
            "allow_network": cfg.allow_network,
            "allow_sub_agent": cfg.allow_sub_agent,
            "max_memory_mb": cfg.max_memory_mb,
            "max_pids": cfg.max_pids,
            "timeout_secs": cfg.timeout_secs,
        }
        return limits

    def _build_request(self, prompt: str, extra_options: Optional[dict]) -> str:
        """Build the Agent JSONL request string."""
        self._validate_request_config()
        options: dict[str, bool | str | int] = {}
        if not self._config.allow_bash:
            options["disable_bash"] = True
        if not self._config.allow_sub_agent:
            options["disable_sub_agent"] = True
        if not self._config.allow_network:
            options["disable_web"] = True
        if not self._config.allow_python:
            options["disable_python"] = True
        if self._config.model:
            options["model"] = self._config.model
        if self._config.max_tokens != 81920:
            options["max_tokens"] = self._config.max_tokens
        if self._config.max_turns != 40:
            options["max_turns"] = self._config.max_turns
        if self._config.tool_timeout != 600:
            options["tool_timeout"] = self._config.tool_timeout
        if self._config.sub_agent_timeout != 300:
            options["sub_agent_timeout"] = self._config.sub_agent_timeout
        if self._config.llm_first_event_timeout != 60:
            options["llm_first_event_timeout"] = self._config.llm_first_event_timeout
        if self._config.llm_idle_timeout != 90:
            options["llm_idle_timeout"] = self._config.llm_idle_timeout
        if self._config.llm_wait_heartbeat != 30:
            options["llm_wait_heartbeat"] = self._config.llm_wait_heartbeat
        if self._config.verbose:
            options["verbose"] = True
        if extra_options:
            options.update(extra_options)

        req: dict[str, Any] = {"version": 1, "prompt": prompt}
        if self._config.session_id:
            req["session_id"] = self._config.session_id
        if options:
            req["options"] = options
        return json.dumps(req) + "\n"

    def _validate_request_config(self) -> None:
        """Validate SDK-side options before launching mink."""
        cfg = self._config
        if cfg.model and cfg.model not in (
            "flash",
            "pro",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
        ):
            raise ValueError("model must be 'flash' or 'pro'")
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
        if cfg.llm_wait_heartbeat < 0:
            raise ValueError("llm_wait_heartbeat must be zero or greater")

    def _build_sandbox_cmd(self) -> list[str]:
        """Build the full command line: sandbox wrapper + mink binary."""
        cmd = self._build_sandbox_cmd_inner()
        self._append_mission_flag(cmd)
        return cmd

    def _mission_home_path(self) -> str:
        """Return a per-run mission file path inside MINK_HOME."""
        return os.path.join(self._home or _default_home(), f"_mission-{uuid.uuid4().hex}.md")

    def _append_mission_flag(self, cmd: list[str]) -> None:
        """Append --mission <path> if configured.

        When sandbox is active, the mission file is always placed under
        MINK_HOME (guaranteed accessible inside all sandbox backends)
        so that ``--mission`` resolves correctly inside the sandbox.
        """
        cfg = self._config
        if not cfg.mission_file and not cfg.mission_content:
            return

        sandbox_active = cfg.sandbox_backend.strip().lower() != "off"

        if sandbox_active:
            # Sandbox: copy/write to MINK_HOME for guaranteed accessibility
            dest = self._mission_home_path()
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if cfg.mission_file:
                if os.path.abspath(cfg.mission_file) != os.path.abspath(dest):
                    shutil.copy2(cfg.mission_file, dest)
            elif cfg.mission_content:
                with open(dest, "w", encoding="utf-8") as f:
                    f.write(cfg.mission_content)
            cmd.extend(["--mission", dest])
        else:
            # No sandbox: use original paths directly
            if cfg.mission_file:
                cmd.extend(["--mission", cfg.mission_file])
            elif cfg.mission_content:
                dest = self._mission_home_path()
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                with open(dest, "w", encoding="utf-8") as f:
                    f.write(cfg.mission_content)
                cmd.extend(["--mission", dest])

    def _build_sandbox_cmd_inner(self) -> list[str]:
        """Launch mink directly — sandboxing is handled by Rust internally."""
        return [self._binary, "--agent-jsonl"]


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
