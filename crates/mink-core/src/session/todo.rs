use crate::session::atomic_file::atomic_replace;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const TODO_FILE_VERSION: u32 = 1;
const MAX_TODO_ITEMS: usize = 256;
const MAX_TODO_CONTENT_BYTES: usize = 1024;
const MAX_TODO_CHANGES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoSnapshot {
    pub version: u32,
    pub revision: u64,
    pub next_id: u64,
    pub items: Vec<TodoItem>,
}

impl Default for TodoSnapshot {
    fn default() -> Self {
        Self {
            version: TODO_FILE_VERSION,
            revision: 0,
            next_id: 1,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoAdd {
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoUpdate {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TodoChanges {
    pub add: Vec<TodoAdd>,
    pub update: Vec<TodoUpdate>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoStructureChange {
    Added {
        id: String,
        status: TodoStatus,
        content: String,
    },
    Updated {
        id: String,
        content: String,
    },
    Removed {
        id: String,
    },
}

#[derive(Debug, Clone)]
pub struct TodoStructureResult {
    pub snapshot: TodoSnapshot,
    pub changes: Vec<TodoStructureChange>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TodoTransitions {
    pub complete: Vec<String>,
    pub activate: Vec<String>,
    pub pause: Vec<String>,
    pub reopen: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoTransitionResult {
    pub snapshot: TodoSnapshot,
    pub completed: Vec<String>,
    pub activated: Vec<String>,
    pub paused: Vec<String>,
    pub reopened: Vec<String>,
}

pub struct TodoStore {
    path: PathBuf,
    state: Mutex<TodoSnapshot>,
}

impl TodoStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<TodoSnapshot>(&bytes)
                .map_err(|error| anyhow::anyhow!("cannot parse {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TodoSnapshot::default(),
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot read todo state {}: {error}",
                    path.display()
                ));
            }
        };
        validate_snapshot(&state)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn snapshot(&self) -> TodoSnapshot {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn apply_structure(
        &self,
        base_revision: u64,
        changes: TodoChanges,
    ) -> Result<TodoStructureResult> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ensure!(
            guard.revision == base_revision,
            "stale todo revision: expected {}, got {base_revision}; read the current todo state and retry",
            guard.revision
        );
        let change_count = changes.add.len() + changes.update.len() + changes.remove.len();
        ensure!(
            change_count > 0,
            "todo update must contain at least one change"
        );
        ensure!(
            change_count <= MAX_TODO_CHANGES,
            "todo update contains {change_count} changes; maximum is {MAX_TODO_CHANGES}"
        );

        let mut next = guard.clone();
        let mut applied = Vec::with_capacity(change_count);
        let existing_ids = guard
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut touched = BTreeSet::new();
        for update in &changes.update {
            ensure!(
                existing_ids.contains(update.id.as_str()),
                "unknown todo item '{}'",
                update.id
            );
            ensure!(
                touched.insert(update.id.as_str()),
                "todo item '{}' is changed more than once in one update",
                update.id
            );
            ensure!(
                !update.content.trim().is_empty(),
                "todo update for '{}' must provide replacement content",
                update.id
            );
            validated_content(&update.content)?;
        }
        for id in &changes.remove {
            ensure!(
                existing_ids.contains(id.as_str()),
                "unknown todo item '{id}'"
            );
            ensure!(
                touched.insert(id.as_str()),
                "todo item '{id}' is changed more than once in one update"
            );
            let item = guard
                .items
                .iter()
                .find(|item| item.id == *id)
                .expect("todo references were validated before applying changes");
            ensure!(
                item.status != TodoStatus::InProgress,
                "active todo item '{id}' must be paused or completed before removal"
            );
        }
        for add in &changes.add {
            validated_content(&add.content)?;
        }
        let final_len = next.items.len() + changes.add.len() - changes.remove.len();
        ensure!(
            final_len <= MAX_TODO_ITEMS,
            "todo list cannot exceed {MAX_TODO_ITEMS} items"
        );

        for add in changes.add {
            let content = validated_content(&add.content)?;
            let id = format!("T{:04}", next.next_id);
            next.next_id = next
                .next_id
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("todo id sequence overflow"))?;
            let status = TodoStatus::Pending;
            next.items.push(TodoItem {
                id: id.clone(),
                content: content.clone(),
                status,
            });
            applied.push(TodoStructureChange::Added {
                id,
                status,
                content,
            });
        }

        for update in changes.update {
            let item = next
                .items
                .iter_mut()
                .find(|item| item.id == update.id)
                .expect("todo references were validated before applying changes");
            ensure!(
                item.status != TodoStatus::Completed,
                "completed todo item '{}' must be reopened before editing",
                update.id
            );
            let content = validated_content(&update.content)?;
            ensure!(
                item.content != content,
                "todo update for '{}' is a no-op",
                update.id
            );
            item.content = content.clone();
            applied.push(TodoStructureChange::Updated {
                id: update.id,
                content,
            });
        }

        for id in changes.remove {
            let index = next
                .items
                .iter()
                .position(|item| item.id == id)
                .expect("todo references were validated before applying changes");
            next.items.remove(index);
            applied.push(TodoStructureChange::Removed { id });
        }

        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("todo revision overflow"))?;
        validate_snapshot(&next)?;
        persist_snapshot(&self.path, &next)?;
        *guard = next.clone();
        Ok(TodoStructureResult {
            snapshot: next,
            changes: applied,
        })
    }

    pub fn advance(
        &self,
        base_revision: u64,
        transitions: TodoTransitions,
    ) -> Result<TodoTransitionResult> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ensure!(
            guard.revision == base_revision,
            "stale todo revision: expected {}, got {base_revision}; read the current todo state and retry",
            guard.revision
        );
        let change_count = transitions.complete.len()
            + transitions.activate.len()
            + transitions.pause.len()
            + transitions.reopen.len();
        ensure!(
            change_count > 0,
            "todo progress update must contain at least one transition"
        );
        ensure!(
            change_count <= MAX_TODO_CHANGES,
            "todo progress update contains {change_count} transitions; maximum is {MAX_TODO_CHANGES}"
        );

        let mut touched = BTreeSet::new();
        for id in transitions
            .complete
            .iter()
            .chain(&transitions.activate)
            .chain(&transitions.pause)
            .chain(&transitions.reopen)
        {
            ensure!(
                touched.insert(id.as_str()),
                "todo item '{id}' is transitioned more than once in one update"
            );
            ensure!(
                guard.items.iter().any(|item| item.id == *id),
                "unknown todo item '{id}'"
            );
        }

        // 模型常直接 complete 一个 pending 条目，因此自动先激活再完成；
        // in_progress 正常完成，已完成的
        // 重复 complete 仍拒绝。
        let mut complete_ids = Vec::new();
        let mut auto_activated = Vec::new();
        for id in &transitions.complete {
            let status = guard
                .items
                .iter()
                .find(|item| item.id == *id)
                .map(|item| item.status);
            match status {
                Some(TodoStatus::InProgress) => complete_ids.push(id.clone()),
                Some(TodoStatus::Pending) => auto_activated.push(id.clone()),
                Some(TodoStatus::Completed) => {
                    bail!("todo item '{id}' is already completed")
                }
                None => unreachable!("unknown todo item already checked above"),
            }
        }
        validate_transitions(&guard, &complete_ids, TodoStatus::InProgress, "complete")?;
        validate_transitions(
            &guard,
            &transitions.activate,
            TodoStatus::Pending,
            "activate",
        )?;
        validate_transitions(&guard, &transitions.pause, TodoStatus::InProgress, "pause")?;
        validate_transitions(&guard, &transitions.reopen, TodoStatus::Completed, "reopen")?;

        let mut next = guard.clone();
        set_statuses(&mut next, &transitions.complete, TodoStatus::Completed);
        set_statuses(&mut next, &transitions.activate, TodoStatus::InProgress);
        set_statuses(&mut next, &transitions.pause, TodoStatus::Pending);
        set_statuses(&mut next, &transitions.reopen, TodoStatus::Pending);
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("todo revision overflow"))?;
        validate_snapshot(&next)?;
        persist_snapshot(&self.path, &next)?;
        *guard = next.clone();
        let mut activated = transitions.activate.clone();
        activated.extend(auto_activated);
        Ok(TodoTransitionResult {
            snapshot: next,
            completed: transitions.complete,
            activated,
            paused: transitions.pause,
            reopened: transitions.reopen,
        })
    }
}

pub fn render_current_todos(snapshot: &TodoSnapshot, read_provider: &str) -> String {
    let pending = snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::Pending)
        .count();
    let active = snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .collect::<Vec<_>>();
    let completed = snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::Completed)
        .count();
    let mut content = format!(
        "<current-todos revision=\"{}\" pending=\"{pending}\" in_progress=\"{}\" completed=\"{completed}\">",
        snapshot.revision,
        active.len()
    );
    if active.is_empty() && pending > 0 {
        content.push_str(&format!(
            "\nPending todo items exist, but none are active. Call {read_provider} before selecting the next batch."
        ));
    } else if !active.is_empty() {
        content.push_str("\nActive batch:");
        for item in active {
            content.push_str(&format!(
                "\n- {}: {}",
                item.id,
                escape_prompt_markup(&item.content)
            ));
        }
    }
    content.push_str("\n</current-todos>");
    content
}

