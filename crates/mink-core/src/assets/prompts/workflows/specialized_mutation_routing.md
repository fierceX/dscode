Route file mutations through the active specialized providers instead of the general shell provider:

<critical>
- MUST use specialized providers for supported file mutations.
- MUST NOT use shell mutation when specialized providers apply.
- Keep unrelated file operations outside shell commands.
</critical>
