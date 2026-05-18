#!/bin/bash
# ================================================================
# Built-in error sensor for dscode.
# Detects 30+ error patterns across multiple languages and tools.
#
# Usage: error.sh <tool_name> <elapsed_ms> <output_len>
#   stdin: full tool output
#
# Output: single-line JSON  {"signals":[...]} or {}
#
# Weight convention:
#   1.0 = deterministic error (compile fail, assertion fail)
#   0.8 = high-probability error (exception, panic, timeout)
#   0.5 = suspicious (non-zero exit, file not found)
#   0.3 = low-confidence warning (deprecation, unused var)
#
# Users can copy and customize this script to add new patterns.
# ================================================================

tool="$1"
elapsed_ms="$2"
output_len="$3"
output=$(cat)
signals=""

# ================================================================
# Rust (12 patterns)
# ================================================================

echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"
echo "$output" | grep -qi "error: aborting" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust build aborted\"},"
echo "$output" | grep -qi "could not compile" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo build failure\"},"
echo "$output" | grep -qi "mismatched types" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust type mismatch\"},"
echo "$output" | grep -qi "cannot borrow" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust borrow checker\"},"
echo "$output" | grep -qi "cannot move" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust move error\"},"
echo "$output" | grep -qi "undefined\[E0425\]" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust undefined symbol\"},"
echo "$output" | grep -qi "expected.*found" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust type mismatch\"},"
echo "$output" | grep -qi "unused variable" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Rust unused variable\"},"
echo "$output" | grep -qi "unused import" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Rust unused import\"},"
echo "$output" | grep -qi "dead code" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Rust dead code\"},"

# ================================================================
# Cargo test (4 patterns)
# ================================================================

echo "$output" | grep -qE "^test .+ FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo test failure\"},"
echo "$output" | grep -qE "test result: FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo test suite failed\"},"
echo "$output" | grep -qi "panicked at" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust panic\"},"
echo "$output" | grep -qi "assertion failed" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust assertion failed\"},"

# ================================================================
# Python (12 patterns)
# ================================================================

echo "$output" | grep -qi "Traceback (most recent call last)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python exception\"},"
echo "$output" | grep -qi "SyntaxError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python syntax error\"},"
echo "$output" | grep -qi "ModuleNotFoundError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python module not found\"},"
echo "$output" | grep -qi "ImportError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python import error\"},"
echo "$output" | grep -qi "IndentationError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python indentation error\"},"
echo "$output" | grep -qi "TypeError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python type error\"},"
echo "$output" | grep -qi "ValueError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python value error\"},"
echo "$output" | grep -qi "KeyError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python key error\"},"
echo "$output" | grep -qi "IndexError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python index error\"},"
echo "$output" | grep -qi "AttributeError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python attribute error\"},"
echo "$output" | grep -qi "NameError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python name error\"},"

# ================================================================
# Pytest (3 patterns)
# ================================================================

echo "$output" | grep -qE "FAILED [a-zA-Z0-9_/]+\.py::" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Pytest failure\"},"
echo "$output" | grep -qE "ERROR collecting" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Pytest collection error\"},"
echo "$output" | grep -qi "assert .+ == .+" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Pytest assertion\"},"

# ================================================================
# JavaScript / Node.js (8 patterns)
# ================================================================

echo "$output" | grep -qi "ReferenceError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS ReferenceError\"},"
echo "$output" | grep -qi "TypeError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS TypeError\"},"
echo "$output" | grep -qi "SyntaxError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS SyntaxError\"},"
echo "$output" | grep -qi "Cannot find module" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"JS module not found\"},"
echo "$output" | grep -qi "npm ERR!" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"NPM error\"},"
echo "$output" | grep -qi "ERR_PNPM" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"PNPM error\"},"
echo "$output" | grep -qi "Error: Cannot find" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"JS file not found\"},"

# ================================================================
# Jest / Vitest (3 patterns)
# ================================================================

