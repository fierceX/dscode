Choose an active execution provider according to the isolation and host-access needs of the task:

<critical>
- For new JSON or CSV content, compute it in one run.
- Persist files only through an active file mutation provider.
- MUST NOT edit existing structured files through execution providers.
- MUST change the script or approach before retrying failures.
</critical>
