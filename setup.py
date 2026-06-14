"""Setup script for mink-agent.

Builds the SDK Rust binary and packages it inside a platform-specific wheel.

The platform tag is set externally via ``-C--build-option=--plat-name=...``
(see ``scripts/build_wheel.py``).
"""

import os
import shutil
import subprocess
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py

HERE = Path(__file__).parent.resolve()
BINARY_NAME = "mink-core"
BINARY_SRC = HERE / "target" / "release" / BINARY_NAME
BINARY_DST = HERE / "mink_agent" / "_binary"
SDK_FEATURES = os.environ.get("MINK_SDK_FEATURES", "sdk-bin")


def _cargo_args() -> list[str]:
    return [
        "cargo",
        "build",
        "-p",
        "mink-cli",
        "--release",
        "--no-default-features",
        "--features",
        SDK_FEATURES,
        "--bin",
        BINARY_NAME,
    ]


def _build_rust_binary() -> None:
    """Compile the mink-core Rust binary for SDK packaging."""
    args = _cargo_args()
    print(
        f":: Building mink-core Rust binary (release, features: {SDK_FEATURES})...",
        flush=True,
    )
    env = os.environ.copy()
    env["CARGO_TERM_COLOR"] = "always"
    result = subprocess.run(
        args,
        cwd=HERE,
        env=env,
        capture_output=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(args)} failed")
    print(":: Rust build complete.", flush=True)


def _copy_binary() -> None:
    """Copy the compiled binary into the Python package directory.

    Always copies if the source binary exists and is newer than the
    destination, ensuring SDK always bundles the latest mink-core binary.
    """
    src = BINARY_SRC
    dst = BINARY_DST / BINARY_NAME

    if not src.exists():
        raise RuntimeError(
            f"Binary not found at {src}. Run '{' '.join(_cargo_args())}' first."
        )

    BINARY_DST.mkdir(parents=True, exist_ok=True)
    for existing in BINARY_DST.iterdir():
        if existing.is_file() and existing.name != BINARY_NAME:
            existing.unlink()

    # Skip if destination is up-to-date after stale binaries have been removed.
    if dst.exists() and dst.stat().st_mtime >= src.stat().st_mtime:
        print(f":: Binary at {dst} is up-to-date, skipping copy", flush=True)
        return

    shutil.copy2(src, dst)
    # Ensure executable
    dst.chmod(0o755)
    print(f":: Copied binary: {src} -> {dst}", flush=True)


class BuildPyWithBinary(build_py):
    """Custom build_py that builds Rust binary before packaging."""

    def run(self) -> None:
        # Build Rust binary
        if not os.environ.get("MINK_SDK_SKIP_BUILD"):
            _build_rust_binary()
        _copy_binary()
        super().run()


cmdclass = {
    "build_py": BuildPyWithBinary,
}


# ── Read long description ──────────────────────────────────────────

long_description = (HERE / "README.md").read_text(encoding="utf-8") if (HERE / "README.md").exists() else ""


# ── Call setup ─────────────────────────────────────────────────────

setup(
    cmdclass=cmdclass,
    long_description=long_description,
    long_description_content_type="text/markdown",
)