echo "$output" | grep -qE "FAIL .+\.(test|spec)\.(js|ts|jsx|tsx)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Jest/Vitest test failure\"},"
echo "$output" | grep -qi "expect.*toBe\|expect.*toEqual" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Test assertion failed\"},"

# ================================================================
# Go (6 patterns)
# ================================================================

echo "$output" | grep -qi "undefined:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go undefined reference\"},"
echo "$output" | grep -qi "cannot use" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go type mismatch\"},"
echo "$output" | grep -qi "syntax error" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go syntax error\"},"
echo "$output" | grep -qi "unreachable code" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Go unreachable code\"},"
echo "$output" | grep -qE "--- FAIL:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go test failure\"},"

# ================================================================
# Java / JVM (5 patterns)
# ================================================================

echo "$output" | grep -qi "Exception in thread" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java exception\"},"
echo "$output" | grep -qi "error: cannot find symbol" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java symbol not found\"},"
echo "$output" | grep -qi "BUILD FAILED\|BUILD FAILURE" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java build failure\"},"
echo "$output" | grep -qi "NullPointerException" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java NullPointerException\"},"
echo "$output" | grep -qi "ClassNotFoundException" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Java class not found\"},"

# ================================================================
# Docker / Container (4 patterns)
# ================================================================

echo "$output" | grep -qi "Cannot connect to the Docker daemon" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Docker daemon not running\"},"
echo "$output" | grep -qi "image.*not found" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Docker image not found\"},"
echo "$output" | grep -qi "container.*exited" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Docker container exited\"},"
echo "$output" | grep -qi "permission denied while trying to connect" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Docker permission denied\"},"

# ================================================================
# Network (5 patterns)
# ================================================================

echo "$output" | grep -qi "Connection timed out" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Network timeout\"},"
echo "$output" | grep -qi "Connection refused" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Connection refused\"},"
echo "$output" | grep -qi "Could not resolve host" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"DNS resolution failed\"},"
echo "$output" | grep -qi "TLS handshake" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"TLS error\"},"
echo "$output" | grep -qi "reset by peer" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Connection reset by peer\"},"

# ================================================================
# Filesystem (5 patterns)
# ================================================================

echo "$output" | grep -qi "Permission denied" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Permission denied\"},"
echo "$output" | grep -qi "No space left on device" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Disk full\"},"
echo "$output" | grep -qi "No such file or directory" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"File not found\"},"
echo "$output" | grep -qi "Is a directory" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Is a directory\"},"
echo "$output" | grep -qi "Read-only file system" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Read-only filesystem\"},"

# ================================================================
# Process / System (4 patterns)
# ================================================================

echo "$output" | grep -qi "killed\|SIGTERM\|SIGKILL\|SIGSEGV" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Process killed by signal\"},"
echo "$output" | grep -qi "timed out" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Command timed out\"},"
echo "$output" | grep -qi "out of memory\|OOM killer\|Allocation failure" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Out of memory\"},"
echo "$output" | grep -qi "segmentation fault\|core dumped" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Segmentation fault\"},"

# ================================================================
# Generic (3 patterns — always included)
# ================================================================

echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"
echo "$output" | grep -qi "command not found" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Command not found\"},"
echo "$output" | grep -qi "invalid option" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Invalid option\"},"

# ================================================================
# Performance signal (elapsed > 60s or output > 1MB)
# ================================================================

if [ "$elapsed_ms" -gt 60000 ] 2>/dev/null; then
  signals+="{\"kind\":\"perf_warning\",\"weight\":0.5,\"detail\":\"Slow execution\"},"
fi
if [ "$output_len" -gt 1048576 ] 2>/dev/null; then
  signals+="{\"kind\":\"perf_warning\",\"weight\":0.3,\"detail\":\"Large output\"},"
fi

# ================================================================
# Output
# ================================================================
if [ -n "$signals" ]; then
  echo "{\"signals\":[${signals%,}]}"
else
  echo "{}"
fi
