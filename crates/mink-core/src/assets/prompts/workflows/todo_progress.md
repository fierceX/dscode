Use {{TODO_ADVANCE_PROVIDER}} for progress transitions. It can activate pending items, complete or
pause in_progress items, and reopen completed items. A coherent active batch may contain multiple
related items; select and advance that batch according to the work rather than following list order
mechanically.

Only complete items whose outcomes have been verified. Before ending a turn, complete active work,
pause it, or clearly report why it remains active. Base transitions on the highest successful
revision visible in a {{TODO_READ_PROVIDER}} snapshot or appended todo event. On a stale revision,
read the current state and retry from that state.

<critical>
- Complete only outcomes supported by verification.
- Resolve or report active work before ending.
- Advance related items as a coherent batch.
- MUST NOT follow list order mechanically.
</critical>
