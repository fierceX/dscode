---
name: verification
description: Use when about to claim work is complete, fixed, or passing, before committing — requires running verification commands and confirming output before making any success claims
---

# Verification Before Completion

## The Iron Law

NO COMPLETION CLAIMS WITHOUT FRESH VERIFICATION EVIDENCE. If you haven't run the verification command, you cannot claim it passes.

## The Gate

```
BEFORE claiming any status or expressing satisfaction:

1. IDENTIFY: What command proves this claim?
2. RUN: Execute the FULL command (fresh, complete)
3. READ: Full output, check exit code, count failures
4. VERIFY: Does output confirm the claim?
   - If NO: State actual status with evidence
   - If YES: State claim WITH evidence
5. ONLY THEN: Make the claim

Skip any step = not verifying
```

## What Each Claim Requires

| Claim | Requires |
|-------|----------|
| Tests pass | Test command output: 0 failures, 0 errors |
| Build succeeds | Build command: exit 0 |
| Bug fixed | Test original symptom: passes |
| Linter clean | Linter output: 0 errors |
| Work complete | Each task in the current execution plan or checklist verified independently |

## Red Flags

- Using "should", "probably", "seems to"
- Expressing satisfaction before verification ("Great!", "Perfect!", "Done!")
- About to commit without verification
- Thinking "just this once"
- Any wording implying success without having run the command

## Rationalization Prevention

| Excuse | Reality |
|--------|---------|
| "Should work now" | RUN the verification |
| "I'm confident" | Confidence ≠ evidence |
| "Just this once" | No exceptions |
| "Linter passed" | Linter ≠ compiler |
| "I'm tired" | Exhaustion ≠ excuse |
| "Partial check is enough" | Partial proves nothing |
