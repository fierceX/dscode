"""Setup script for mink-sdk.

Builds the Rust binary and packages it inside a platform-specific wheel.

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
BINARY_SRC = HERE / "target" / "release" / "mink"
BINARY_DST = HERE / "mink_sdk" / "_binary"


def _build_rust_binary() -> None:
    """Compile the mink Rust binary in release mode."""
    print(":: Building mink Rust binary (release)...", flush=True)
    env = os.environ.copy()
    env["CARGO_TERM_COLOR"] = "always"
    result = subprocess.run(
        ["cargo", "build", "--release"],
        cwd=HERE,
        env=env,
        capture_output=False,
    )
    if result.returncode != 0:
        raise RuntimeError("cargo build --release failed")
    print(":: Rust build complete.", flush=True)


def _copy_binary() -> None:
    """Copy the compiled binary into the Python package directory.

    Always copies if the source binary exists and is newer than the
    destination, ensuring SDK always bundles the latest mink binary.
    """
    src = BINARY_SRC
    dst = BINARY_DST / "mink"

    if not src.exists():
        # On Windows it would be mink.exe, but we don't support Windows yet
        raise RuntimeError(
            f"Binary not found at {src}. Run 'cargo build --release' first."
        )

    # Skip if destination is up-to-date
    if dst.exists() and dst.stat().st_mtime >= src.stat().st_mtime:
        print(f":: Binary at {dst} is up-to-date, skipping copy", flush=True)
        return

    BINARY_DST.mkdir(parents=True, exist_ok=True)
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
