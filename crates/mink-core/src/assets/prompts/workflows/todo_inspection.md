Use the persisted todo list for non-trivial multi-step implementation, debugging, refactoring, or
review. Omit it for trivial single-step or purely informational requests.

{{TODO_READ_PROVIDER}} returns the authoritative current revision and stable item IDs. Call it when
the current revision or full backlog is unknown, after a stale-revision error, or when the active
projection says pending work exists but no batch is active. The highest successful revision visible
in the conversation is current; do not reconstruct the list from older events.
