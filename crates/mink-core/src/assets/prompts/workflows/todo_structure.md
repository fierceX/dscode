Use {{TODO_WRITE_PROVIDER}} only for structural changes: add tasks, replace task descriptions, or
remove non-active tasks using their stable IDs. Base each atomic update on the latest revision from
{{TODO_READ_PROVIDER}} or a successful todo event. New tasks always start pending. This provider
does not change task status.

Pass that revision as `base_revision`. Do not guess IDs or revisions. If a write reports a stale revision, call {{TODO_READ_PROVIDER}} and
recompute the intended update. Keep task descriptions concise, concrete, and independently
verifiable.

<critical>
- Base updates on the highest successful revision.
- Re-read and recompute after stale revision errors.
- MUST NOT guess stable IDs or revisions.
</critical>