pub fn todo_state_metadata(revision: u64, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "todo_revision": revision,
        "todo_state_kind": kind,
    })
}

pub fn sync_message(snapshot: &TodoSnapshot, read_provider: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": format!(
            "<todo-sync revision=\"{}\">\nThe persisted todo state is newer than the active conversation. This appended projection is authoritative.\n</todo-sync>\n\n{}",
            snapshot.revision,
            render_current_todos(snapshot, read_provider),
        ),
        "_mink": todo_state_metadata(snapshot.revision, "sync"),
    })
}

pub fn visible_revision(messages: &[serde_json::Value]) -> Result<u64> {
    let mut latest = 0_u64;
    for message in messages {
        if let Some(revision) = metadata_revision(message)? {
            latest = latest.max(revision);
        }
        if let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array) {
            for block in blocks {
                if let Some(revision) = metadata_revision(block)? {
                    latest = latest.max(revision);
                }
            }
        }
    }
    Ok(latest)
}

fn metadata_revision(value: &serde_json::Value) -> Result<Option<u64>> {
    let Some(raw) = value
        .get("_mink")
        .and_then(|metadata| metadata.get("todo_revision"))
    else {
        return Ok(None);
    };
    raw.as_u64()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("invalid internal todo revision metadata"))
}

pub(crate) fn escape_prompt_markup(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn validated_content(content: &str) -> Result<String> {
    let content = content.trim();
    ensure!(!content.is_empty(), "todo item content is required");
    ensure!(
        content.len() <= MAX_TODO_CONTENT_BYTES,
        "todo item content exceeds {MAX_TODO_CONTENT_BYTES} bytes"
    );
    ensure!(
        !content.chars().any(char::is_control),
        "todo item content must be a single printable line"
    );
    Ok(content.to_string())
}

fn validate_transitions(
    snapshot: &TodoSnapshot,
    ids: &[String],
    expected: TodoStatus,
    operation: &str,
) -> Result<()> {
    for id in ids {
        let item = snapshot
            .items
            .iter()
            .find(|item| item.id == *id)
            .expect("todo references were validated before checking transitions");
        ensure!(
            item.status == expected,
            "cannot {operation} todo item '{id}' from status {:?}",
            item.status
        );
    }
    Ok(())
}

fn set_statuses(snapshot: &mut TodoSnapshot, ids: &[String], status: TodoStatus) {
    for id in ids {
        snapshot
            .items
            .iter_mut()
            .find(|item| item.id == *id)
            .expect("todo references were validated before applying transitions")
            .status = status;
    }
}

fn persist_snapshot(path: &Path, snapshot: &TodoSnapshot) -> Result<()> {
    let mut serialized = serde_json::to_vec_pretty(snapshot)?;
    serialized.push(b'\n');
    atomic_replace(path, &serialized)
}

fn validate_snapshot(snapshot: &TodoSnapshot) -> Result<()> {
    ensure!(
        snapshot.version == TODO_FILE_VERSION,
        "unsupported todo file version {}; expected {TODO_FILE_VERSION}",
        snapshot.version
    );
    ensure!(snapshot.next_id > 0, "todo next_id must be positive");
    ensure!(
        snapshot.items.len() <= MAX_TODO_ITEMS,
        "todo list contains {} items; maximum is {MAX_TODO_ITEMS}",
        snapshot.items.len()
    );
    let mut ids = BTreeSet::new();
    let mut max_id = 0_u64;
    for item in &snapshot.items {
        ensure!(ids.insert(&item.id), "duplicate todo id '{}'", item.id);
        let numeric = parse_todo_id(&item.id)?;
        ensure!(
            item.id == format!("T{numeric:04}"),
            "todo item '{}' has a non-canonical id",
            item.id
        );
        max_id = max_id.max(numeric);
        let content = validated_content(&item.content)?;
        ensure!(
            content == item.content,
            "todo item '{}' content has surrounding whitespace",
            item.id
        );
    }
    ensure!(
        snapshot.next_id > max_id,
        "todo next_id {} must be greater than existing id sequence {max_id}",
        snapshot.next_id
    );
    Ok(())
}

fn parse_todo_id(id: &str) -> Result<u64> {
    let digits = id
        .strip_prefix('T')
        .filter(|digits| !digits.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid todo id '{id}'"))?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("invalid todo id '{id}'");
    }
    digits
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid todo id '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn store(name: &str) -> (PathBuf, TodoStore) {
        let root = std::env::temp_dir().join(format!(
            "mink-todo-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("todos.json");
        let store = TodoStore::load(path).unwrap();
        (root, store)
    }

    fn add(content: &str) -> TodoAdd {
        TodoAdd {
            content: content.into(),
        }
    }

    #[test]
    fn complete_pending_item_auto_activates() {
        // 直接 complete 一个 pending 条目时自动先激活再完成。
        let (root, store) = store("autoactivate");
        let result = store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("first"), add("second")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        // first 仍 pending；second 先激活再 complete。
        let transition = store
            .advance(
                result.snapshot.revision,
                TodoTransitions {
                    complete: vec!["T0001".to_string()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        assert!(transition.activated.contains(&"T0001".to_string()));
        assert!(transition.completed.contains(&"T0001".to_string()));
        let item = transition
            .snapshot
            .items
            .iter()
            .find(|item| item.id == "T0001")
            .unwrap();
        assert_eq!(item.status, TodoStatus::Completed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn complete_already_completed_item_still_rejected() {
        let (root, store) = store("doublecomplete");
        let result = store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("first")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        let r1 = store
            .advance(
                result.snapshot.revision,
                TodoTransitions {
                    complete: vec!["T0001".to_string()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        let err = store
            .advance(
                r1.snapshot.revision,
                TodoTransitions {
                    complete: vec!["T0001".to_string()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("already completed"), "{err}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_file_starts_at_revision_zero_and_persists_ids() {
        let (root, store) = store("persist");
        let result = store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("first"), add("second")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        assert_eq!(result.snapshot.revision, 1);
        assert_eq!(result.snapshot.items[0].id, "T0001");
        assert_eq!(result.snapshot.items[1].id, "T0002");
        assert_eq!(result.snapshot.next_id, 3);

        let reloaded = TodoStore::load(root.join("todos.json")).unwrap();
        assert_eq!(reloaded.snapshot(), result.snapshot);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_update_is_atomic_and_rejects_stale_revision() {
        let (root, store) = store("batch");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("first"), add("second")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        let updated = store
            .apply_structure(
                1,
                TodoChanges {
                    update: vec![TodoUpdate {
                        id: "T0002".into(),
                        content: "second revised".into(),
                    }],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        assert_eq!(updated.snapshot.revision, 2);
        let before = std::fs::read(root.join("todos.json")).unwrap();
        assert!(
            store
                .apply_structure(
                    1,
                    TodoChanges {
                        remove: vec!["T0001".into()],
                        ..TodoChanges::default()
                    }
                )
                .is_err()
        );
        assert_eq!(std::fs::read(root.join("todos.json")).unwrap(), before);
        assert_eq!(store.snapshot(), updated.snapshot);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_change_rolls_back_the_entire_batch() {
        let (root, store) = store("rollback");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("first")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        let before = store.snapshot();
        let error = store
            .apply_structure(
                1,
                TodoChanges {
                    update: vec![TodoUpdate {
                        id: "T0001".into(),
                        content: "changed".into(),
                    }],
                    remove: vec!["missing".into()],
                    ..TodoChanges::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown todo item"), "{error}");
        assert_eq!(store.snapshot(), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persistence_failure_does_not_advance_in_memory_revision() {
        let (root, store) = store("write-failure");
        std::fs::create_dir(root.join("todos.json")).unwrap();
        assert!(
            store
                .apply_structure(
                    0,
                    TodoChanges {
                        add: vec![add("must not commit")],
                        ..TodoChanges::default()
                    },
                )
                .is_err()
        );
        assert_eq!(store.snapshot(), TodoSnapshot::default());
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_file_fails_closed() {
        let (root, _) = store("corrupt");
        std::fs::write(root.join("todos.json"), b"{broken").unwrap();
        assert!(TodoStore::load(root.join("todos.json")).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_cannot_target_an_id_created_in_the_same_write() {
        let (root, store) = store("guessed-id");
        let error = store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("new")],
                    update: vec![TodoUpdate {
                        id: "T0001".into(),
                        content: "guessed".into(),
                    }],
                    ..TodoChanges::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown todo item"), "{error}");
        assert_eq!(store.snapshot(), TodoSnapshot::default());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn projection_includes_only_active_items_and_counts() {
        let (root, store) = store("projection-active");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("pending"), add("<active>"), add("done")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        store
            .advance(
                1,
                TodoTransitions {
                    activate: vec!["T0002".into()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        let content = render_current_todos(&store.snapshot(), "TodoRead");
        assert!(content.contains("pending=\"2\""));
        assert!(content.contains("T0002: &lt;active&gt;"));
        assert!(!content.contains("T0001: pending"));
        assert!(!content.contains("T0003: done"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn projection_reminds_to_read_when_pending_has_no_active_batch() {
        let (root, store) = store("projection-pending");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("pending")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        let projected = render_current_todos(&store.snapshot(), "InspectTodos");
        assert!(projected.contains("Call InspectTodos"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn projection_represents_pending_only_state() {
        let (root, store) = store("projection-pending-only");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("done")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        let projected = render_current_todos(&store.snapshot(), "TodoRead");
        assert!(projected.contains("pending=\"1\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn progress_transitions_are_atomic_and_enforce_source_status() {
        let (root, store) = store("transitions");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("active"), add("pending")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        store
            .advance(
                1,
                TodoTransitions {
                    activate: vec!["T0001".into()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        let advanced = store
            .advance(
                2,
                TodoTransitions {
                    complete: vec!["T0001".into()],
                    activate: vec!["T0002".into()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        assert_eq!(advanced.snapshot.revision, 3);
        assert_eq!(advanced.snapshot.items[0].status, TodoStatus::Completed);
        assert_eq!(advanced.snapshot.items[1].status, TodoStatus::InProgress);

        let before = advanced.snapshot;
        let error = store
            .advance(
                3,
                TodoTransitions {
                    complete: vec!["T0001".into()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("already completed"),
            "repeated complete must fail closed: {error}"
        );
        assert_eq!(store.snapshot(), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn structure_changes_create_pending_items_and_cannot_remove_active_items() {
        let (root, store) = store("structure-boundaries");
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("active")],
                    ..TodoChanges::default()
                },
            )
            .unwrap();
        assert_eq!(store.snapshot().items[0].status, TodoStatus::Pending);
        store
            .advance(
                1,
                TodoTransitions {
                    activate: vec!["T0001".into()],
                    ..TodoTransitions::default()
                },
            )
            .unwrap();
        let error = store
            .apply_structure(
                2,
                TodoChanges {
                    remove: vec!["T0001".into()],
                    ..TodoChanges::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be paused or completed"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn visible_revision_uses_internal_metadata_without_parsing_projection_text() {
        let messages = vec![
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "one",
                    "content": "<current-todos revision=\"999\">",
                    "_mink": todo_state_metadata(4, "structure"),
                }],
            }),
            serde_json::json!({
                "role": "system",
                "content": "sync",
                "_mink": todo_state_metadata(7, "sync"),
            }),
        ];
        assert_eq!(visible_revision(&messages).unwrap(), 7);
    }

    #[test]
    fn visible_revision_rejects_corrupt_internal_metadata() {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": "state",
            "_mink": {"todo_revision": "not-a-number"},
        })];
        assert!(visible_revision(&messages).is_err());
    }
}
