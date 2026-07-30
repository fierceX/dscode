---
name: pre-code-check
description: Use BEFORE touching any code — read context, search call sites, verify assumptions. Prevents blind edits.
---

# Pre-Code Check

## The Iron Law

NO CODE CHANGES WITHOUT PRIOR INSPECTION AND SEARCH. Do not mutate a target until you have inspected it and searched for all call sites with the active providers.

## The Checklist

Before using any capability that modifies files, complete these steps:

1. **Inspect the target file** — understand the current structure, not just where you're changing
2. **Search for call sites** — use an active content-search provider to find every reference to the function, class, or symbol
3. **Inspect the callers** — examine at least one caller to verify your understanding
4. **Check for existing tests** — search the relevant test locations before adding or changing coverage
5. **Verify assumptions** — confirm that the implementation behaves as expected before changing it

## Red Flags

- Editing a file you haven't inspected first
- Changing a function without searching for its callers
- Assuming "this is only used here" without searching
- Refactoring without inspecting the relevant tests first
