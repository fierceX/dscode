use super::metadata::{ApprovalTier, ToolMetadata, ToolResultKind};
use super::runner::{ToolExec, ToolOutcome};
use crate::context::ToolContext;
use crate::session::todo::{
    TodoAdd, TodoChanges, TodoSnapshot, TodoStatus, TodoStructureChange, TodoStructureResult,
    TodoTransitionResult, TodoTransitions, TodoUpdate, escape_prompt_markup, render_current_todos,
    todo_state_metadata,
};
use crate::ui::{
    TodoChangeDisplay, TodoCountsDisplay, TodoDisplay, TodoItemDisplay, TodoStatusDisplay,
    ToolPresentation,
};
use anyhow::Result;
use serde::Deserialize;
use std::fmt::Write as _;

pub struct TodoReadTool;
pub struct TodoWriteTool;
pub struct TodoAdvanceTool;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TodoReadArgs {
    include_completed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoWriteArgs {
    base_revision: u64,
    #[serde(default)]
    add: Vec<TodoAdd>,
    #[serde(default)]
    update: Vec<TodoUpdate>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoAdvanceArgs {
    base_revision: u64,
    #[serde(default)]
    complete: Vec<String>,
    #[serde(default)]
    activate: Vec<String>,
    #[serde(default)]
    pause: Vec<String>,
    #[serde(default)]
    reopen: Vec<String>,
}

impl ToolExec for TodoReadTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "TodoRead",
            "Read the persisted todo state for the current session.",
            ApprovalTier::Read,
            ToolResultKind::Text,
        )
    }

    fn execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TodoReadArgs = serde_json::from_value(input.clone())?;
        let snapshot = ctx.todo_store.snapshot();
        let mut outcome = ToolOutcome::text(render_snapshot(&snapshot, args.include_completed));
        outcome.state_metadata = Some(todo_state_metadata(snapshot.revision, "snapshot"));
        outcome.presentation = Some(ToolPresentation::Todo(todo_display(
            &snapshot,
            snapshot.items.iter().map(todo_item_display).collect(),
            Vec::new(),
        )));
        Ok(outcome)
    }
}

impl ToolExec for TodoWriteTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "TodoWrite",
            "Apply an incremental update to the persisted todo state.",
            ApprovalTier::Write,
            ToolResultKind::Control,
        )
        .mutating()
        .storm_exempt()
    }

    fn execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TodoWriteArgs = serde_json::from_value(input.clone())?;
        let result = ctx.todo_store.apply_structure(
            args.base_revision,
            TodoChanges {
                add: args.add,
                update: args.update,
                remove: args.remove,
            },
        )?;
        let read_provider = todo_read_provider(ctx);
        let changes = result
            .changes
            .iter()
            .map(|change| match change {
                TodoStructureChange::Added {
                    id,
                    status,
                    content,
                } => TodoChangeDisplay::Added {
                    item: TodoItemDisplay {
                        id: id.clone(),
                        content: content.clone(),
                        status: todo_status_display(*status),
                    },
                },
                TodoStructureChange::Updated { id, content } => TodoChangeDisplay::Updated {
                    id: id.clone(),
                    content: content.clone(),
                },
                TodoStructureChange::Removed { id } => {
                    TodoChangeDisplay::Removed { id: id.clone() }
                }
            })
            .collect();
        let mut outcome = state_change_outcome(
            render_apply_result(&result, read_provider),
            &result.snapshot,
            "structure",
            read_provider,
            ctx.tool_config.tool_result_max_bytes,
        );
        outcome.presentation = Some(ToolPresentation::Todo(todo_display(
            &result.snapshot,
            active_items(&result.snapshot),
            changes,
        )));
        Ok(outcome)
    }
}

