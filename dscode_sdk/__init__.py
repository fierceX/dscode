"""
dscode SDK — Python wrapper for agent execution (optionally sandboxed).

Sandboxing is handled entirely by the Rust ``dscode`` binary internally.
The Python layer does NOT construct sandbox commands — it just launches
``dscode --json-rpc`` and passes sandbox configuration via the
``DSCODE_LIMITS`` environment variable.

* **Linux**: nsjail / bubblewrap (auto-detected, Rust re-exec).
* **macOS**: sandbox-exec (built-in, Rust re-exec).

Usage::

    from dscode_sdk import SandboxConfig, AgentSession

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
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


# ── Exceptions ───────────────────────────────────────────────────────

class AgentError(RuntimeError):
    """Raised when the agent process fails."""


# ── Helpers ──────────────────────────────────────────────────────────

def _find_binary() -> str:
    """Locate the bundled ``dscode`` binary.

    Resolution order:
    1. Package-internal ``_binary/dscode`` (bundled wheel).
    2. ``dscode`` on ``PATH``.
    3. ``./dscode`` in the current working directory.
    """
    # 1. Bundled binary inside the package
    try:
        ref = _resources.files("dscode_sdk") / "_binary" / "dscode"
        if ref.is_file():
            bin_path = str(ref)
            os.chmod(bin_path, 0o755)
            return bin_path
    except (TypeError, AttributeError, OSError):
        pass

    # 2. On PATH
    which = shutil.which("dscode")
    if which:
        return which

    # 3. CWD fallback
    cwd_bin = os.path.join(os.getcwd(), "dscode")
    if os.path.isfile(cwd_bin):
        return cwd_bin

    raise FileNotFoundError(
        "dscode binary not found. "
        "Install dscode-sdk from the correct platform wheel "
        "or place the dscode binary on PATH."
    )


def _default_home() -> str:
    """Return the default ``DSCODE_HOME`` path."""
    base = os.path.join(Path.home(), ".dscode")
    os.makedirs(base, exist_ok=True)
    return base


# ── SandboxConfig ────────────────────────────────────────────────────

@dataclass
class SandboxConfig:
    """Configuration for a sandboxed agent session.

    Parameters
    ----------
    dscode_home:
        Session storage directory.  Defaults to ``~/.dscode/``.
        Also read from the ``DSCODE_HOME`` environment variable.
    mission_file:
        Path to a MISSION.md file.  When set, replaces the default system
        prompt sections with those defined in the file.  Each ``# heading``
        in the file maps to a prompt section.
    mission_content:
        Inline MISSION.md content (alternative to ``mission_file``).  When set,
        the content is written to a temp file and passed to dscode automatically.
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
    sandbox_backend:
        ``"auto"`` | ``"nsjail"`` | ``"bwrap"`` | ``"sandbox-exec"`` | ``"off"``.
        Passed to the Rust binary via ``DSCODE_LIMITS`` — the Rust side
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
        runtime signal intervention.  ``None`` inherits ``DSCODE_SIGNAL_MODE``;
        if unset, dscode defaults to ``"full"``.
    cwd:
        Working directory for the agent (default: current working directory).
    """

    # Paths
    dscode_home: Optional[str] = None
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

    Each call to :meth:`run` launches a sandboxed ``dscode`` process,
    executes the prompt, collects results, and cleans up.
    """

    def __init__(self, config: SandboxConfig):
        self._config = config
        self._binary: str = _find_binary()
        self._home: Optional[str] = None
        self._proc: Optional[subprocess.Popen] = None

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
        self._prepare()

        cmd = self._build_sandbox_cmd()
        request = self._build_request(prompt, extra_options)
        env = self._build_env()

        result = self._run_process(cmd, request, env)
        return result

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

        # Try graceful termination first
        proc.terminate()  # SIGTERM
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()   # SIGKILL
            proc.wait()
        self._proc = None

    # ── Internals ──────────────────────────────────────────────────

    def _run_process(
        self,
        cmd: list[str],
        request: str,
        env: dict[str, str],
    ) -> dict[str, Any]:
        """Run the dscode process with the given command and request."""
        try:
            proc = subprocess.Popen(
                cmd,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=env,
                cwd=self._config.cwd,
                text=True,
            )
        except FileNotFoundError as e:
            raise RuntimeError(
                f"dscode binary not found ({e}). "
                f"Ensure dscode is installed and available."
            ) from e

        self._proc = proc

        try:
            # Send the JSON-RPC request
            try:
                proc.stdin.write(request)
                proc.stdin.close()
            except BrokenPipeError:
                stderr_text = proc.stderr.read()
                raise AgentError(
                    f"Agent process exited early. exit_code={proc.returncode} "
                    f"stderr: {stderr_text}"
                )

            # Read the JSON event stream
            text_parts: list[str] = []
            thinking_parts: list[str] = []
            tool_calls: list[dict] = []
            error_message: Optional[str] = None

            for line in proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue

                event_type = event.get("type", "")
                if event_type == "text":
                    text_parts.append(event.get("content", ""))
                elif event_type == "thinking":
                    thinking_parts.append(event.get("content", ""))
                elif event_type == "tool_call":
                    tool_calls.append(event)
                elif event_type == "turn_end":
                    break
                elif event_type == "error":
                    error_message = event.get("message", "Unknown error")

            # Wait for process to finish
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

            # Read remaining stderr
            stderr_text = ""
            try:
                remaining = proc.stderr.read()
                if remaining:
                    stderr_text = remaining
            except OSError:
                pass

            return {
                "text": "".join(text_parts),
                "thinking": "".join(thinking_parts),
                "tool_calls": tool_calls,
                "exit_code": proc.returncode,
                "error": error_message,
                "stderr": stderr_text,
            }
        finally:
            self._proc = None

    def _prepare(self) -> None:
        """Ensure the home directory exists."""
        cfg = self._config
        self._home = (
            cfg.dscode_home
            or os.environ.get("DSCODE_HOME")
            or _default_home()
        )
        os.makedirs(self._home, exist_ok=True)

    def _build_env(self) -> dict[str, str]:
        """Build environment variables for the agent process."""
        env = os.environ.copy()
        env["DSCODE_HOME"] = self._home or _default_home()
        if self._config.signal_mode is not None:
            signal_mode = self._config.signal_mode.strip().lower()
            if signal_mode not in ("full", "off"):
                raise ValueError("signal_mode must be 'full' or 'off'")
            env["DSCODE_SIGNAL_MODE"] = signal_mode
        if self._config.api_key:
            env["DEEPSEEK_API_KEY"] = self._config.api_key
        if self._config.api_url:
            env["DEEPSEEK_BASE_URL"] = self._config.api_url

        # ── Pass sandbox config via DSCODE_LIMITS (Rust handles the rest) ──
        sb = self._build_sandbox_limits()
        if sb is not None:
            env["DSCODE_LIMITS"] = json.dumps(sb)

        return env

    def _build_sandbox_limits(self) -> Optional[dict]:
        """Build the DSCODE_LIMITS JSON dict for Rust's SandboxConfig.

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
        """Build the JSON-RPC request string."""
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
        if self._config.verbose:
            options["verbose"] = True
        if extra_options:
            options.update(extra_options)

        req: dict[str, Any] = {"prompt": prompt}
        if self._config.session_id:
            req["session_id"] = self._config.session_id
        if options:
            req["options"] = options
        return json.dumps(req) + "\n"

    def _build_sandbox_cmd(self) -> list[str]:
        """Build the full command line: sandbox wrapper + dscode binary."""
        cmd = self._build_sandbox_cmd_inner()
        self._append_mission_flag(cmd)
        return cmd

    def _mission_home_path(self) -> str:
        """Return the canonical mission file path inside DSCODE_HOME."""
        return os.path.join(self._home or _default_home(), "_mission.md")

    def _append_mission_flag(self, cmd: list[str]) -> None:
        """Append --mission <path> if configured.

        When sandbox is active, the mission file is always placed under
        DSCODE_HOME (guaranteed accessible inside all sandbox backends)
        so that ``--mission`` resolves correctly inside the sandbox.
        """
        cfg = self._config
        if not cfg.mission_file and not cfg.mission_content:
            return

        sandbox_active = cfg.sandbox_backend.strip().lower() != "off"

        if sandbox_active:
            # Sandbox: copy/write to DSCODE_HOME for guaranteed accessibility
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
        """Launch dscode directly — sandboxing is handled by Rust internally."""
        return [self._binary, "--json-rpc"]


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
