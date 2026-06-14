---
name: tdd
description: Use when implementing any feature or bugfix, before writing implementation code — write the test first, watch it fail, then implement
---

# Test-Driven Development

## The Iron Law

IF YOU DIDN'T WATCH THE TEST FAIL, YOU DON'T KNOW IF IT TESTS THE RIGHT THING. Always see the red before the green.

## The Cycle

```
RED   → Write a failing test           → Run it, confirm it fails
GREEN → Write minimal code to pass     → Run it, confirm it passes
REFACTOR → Clean up while keeping green → Run tests again
```

## Phase 1: Write the Failing Test

1. **Understand the behavior** before writing a single line of implementation
2. **Write the smallest possible test** that captures the requirement
3. **Run it** — confirm it fails for the expected reason (not a compile error)
4. **If it passes without implementation**: you tested the wrong thing

## Phase 2: Make It Pass

1. **Write the minimal code** needed to pass the test
2. **No "while I'm here" improvements** — just enough to pass
3. **Run the test** — confirm it passes
4. **Run the full test suite** — check for regressions

## Phase 3: Refactor

1. **Clean up** while keeping all tests green
2. **Run tests after each change** — confirm nothing broke
3. **Commit** — one logical change per commit

## Adapting to Project Context

| If the project has... | Then... |
|-----------------------|---------|
| `cargo test` / `pytest` / `jest` | Use the standard test framework. Run with the project's standard command. |
| No test framework | Create a standalone test script: `tests/test_<feature>.sh` or inline assertions |
| Integration tests only | Add a unit-level test if possible. Otherwise write integration test with minimal setup. |
| No existing tests at all | Start with one simple test. Run it. Add more as you add features. |

## Red Flags

- Writing implementation before the test
- The test passes immediately (without having been seen red first)
- Skipping the test run because "it's obvious"
- Making multiple changes before running tests again
- Committing without running the full test suite

## Common Rationalizations

| Excuse | Reality |
|--------|---------|
| "I know what the fix is, skip the test" | You don't know until you prove it. Write the test. |
| "The test is too simple to matter" | Simple tests catch simple regressions. Write it anyway. |
| "I'll add tests after the implementation" | You won't. Write the test first. |
| "This is just a refactor, no new tests needed" | Refactors need tests too. Write one that covers the behavior. |
