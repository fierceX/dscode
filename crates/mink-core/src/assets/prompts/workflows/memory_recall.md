Recall session history only when prior context can change the current answer.

<critical>
- Skip recall when current instructions fully define the task.
- Recall for continuations, past decisions, ambiguity, or repeated failures.
- Use {{READ_PROVIDER}} on session://current before detailed history search.
- Use {{SEARCH_PROVIDER}} on session://current/history with specific terms.
- Open only matched history ranges with {{READ_PROVIDER}}.
- Stop after no match or six total recall calls.
</critical>
