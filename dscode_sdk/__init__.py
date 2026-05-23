"""
dscode SDK — Python wrapper for sandboxed agent execution.

Supports Linux (nsjail / bubblewrap) and macOS (sandbox-exec).
The dscode binary is spawned as a child process inside a sandbox,
communicating via JSON-RPC over stdin/stdout.

Usage::

    from dscode_sdk import SandboxConfig, AgentSession

    session = AgentSession(SandboxConfig(
        dscode_binary="./target/release/dscode",
        read_dirs=["/project/src", "/project/tests"],
        write_dirs=["/project/src"],
        allow_bash=True,
        allow_network=True,
    ))
    result = session.run("Refactor src/handler.rs")
    print(result["text"])
    session.close()
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


# ── Exceptions ───────────────────────────────────────────────────────

class SandboxError(RuntimeError):
    """Raised when the sandbox tool is not available."""

class AgentError(RuntimeError):
    """Raised when the agent process fails."""


# ── SandboxConfig ────────────────────────────────────────────────────

@dataclass
class SandboxConfig:
    """Configuration for a sandboxed agent session.

    Attributes:
        dscode_binary:
            Path to the dscode executable.
        read_dirs:
            Directories the agent is allowed to read from.
            Relative paths are resolved against the current working directory.
        write_dirs:
            Directories the agent is allowed to write to.
        allow_bash:
            Whether the Bash tool is enabled.
        bash_allow_commands:
            Command-name whitelist for Bash. Empty = use built-in deny list only.
        allow_python:
            Whether Python scripts may be executed via Bash.
        allow_network:
            Whether network access is allowed (LLM API requires this).
        allow_sub_agent:
            Whether SubAgent tool is enabled.
        max_memory_mb:
            Maximum memory for the sandboxed process (nsjail cgroup only).
        max_pids:
            Maximum number of processes (nsjail cgroup only).
        timeout_secs:
            Hard timeout for the entire agent run.
        sandbox_backend:
            "auto" | "nsjail" | "bwrap" | "sandbox-exec" | "off".
            "auto" tries nsjail first, then bubblewrap, then falls back.
        api_key:
            DeepSeek API key (env: DEEPSEEK_API_KEY).
        api_url:
            DeepSeek API base URL.
        cwd:
            Working directory for the agent.
        dscode_home:
            Session storage directory (default: a new temp dir).
    """

    dscode_binary: str = "./dscode"

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

    # Backend
    sandbox_backend: str = "auto"

    # API
    api_key: str = ""
    api_url: str = ""

    # Paths
    cwd: Optional[str] = None
    dscode_home: Optional[str] = None


# ── AgentSession ─────────────────────────────────────────────────────

class AgentSession:
    """A single-shot sandboxed agent session.

    Each call to :meth:`run` launches a fresh sandboxed dscode process,
    executes the prompt, collects results, and cleans up.
    """

    def __init__(self, config: SandboxConfig):
        self._config = config
        self._home: Optional[str] = None
        self._work_dir: Optional[str] = None

    # ── Public API ────────────────────────────────────────────────

    def run(self, prompt: str, *, extra_options: Optional[dict] = None) -> dict[str, Any]:
        """Execute a prompt in the sandbox and return the result.

        Returns a dict with keys:
            ``text`` — the agent's text response,
            ``tool_calls`` — list of tool call events,
            ``thinking`` — the agent's reasoning content,
            ``exit_code`` — process exit code (0 = success).
        """
        self._prepare()

        cmd = self._build_sandbox_cmd()
        request = self._build_request(prompt, extra_options)
        env = self._build_env()

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
            stderr = proc.stderr.read()
            raise AgentError(f"Agent process exited early. stderr: {stderr}")

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

        self._cleanup()

        return {
            "text": "".join(text_parts),
            "thinking": "".join(thinking_parts),
            "tool_calls": tool_calls,
            "exit_code": proc.returncode,
            "error": error_message,
        }

    def close(self) -> None:
        """Clean up temporary directories created for this session."""
        self._cleanup()

    # ── Internals ──────────────────────────────────────────────────

    def _prepare(self) -> None:
        """Create temporary directories for this run."""
        if self._home is None:
            self._home = tempfile.mkdtemp(prefix="dscode_")
        if self._work_dir is None:
            self._work_dir = tempfile.mkdtemp(prefix="dscode_work_")

    def _cleanup(self) -> None:
        """Remove temporary directories."""
        for d in (self._home, self._work_dir):
            if d and os.path.isdir(d):
                shutil.rmtree(d, ignore_errors=True)
        self._home = None
        self._work_dir = None

    def _build_env(self) -> dict[str, str]:
        """Build environment variables for the sandboxed process."""
        env = os.environ.copy()
        env["DSCODE_HOME"] = self._home or tempfile.gettempdir()
        if self._config.api_key:
            env["DEEPSEEK_API_KEY"] = self._config.api_key
        if self._config.api_url:
            env["DEEPSEEK_BASE_URL"] = self._config.api_url
        return env

    def _build_request(self, prompt: str, extra_options: Optional[dict]) -> str:
        """Build the JSON-RPC request string."""
        options: dict[str, bool] = {}
        if not self._config.allow_bash:
            options["disable_bash"] = True
        if not self._config.allow_sub_agent:
            options["disable_sub_agent"] = True
        if not self._config.allow_network:
            options["disable_web"] = True
        if extra_options:
            options.update(extra_options)

        req: dict[str, Any] = {"prompt": prompt}
        if options:
            req["options"] = options
        return json.dumps(req) + "\n"

    def _build_sandbox_cmd(self) -> list[str]:
        """Build the full command line: sandbox wrapper + dscode binary."""
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

        raise SandboxError(
            "No sandbox backend available. Install nsjail or bubblewrap, "
            "or set sandbox_backend='off' to disable sandboxing."
        )

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

        # Target binary
        cmd += ["--", cfg.dscode_binary, "--json-rpc"]
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

        # Namespace isolation
        cmd += ["--unshare-pid", "--unshare-ipc", "--unshare-uts"]
        if not cfg.allow_network:
            cmd += ["--unshare-net"]

        # Target binary
        cmd += ["--", cfg.dscode_binary, "--json-rpc"]
        return cmd

    # ── macOS: sandbox-exec ────────────────────────────────────────

    def _build_macos_cmd(self) -> list[str]:
        cfg = self._config
        if cfg.sandbox_backend == "off":
            return self._direct_cmd()

        sb_profile = self._build_sb_profile()
        return ["sandbox-exec", "-p", sb_profile, cfg.dscode_binary, "--json-rpc"]

    def _build_sb_profile(self) -> str:
        cfg = self._config
        cwd = cfg.cwd or os.getcwd()

        lines = ["(version 1)", "(deny default)"]

        # Read dirs
        for d in cfg.read_dirs:
            resolved = self._resolve_dir(d, cwd)
            lines.append(
                f'(allow file-read* file-read-metadata (subpath "{resolved}"))'
            )

        # System paths needed by the binary
        for sys_dir in [
            "/usr/lib", "/usr/libexec", "/usr/share",
            "/System/Library", "/private/var/db/timezone",
            "/dev/null", "/dev/urandom",
        ]:
            lines.append(f'(allow file-read* (subpath "{sys_dir}"))')

        # Write dirs
        for d in cfg.write_dirs:
            resolved = self._resolve_dir(d, cwd)
            lines.append(f'(allow file-write* (subpath "{resolved}"))')

        # Session home
        if self._home:
            lines.append(f'(allow file-write* (subpath "{self._home}"))')

        # Process exec
        if cfg.allow_bash:
            for exe in ["/bin/bash", "/bin/sh", "/bin/cat", "/bin/ls"]:
                lines.append(f'(allow process-exec (literal "{exe}"))')
        for rg_path in ["/usr/local/bin/rg", "/opt/homebrew/bin/rg"]:
            lines.append(f'(allow process-exec (subpath "{rg_path}"))')
        lines.append('(allow process-exec (literal "/usr/bin/diff"))')
        if cfg.allow_python:
            for py in [
                "/usr/bin/python3",
                "/usr/local/bin/python3",
                "/opt/homebrew/bin/python3",
            ]:
                lines.append(f'(allow process-exec (literal "{py}"))')

        # Network
        if cfg.allow_network:
            lines.append("(allow network-outbound)")

        # Basics
        lines.append("(allow sysctl-read)")
        lines.append("(allow signal (target self))")

        return "\n".join(lines)

    # ── Helpers ────────────────────────────────────────────────────

    def _direct_cmd(self) -> list[str]:
        """No sandbox — run dscode directly."""
        return [self._config.dscode_binary, "--json-rpc"]

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
    dscode_binary: str = "./dscode",
    read_dirs: Optional[list[str]] = None,
    write_dirs: Optional[list[str]] = None,
    **kwargs,
) -> dict[str, Any]:
    """One-shot convenience: create a session, run, close.

    >>> result = quick_run("Hello!", read_dirs=["/tmp"])
    >>> print(result["text"])
    """
    session = AgentSession(SandboxConfig(
        dscode_binary=dscode_binary,
        read_dirs=read_dirs or [],
        write_dirs=write_dirs or [],
        **kwargs,
    ))
    try:
        return session.run(prompt)
    finally:
        session.close()
