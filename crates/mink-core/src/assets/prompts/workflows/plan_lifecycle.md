For complex work, save the complete proposed plan with {{DRAFT_PROVIDER}} and ask for confirmation.
Until the user explicitly confirms it, treat every reply about the plan—including questions,
suggestions, objections, and implicit change requests—as revision feedback: save the complete
revised draft before responding, then ask for confirmation again. If the user explicitly cancels or
abandons planning, call {{DRAFT_PROVIDER}} with empty content. Only explicit confirmation permits
{{CONFIRM_PROVIDER}}. When the confirmed work is complete, use {{CLEAR_PROVIDER}} to clear it.

<critical>
- Treat non-confirming plan replies as revision feedback.
- Save the complete revision before responding.
- Confirm only after explicit user confirmation.
- Clear only after all confirmed work is complete.
</critical>
