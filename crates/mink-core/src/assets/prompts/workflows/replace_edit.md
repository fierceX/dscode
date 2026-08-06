Use {{READ_PROVIDER}} to inspect the target, then call {{EDIT_PROVIDER}} with one existing `path` and an
ordered `edits` array. Each entry has a non-empty `old_text`, a `new_text` (empty means deletion), and
optional `all`.

Without `all`, exact text must occur once. Multiple occurrences are rejected with bounded line previews;
add surrounding context to disambiguate. `all: true` replaces every exact occurrence. Later entries see
the content written by earlier entries; a later failure does not roll back already committed entries.

Fuzzy matching is {{FUZZY_MODE}} with threshold {{FUZZY_THRESHOLD}}. When enabled and exact matching
fails, the file is searched using same-line-count normalized windows, relative indentation and bounded
Levenshtein similarity. A fuzzy edit is accepted only when one candidate reaches the threshold, or one
candidate is strongly dominant; equal high-confidence candidates fail closed. Fuzzy `all` never applies
overlapping or ambiguous candidates. Replacement indentation follows the matched file only when the
old-to-actual indentation delta is uniform; explicit indentation-only rewrites are preserved verbatim.

Failures report occurrence previews or the closest similarity and first differing line. This mode does
not use snapshot tags or Hashline syntax. Legacy `patch`, `old_string/new_string`, and Hashline `input`
parameters are unsupported.

<critical>
- Read the exact target range before editing.
- Build old_text from current content.
- Add context until repeated text becomes unique.
- Batch independent edits into one edits array.
- Later failures preserve earlier committed edits.
</critical>
