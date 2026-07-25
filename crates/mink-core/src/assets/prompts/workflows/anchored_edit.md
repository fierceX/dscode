For local anchored changes, obtain a fresh non-raw snapshot header with {{SNAPSHOT_PROVIDER}} and
pass that header to {{EDIT_PROVIDER}}. Keep ranges tight and combine hunks from one snapshot.
After stale, missing, uncovered, or no-op results, stop and obtain a new snapshot. A successful
edit returns a new header that may ground an immediate follow-up within its displayed range.
