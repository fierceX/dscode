"""
dscode SDK — Python wrapper for agent execution (optionally sandboxed).

The ``dscode`` binary is bundled inside the pip package and discovered
automatically.

Sandboxing
----------
* **Linux**: nsjail / bubblewrap (auto-detected, strongly recommended).
* **macOS**: sandbox-exec (built-in). Write restrictions only — reads
  are enforced at the application level. Use ``read_dirs`` / ``write_dirs``
  to control filesystem access.

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
import platform
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


# ── Exceptions ───────────────────────────────────────────────────────

class SandboxError(RuntimeError):
    """Raised when the sandbox tool is not available."""


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
        Linux: ``"auto"`` tries nsjail → bubblewrap → no sandbox.
        macOS: ``"auto"`` uses ``sandbox-exec``. Set to ``"off"`` to
        disable sandboxing entirely.
    api_key:
        DeepSeek API key.  Also read from the ``DEEPSEEK_API_KEY`` env var.
    api_url:
        DeepSeek API base URL.  Also read from ``DEEPSEEK_BASE_URL``.
    model:
        Model name override (e.g. ``"deepseek-chat"``).
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
        """No-op (sessions now use a persistent home directory)."""
        pass

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
                text=True,
            )
        except FileNotFoundError as e:
            raise SandboxError(
                f"Sandbox tool not found ({e}). "
                f"Install nsjail or bubblewrap (Linux), or use macOS built-in sandbox-exec."
            ) from e

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
        if self._config.api_key:
            env["DEEPSEEK_API_KEY"] = self._config.api_key
        if self._config.api_url:
            env["DEEPSEEK_BASE_URL"] = self._config.api_url
        return env

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
        if options:
            req["options"] = options
        return json.dumps(req) + "\n"

    def _build_sandbox_cmd(self) -> list[str]:
        """Build the full command line: sandbox wrapper + dscode binary."""
        cmd = self._build_sandbox_cmd_inner()
        self._append_mission_flag(cmd)
        return cmd

    def _append_mission_flag(self, cmd: list[str]) -> None:
        """Append --mission <path> if configured."""
        if self._config.mission_file:
            cmd.extend(["--mission", self._config.mission_file])
        elif self._config.mission_content:
            path = self._write_mission_content()
            cmd.extend(["--mission", path])

    def _write_mission_content(self) -> str:
        """Write mission_content to a temp file and return its path."""
        path = os.path.join(self._home or _default_home(), "_mission.md")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(self._config.mission_content)
        return path

    def _build_sandbox_cmd_inner(self) -> list[str]:

        if sys.platform == "linux":
            return self._build_linux_cmd()
        elif sys.platform == "darwin":
            return self._build_macos_cmd()
        else:
            raise SandboxError(f"Unsupported platform: {sys.platform}")

    # ── Linux: nsjail / bubblewrap ─────────────────────────────────

    def _build_linux_cmd(self) -> list[str]:
        cfg = self._config
        backend = cfg.sandbox_backend

        if backend in ("nsjail", "auto"):
            if self._binary_exists("nsjail"):
                return self._nsjail_cmd()
            if backend == "nsjail":
                raise SandboxError("nsjail not found in PATH")

        if backend in ("bwrap", "auto"):
            if self._binary_exists("bwrap"):
                return self._bwrap_cmd()
            if backend == "bwrap":
                raise SandboxError("bwrap not found in PATH")

        if backend == "off":
            return self._direct_cmd()

        # No sandbox available — fall through to direct mode
        return self._direct_cmd()

    def _nsjail_cmd(self) -> list[str]:
        cfg = self._config
        cwd = cfg.cwd or os.getcwd()

        cmd = ["nsjail", "--mode", "execve"]

        # Bind mounts
        for d in cfg.read_dirs:
            resolved = self._resolve_dir(d, cwd)
            cmd += ["--bindmount_ro", f"{resolved}:{resolved}"]
        for d in cfg.write_dirs:
            resolved = self._resolve_dir(d, cwd)
            cmd += ["--bindmount", f"{resolved}:{resolved}"]

        # Working directory
        work_dir = (
            self._resolve_dir(cfg.write_dirs[0], cwd) if cfg.write_dirs
            else self._resolve_dir(cfg.read_dirs[0], cwd) if cfg.read_dirs
            else cwd
        )
        cmd += ["--cwd", work_dir]

        # Resource limits
        cmd += ["--cgroup_mem_max", str(cfg.max_memory_mb * 1024 * 1024)]
        cmd += ["--cgroup_pids_max", str(cfg.max_pids)]
        cmd += ["--time_limit", str(cfg.timeout_secs)]

        # Security
        cmd += ["--disable_proc"]
        if not cfg.allow_network:
            cmd += ["--iface_no_lo"]

        # Add home as a write mount so sessions persist
        if self._home:
            cmd += ["--bindmount", f"{self._home}:{self._home}"]

        # Target binary
        cmd += ["--", self._binary, "--json-rpc"]
        return cmd

    def _bwrap_cmd(self) -> list[str]:
        cfg = self._config
        cwd = cfg.cwd or os.getcwd()

        cmd = [
            "bwrap",
            "--dev", "/dev",
            "--proc", "/proc",
            "--tmpfs", "/tmp",
        ]

        # Bind mounts
        for d in cfg.read_dirs:
            resolved = self._resolve_dir(d, cwd)
            cmd += ["--ro-bind", resolved, resolved]
        for d in cfg.write_dirs:
            resolved = self._resolve_dir(d, cwd)
            cmd += ["--bind", resolved, resolved]

        # Add home as a write mount
        if self._home:
            cmd += ["--bind", self._home, self._home]

        # Namespace isolation
        cmd += ["--unshare-pid", "--unshare-ipc", "--unshare-uts"]
        if not cfg.allow_network:
            cmd += ["--unshare-net"]

        # Target binary
        cmd += ["--", self._binary, "--json-rpc"]
        return cmd

    # ── macOS: sandbox-exec ────────────────────────────────────────

    def _build_macos_cmd(self) -> list[str]:
        """Build sandbox-exec command (default on macOS when enabled).

        Strategy matches the Rust codebase:
        1. ``(allow default)`` — everything starts normally
        2. ``(deny file-write* (subpath "/"))`` — block all writes
        3. ``(allow file-write* ...)`` — punch holes for write dirs
        4. No blanket read deny — read restrictions at app level
        """
        cfg = self._config
        if cfg.sandbox_backend == "off":
            return self._direct_cmd()

        sb_profile = self._build_sb_profile()
        return ["sandbox-exec", "-p", sb_profile, self._binary, "--json-rpc"]

    def _build_sb_profile(self) -> str:
        """Build a sandbox-exec profile — write-restriction only.

        Designed to match ``src/sandbox/platform_macos.rs`` in the Rust
        codebase: allow everything by default, then deny all writes, then
        punch holes for write-allowed directories.

        Read restrictions are NOT applied here — they are enforced at
        the application level (path checks in tool implementations).
        """
        cfg = self._config
        cwd = cfg.cwd or os.getcwd()
        real_home = Path.home()

        lines = ["(version 1)"]

        # ═══ Step 1: Allow default — let everything initialize ═══
        lines.append("(allow default)")

        # ═══ Step 2: Write restrictions ═════════════════════════════
        write_dirs = list(cfg.write_dirs)

        # Only install write rules if there are dirs to restrict to
        if write_dirs or cfg.dscode_home:
            # Deny all writes (deny overrides allow regardless of order)
            lines.append('(deny file-write* (subpath "/"))')

            # Punch holes for user-specified write dirs
            for d in write_dirs:
                resolved = self._resolve_dir(d, cwd)
                lines.append(
                    f'(allow file-write* (subpath "{resolved}"))'
                )

            # Always allow dscode session storage
            home_dscode = os.path.join(str(real_home), ".dscode")
            lines.append(
                f'(allow file-write* (subpath "{home_dscode}"))'
            )

            # Always allow temp files (Edit tool diff, env files, etc.)
            lines.append('(allow file-write* (subpath "/tmp"))')
            lines.append('(allow file-write* (subpath "/private/tmp"))')

        # ═══ No blanket read deny ═══════════════════════════════════
        # Read restrictions are enforced by application-level path
        # checks (tools/file.rs), not by sandbox-exec.

        return "\n".join(lines)

    # ── Helpers ────────────────────────────────────────────────────

    def _direct_cmd(self) -> list[str]:
        """No sandbox — run dscode directly."""
        return [self._binary, "--json-rpc"]

    @staticmethod
    def _resolve_dir(path: str, cwd: str) -> str:
        p = Path(path)
        if p.is_absolute():
            return str(p)
        return str(Path(cwd) / p)

    @staticmethod
    def _binary_exists(name: str) -> bool:
        return shutil.which(name) is not None


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
