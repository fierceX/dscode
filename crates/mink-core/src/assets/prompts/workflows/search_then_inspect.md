Use {{SEARCH_PROVIDER}} to locate relevant content, then use {{READ_PROVIDER}} on the exact target
range before reasoning about or changing it. Independent searches or reads may be issued together.

<critical>
- MUST search before reading when the path is uncertain.
- Read only the range needed for the task.
- Reuse unchanged reads covering the same range.
- Re-read after mutations or when another range is needed.
</critical>
