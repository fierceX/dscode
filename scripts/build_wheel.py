#!/usr/bin/env python3
"""Build a platform-specific wheel for mink-agent.

Usage:
    python scripts/build_wheel.py              # full build (Rust + wheel)
    MINK_SDK_SKIP_BUILD=1 python scripts/build_wheel.py   # wheel only

The script auto-detects the current platform and produces a properly
tagged wheel (e.g. ``macosx_11_0_arm64``, ``manylinux_2_35_x86_64``).

Environment variables:
  MINK_SDK_SKIP_BUILD=1   Skip Rust build, wheel only
  MINK_SDK_FEATURES="sdk-bin python-sandbox"
                          Cargo features for mink-core (default: sdk-bin)
  GLIBC_VERSION=2_35      Override manylinux glibc version (Linux only, default: 2_31)
  PLATFORM_TAG=musllinux_1_2_x86_64   Fully override the platform tag (Linux only)
"""

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
BINARY_NAME = "mink-core"
SDK_FEATURES = os.environ.get("MINK_SDK_FEATURES", "sdk-bin")


def cargo_args() -> list[str]:
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


def get_platform_tag() -> str:
    """Return the wheel platform tag for the current build environment."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "darwin":
        mac_ver = platform.mac_ver()[0]
        parts = mac_ver.split(".")
        major = int(parts[0]) if parts else 11
        minor = int(parts[1]) if len(parts) > 1 else 0
        tag_major = max(major, 11)
        tag_minor = minor if major < 11 else 0
        arch = "arm64" if machine in ("arm64", "aarch64") else "x86_64"
        return f"macosx_{tag_major}_{tag_minor}_{arch}"

    elif system == "linux":
        plat = os.environ.get("PLATFORM_TAG")
        if plat:
            return plat
        arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
        glibc_ver = os.environ.get("GLIBC_VERSION", "2_31")
        return f"manylinux_{glibc_ver}_{arch}"

    else:
        raise RuntimeError(f"Unsupported platform: {system}")


def main() -> None:
    tag = get_platform_tag()
    print(f":: Platform tag: {tag}", flush=True)

    dist_dir = HERE / "dist"
    dist_dir.mkdir(exist_ok=True)
    for wheel in dist_dir.glob("*.whl"):
        wheel.unlink()
    shutil.rmtree(HERE / "build", ignore_errors=True)

    # Optionally build Rust binary
    if not os.environ.get("MINK_SDK_SKIP_BUILD"):
        print(f":: Building Rust binary (features: {SDK_FEATURES})...", flush=True)
        subprocess.run(
            cargo_args(),
            cwd=HERE, check=True,
        )

    # Build wheel with platform tag
    print(":: Building wheel...", flush=True)
    # Note: -C<value> with NO space between -C and the value is required
    # by the build module (build v1.5.0+)
    cmd = [
        sys.executable, "-m", "build", "--wheel",
        f"-C--build-option=--plat-name={tag}",
    ]
    build_env = os.environ.copy()
    build_env["MINK_SDK_SKIP_BUILD"] = "1"
    subprocess.run(cmd, cwd=HERE, env=build_env, check=True)

    # Show result
    wheels = list(dist_dir.glob("*.whl"))
    print(f":: Built: {', '.join(str(w) for w in wheels)}", flush=True)


if __name__ == "__main__":
    main()
