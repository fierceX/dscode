Use {{READ_PROVIDER}} (or another active provider that emits the same header) to obtain `[PATH#TAG]`
before calling {{EDIT_PROVIDER}}. Tags bind the complete normalized file content; range reads expose only
the displayed lines while keeping a full session-local version for validation and stale recovery.

<critical>
- Use each successful Edit's returned header and line numbers.
- Re-read after stale, missing context, or unexpected results.
- Use line numbers for small precise ranges.
- Use 'start'..'end' anchors for wide or structural ranges.
- Prefix literal body rows with `+`.
- Body rows represent exact final file text.
</critical>

Supported operations are `PUT N.=M:` (replace), `CUT N.=M` (delete and capture), `PUT <N:` /
`PUT >N:` (insert before/after), `PUT <1:` (head), `PUT >$:` (tail), `REM`, and `MV DEST`.
Single lines use `N.=N`. Prefix every literal body row with `+`; `++text` writes a line beginning
with `+`.

Anchor locators name a range by its first and last line: give the exact text of both lines
(from the latest {{READ_PROVIDER}} output), and the tool operates on everything between them —
no line math, no off-by-one. `PUT 'start'..'end':` replaces the range (inclusive) and
`CUT 'start'..'end':` deletes it. Line numbers (`40.=60`) stay the right tool for small,
already-known ranges; anchors win for wide ranges and structural boundaries (function bodies,
blocks ending in `}` or `)`). Anchors are matched after trimming and must be unique: short lines
such as `}` or `)` are rarely unique — use a longer line (a signature or a distinctive statement)
instead.

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
- Use CUT instead of empty range PUT deletion.
- Use gap PUT instead of rewriting unchanged keeper lines.
- Bodies exclude diff deletions and unchanged context.
- Register PUT operations MUST be bodyless.
- Keep range boundaries outside expression or block interiors.
- Use headers from {{READ_PROVIDER}} or successful {{EDIT_PROVIDER}} calls.
- MUST NOT guess line numbers when anchor text is available.
- MUST NOT use non-unique anchor text.
- MUST NOT guess, invent, or reuse cross-session tags.
{{LARGE_FILE_GUIDANCE}}
</anti-patterns>

Call {{EDIT_PROVIDER}} with exactly one `input` string. It may contain several file sections:

```text
[src/lib.rs#A1B2]
PUT 10.=11:
+replacement line
PUT >20:
+inserted after line 20
```

Same change, two locators — line numbers for a small known range, anchors for a wide or
structural range:

```text
[src/lib.rs#A1B2]
PUT 2.=4:
+    new_body();
+    finish();

PUT 'pub fn run('..'   }':
+pub fn run() {
+    new_body();
+    finish();
+}
```
