Use {{READ_PROVIDER}} (or another active provider that emits the same header) to obtain `[PATH#TAG]`
before calling {{EDIT_PROVIDER}}. Tags bind the complete normalized file content; range reads expose only
the displayed lines while keeping a full session-local version for validation and stale recovery.

<critical>
- After every successful Edit, take the new header and line numbers from the Edit response. The tag changes
  whenever file content changes.
- If the response lacks the target context, reports stale content, or the result is unexpected, immediately
  use {{READ_PROVIDER}} again before editing.
- Cover only lines whose final content truly changes. For pure insertion, use a gap PUT (`<N` or `>N`).
- Prefix every literal body row with `+`; the body is the exact final text to write.
</critical>

Call {{EDIT_PROVIDER}} with exactly one `input` string. It may contain several file sections:

```text
[src/lib.rs#A1B2]
PUT 10.=11:
+replacement line
PUT >20:
+inserted after line 20
```

Supported operations are `PUT N.=M:` (replace), `CUT N.=M` (delete and capture), `PUT <N:` /
`PUT >N:` (insert before/after), `PUT <1:` (head), `PUT >$:` (tail), `REM`, and `MV DEST`.
Single lines use `N.=N`. Prefix every literal body row with `+`; `++text` writes a line beginning
with `+`.

Move text with registers: `CUT 5.=9 @fn` captures named register `fn`, and bodyless
`PUT >40 @fn` pastes it. Named registers persist across Edit calls. An unlabeled `CUT` followed by a
bodyless gap `PUT >40` uses a call-local anonymous register; multiple unlabeled cuts make that paste
ambiguous. A named register can replace a range with `PUT 10.=12 @fn`. `MV DEST` may follow line edits
in the same section.

All sections are preflighted before commit. Anchors refer to the original tagged coordinates and may
recover only when every unchanged target maps unambiguously with one consistent offset. Changed,
deleted, split, repeated, or conflicting anchors fail closed. Head/tail-only edits may apply to stale
content with a warning. Seen-line enforcement is {{SEEN_LINE_MODE}}; when enabled, only anchors actually
displayed by the active read/search providers may be used.

Legacy `path + patch`, `@PATH#TAG`, `old_string/new_string`, apply_patch, unified diffs, and syntactic
block locators are unsupported.

<anti-patterns>
- Do not use an empty range PUT to delete; use CUT.
- Do not insert by widening a PUT range and rewriting unchanged keeper lines; use a gap PUT.
- Do not put unified-diff `-` rows or context rows in a body. Bodies contain final file content only.
- A register PUT must be bodyless; do not combine `@register` with literal `+` rows.
- Do not start or end a range in the middle of an expression or structural block.
- Never guess, invent, or reuse a tag from another session. Obtain a verifiable header from a tool response.
</anti-patterns>
