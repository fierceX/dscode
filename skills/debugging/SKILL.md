---
name: debugging
description: Use when encountering any bug, test failure, or unexpected behavior, before proposing fixes
---

# Systematic Debugging

## The Iron Law

NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST. If you haven't completed Phase 1, you cannot propose fixes.

## Phase 1: Root Cause Investigation

1. **Read the full error output** — don't skip lines, check line numbers and file paths
2. **Reproduce consistently** — can you trigger it reliably? If not, gather more data
3. **Check recent changes** — git diff, recent commits, config changes
4. **Gather evidence** — for multi-step failures, check each layer independently
5. **Trace data flow** — where does the bad value originate? Fix at source, not symptom

## Phase 2: Pattern Analysis

1. **Find working examples** — similar code that does work in the same codebase
2. **Read reference implementations completely** — don't skim
3. **List every difference between working and broken** — don't assume "that can't matter"

## Phase 3: Hypothesis and Testing

1. **Form a single hypothesis** — "I think X is the root cause because Y"
2. **Make the smallest possible change** to test it
3. **Verify before continuing** — did it work? If not, form a new hypothesis
4. **If 3+ fixes failed**: STOP. The architecture may be wrong — question fundamentals

## Phase 4: Implementation

1. **Create a failing reproduction first** — simplest possible test case
2. **Implement one fix** — single change, no "while I'm here" improvements
3. **Verify the original error is gone** — run the exact same command that failed
4. **Run the project's test suite** — confirm no regressions

## Red Flags — STOP and Return to Phase 1

- "Quick fix for now, investigate later"
- "Just try changing X and see if it works"
- "Make multiple changes, then test"
- "Skip the test, I'll manually verify"
- "Should work now" (without having run verification)
- "One more fix attempt" (when already tried 2+)

## Common Rationalizations

| Excuse | Reality |
|--------|---------|
| "Issue is simple, don't need process" | Simple issues have root causes too. Process is fast for simple bugs. |
| "Just try this first, then investigate" | First fix sets the pattern. Do it right from the start. |
| "I'll write test after confirming" | Untested fixes don't stick. Test first proves it. |
| "Multiple fixes at once saves time" | Can't isolate what worked. May introduce new bugs. |
