#!/bin/bash
# Built-in error sensor for dscode.
# Detects Rust compilation errors, test failures, and non-zero exits.
#
# Usage: error.sh <tool_name> <elapsed_ms> <output_len>
#   stdin: full tool output
#
# Output: single-line JSON  {"signals":[...]} or {}
#
# Users can copy and customize this script to add new detection patterns.

tool="$1"
elapsed_ms="$2"
output_len="$3"
output=$(cat)
signals=""

# Rust compilation errors
echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"

# Test failures (Python pytest, Rust cargo test, general)
echo "$output" | grep -q "FAILED\|AssertionError\|Traceback" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Test failure\"},"

# Non-zero exit codes or generic errors
echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"

# Output signals if any, otherwise empty JSON
if [ -n "$signals" ]; then
  echo "{\"signals\":[${signals%,}]}"
else
  echo "{}"
fi
