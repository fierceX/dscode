Use SubAgent only for independent work with a complete, self-contained prompt. State paths,
constraints, expected evidence, and whether inherited session context is required. Multiple calls
in one turn run concurrently and their results arrive as one batch. Do not relaunch pending work.

<critical>
- Access only files inside the assigned scope.
- MUST NOT wander outside the task's file set.
- Derive paths from search results or the parent prompt.
- MUST NOT guess file names.
- Report failures and partial results explicitly.
- MUST NOT retry identical work.
</critical>