impl ToolExec for TodoAdvanceTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "TodoAdvance",
            "Apply progress transitions to persisted todo items.",
            ApprovalTier::Write,
            ToolResultKind::Control,
        )
        .mutating()
        .storm_exempt()
    }

    fn execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TodoAdvanceArgs = serde_json::from_value(input.clone())?;
        let result = ctx.todo_store.advance(
            args.base_revision,
            TodoTransitions {
                complete: args.complete,
                activate: args.activate,
                pause: args.pause,
                reopen: args.reopen,
            },
        )?;
        let read_provider = todo_read_provider(ctx);
        let mut changes = Vec::new();
        changes.extend(
            result
                .completed
                .iter()
                .cloned()
                .map(|id| TodoChangeDisplay::Completed { id }),
        );
        changes.extend(
            result
                .activated
                .iter()
                .cloned()
                .map(|id| TodoChangeDisplay::Activated { id }),
        );
        changes.extend(
            result
                .paused
                .iter()
                .cloned()
                .map(|id| TodoChangeDisplay::Paused { id }),
        );
        changes.extend(
            result
                .reopened
                .iter()
                .cloned()
                .map(|id| TodoChangeDisplay::Reopened { id }),
        );
        let mut outcome = state_change_outcome(
            render_transition_result(&result, read_provider),
            &result.snapshot,
            "progress",
            read_provider,
            ctx.tool_config.tool_result_max_bytes,
        );
        outcome.presentation = Some(ToolPresentation::Todo(todo_display(
            &result.snapshot,
            active_items(&result.snapshot),
            changes,
        )));
        Ok(outcome)
    }
}

fn render_snapshot(snapshot: &TodoSnapshot, include_completed: bool) -> String {
    let (pending, in_progress, completed) = status_counts(snapshot);
    let mut output = format!(
        "<todo-snapshot revision=\"{}\" pending=\"{pending}\" in_progress=\"{in_progress}\" completed=\"{completed}\">",
        snapshot.revision
    );
    let visible = snapshot
        .items
        .iter()
        .filter(|item| include_completed || item.status != TodoStatus::Completed)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        output.push_str(if snapshot.items.is_empty() {
            "\nNo todo items."
        } else {
            "\nNo non-completed todo items."
        });
        output.push_str("\n</todo-snapshot>");
        return output;
    }
    output.push('\n');
    for (index, item) in visible.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let _ = write!(
            output,
            "- [{}] {}: {}",
            status_label(item.status),
            item.id,
            escape_prompt_markup(&item.content)
        );
    }
    output.push_str("\n</todo-snapshot>");
    output
}

fn render_apply_result(result: &TodoStructureResult, read_provider: &str) -> String {
    let mut output = format!(
        "<todo-event revision=\"{}\" kind=\"structure\">",
        result.snapshot.revision,
    );
    for change in &result.changes {
        output.push('\n');
        match change {
            TodoStructureChange::Added {
                id,
                status,
                content,
            } => {
                let _ = write!(
                    output,
                    "- added {id} [{}]: {}",
                    status_label(*status),
                    escape_prompt_markup(content)
                );
            }
            TodoStructureChange::Updated { id, content } => {
                let _ = write!(output, "- updated {id}: {}", escape_prompt_markup(content));
            }
            TodoStructureChange::Removed { id } => {
                let _ = write!(output, "- removed {id}");
            }
        }
    }
    output.push_str("\n</todo-event>\n\n");
    output.push_str(&render_current_todos(&result.snapshot, read_provider));
    output
}

fn render_transition_result(result: &TodoTransitionResult, read_provider: &str) -> String {
    let mut output = format!(
        "<todo-event revision=\"{}\" kind=\"progress\">",
        result.snapshot.revision
    );
    append_ids(&mut output, "Completed", &result.completed);
    append_ids(&mut output, "Activated", &result.activated);
    append_ids(&mut output, "Paused", &result.paused);
    append_ids(&mut output, "Reopened", &result.reopened);
    output.push_str("\n</todo-event>\n\n");
    output.push_str(&render_current_todos(&result.snapshot, read_provider));
    output
}

fn append_ids(output: &mut String, label: &str, ids: &[String]) {
    if !ids.is_empty() {
        let _ = write!(output, "\n{label}: {}", ids.join(", "));
    }
}

fn state_change_outcome(
    content: String,
    snapshot: &TodoSnapshot,
    kind: &str,
    read_provider: &str,
    max_bytes: usize,
) -> ToolOutcome {
    let mut outcome = ToolOutcome::text(content.clone());
    outcome.conversation_content = if content.len() <= max_bytes {
        content
    } else {
        let (pending, in_progress, completed) = status_counts(snapshot);
        format!(
            "<todo-event revision=\"{}\" kind=\"{kind}\">\nThe detailed event exceeded the conversation result limit. Call {read_provider} for the current list.\n</todo-event>\n\n<current-todos revision=\"{}\" pending=\"{pending}\" in_progress=\"{in_progress}\" completed=\"{completed}\">\nCall {read_provider} to inspect the active batch.\n</current-todos>",
            snapshot.revision, snapshot.revision,
        )
    };
    outcome.state_metadata = Some(todo_state_metadata(snapshot.revision, kind));
    outcome
}

