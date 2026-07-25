For complex work, draft the plan at {{PLAN_DRAFT_FILE}} using {{WRITER_PROVIDER}} and ask for
confirmation. While that draft is non-empty and the user has not explicitly confirmed it, treat
every user reply about the plan—including questions, suggestions, objections, and implicit change
requests—as revision feedback: update the draft before responding, then ask for confirmation again.
{{DRAFT_CANCEL_INSTRUCTION}}
Only explicit confirmation permits {{CONFIRM_PROVIDER}}, which locks the draft at {{PLAN_FILE}}.
When the confirmed work is complete, use {{CLEAR_PROVIDER}} to clear the locked plan.
