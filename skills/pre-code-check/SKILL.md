---
name: pre-code-check
description: Use BEFORE touching any code — read context, search call sites, verify assumptions. Prevents blind edits.
---

# Pre-Code Check

## The Iron Law

NO CODE CHANGES WITHOUT PRIOR SEARCH AND READ. If you haven't read the file and searched for all call sites, you cannot use Edit.

## The Checklist

Before using Write, Edit, or any tool that modifies files, complete these steps:

1. **Read the target file** — understand the current structure, not just where you're changing
2. **Grep for call sites** — search all files that reference the function, class, or symbol you're changing:
   ```bash
   grep -rn "function_name" --include="*.rs" --include="*.py" --include="*.js" .
   ```
3. **Read the callers** — at least one caller to verify your understanding
4. **Check for existing tests** — `grep -rn "test_my_change" tests/`
5. **Verify assumptions** — does the function behavior match what you think? Read it.

## Red Flags

- Editing a file you haven't read first
- Changing a function without Grep-ing its callers
- Assuming "this is only used here" without searching
- Refactoring without reading the test file first