fn todo_read_provider(ctx: &ToolContext) -> &'static str {
    use crate::tools::semantic_capabilities::ToolSemanticCapability::TodoInspect;
    ctx.tool_capabilities
        .primary_provider(TodoInspect)
        .expect("todo mutation tools require a resolved TodoInspect provider")
        .tool
}

fn status_counts(snapshot: &TodoSnapshot) -> (usize, usize, usize) {
    snapshot.items.iter().fold(
        (0, 0, 0),
        |(pending, in_progress, completed), item| match item.status {
            TodoStatus::Pending => (pending + 1, in_progress, completed),
            TodoStatus::InProgress => (pending, in_progress + 1, completed),
            TodoStatus::Completed => (pending, in_progress, completed + 1),
        },
    )
}

fn status_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
    }
}

fn todo_status_display(status: TodoStatus) -> TodoStatusDisplay {
    match status {
        TodoStatus::Pending => TodoStatusDisplay::Pending,
        TodoStatus::InProgress => TodoStatusDisplay::InProgress,
        TodoStatus::Completed => TodoStatusDisplay::Completed,
    }
}

fn todo_item_display(item: &crate::session::todo::TodoItem) -> TodoItemDisplay {
    TodoItemDisplay {
        id: item.id.clone(),
        content: item.content.clone(),
        status: todo_status_display(item.status),
    }
}

fn active_items(snapshot: &TodoSnapshot) -> Vec<TodoItemDisplay> {
    snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .map(todo_item_display)
        .collect()
}

fn todo_display(
    snapshot: &TodoSnapshot,
    items: Vec<TodoItemDisplay>,
    changes: Vec<TodoChangeDisplay>,
) -> TodoDisplay {
    let (pending, in_progress, completed) = status_counts(snapshot);
    TodoDisplay {
        revision: snapshot.revision,
        counts: TodoCountsDisplay {
            pending,
            in_progress,
            completed,
        },
        items,
        changes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_hides_completed_items_by_default() {
        let snapshot = TodoSnapshot {
            version: 1,
            revision: 3,
            next_id: 4,
            items: vec![
                crate::session::todo::TodoItem {
                    id: "T0001".into(),
                    content: "active".into(),
                    status: TodoStatus::InProgress,
                },
                crate::session::todo::TodoItem {
                    id: "T0002".into(),
                    content: "done".into(),
                    status: TodoStatus::Completed,
                },
            ],
        };
        let rendered = render_snapshot(&snapshot, false);
        assert!(rendered.contains("T0001: active"));
        assert!(!rendered.contains("T0002: done"));
        assert!(render_snapshot(&snapshot, true).contains("T0002: done"));
    }

    #[test]
    fn todo_write_protocol_rejects_all_status_fields() {
        let update_error = serde_json::from_value::<TodoWriteArgs>(serde_json::json!({
            "base_revision": 2,
            "update": [{"id": "T0001", "status": "completed"}],
        }))
        .unwrap_err()
        .to_string();
        assert!(update_error.contains("unknown field"), "{update_error}");

        let add_error = serde_json::from_value::<TodoWriteArgs>(serde_json::json!({
            "base_revision": 2,
            "add": [{"content": "cannot start active", "status": "in_progress"}],
        }))
        .unwrap_err()
        .to_string();
        assert!(add_error.contains("unknown field"), "{add_error}");
    }

    #[test]
    fn progress_result_contains_delta_and_materialized_projection() {
        let result = TodoTransitionResult {
            snapshot: TodoSnapshot {
                version: 1,
                revision: 5,
                next_id: 3,
                items: vec![
                    crate::session::todo::TodoItem {
                        id: "T0001".into(),
                        content: "done".into(),
                        status: TodoStatus::Completed,
                    },
                    crate::session::todo::TodoItem {
                        id: "T0002".into(),
                        content: "active".into(),
                        status: TodoStatus::InProgress,
                    },
                ],
            },
            completed: vec!["T0001".into()],
            activated: vec!["T0002".into()],
            paused: vec![],
            reopened: vec![],
        };
        let rendered = render_transition_result(&result, "InspectTodos");
        assert!(rendered.contains("<todo-event revision=\"5\" kind=\"progress\">"));
        assert!(rendered.contains("Completed: T0001"));
        assert!(rendered.contains("<current-todos revision=\"5\""));
        assert!(rendered.contains("T0002: active"));
    }
}
