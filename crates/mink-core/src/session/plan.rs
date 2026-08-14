use crate::session::atomic_file::atomic_replace;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Project the confirmed `<current-plan>` into a message list.
///
/// `tail=true` appends the plan as the **last** message (default): the plan
/// stays outside the cacheable prefix, so plan edits no longer invalidate the
/// whole conversation prefix. `tail=false` keeps the legacy head projection
/// (inserted after the leading system messages) as an A/B fallback.
pub fn project_current_plan(
    plan_path: &Path,
    messages: &[serde_json::Value],
    tail: bool,
) -> Result<Vec<serde_json::Value>> {
    let content = match std::fs::read_to_string(plan_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(messages.to_vec());
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "cannot read current plan {}: {error}",
                plan_path.display()
            ));
        }
    };
    let content = content.trim();
    if content.is_empty() {
        return Ok(messages.to_vec());
    }

    let mut projected = messages.to_vec();
    let plan_message = serde_json::json!({
        "role": "system",
        "content": format!("<current-plan>\n{content}\n</current-plan>"),
    });
    if tail {
        projected.push(plan_message);
    } else {
        let insert_at = projected
            .iter()
            .take_while(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("system")
            })
            .count();
        projected.insert(insert_at, plan_message);
    }
    Ok(projected)
}

pub struct PlanStore {
    plan_path: PathBuf,
    draft_path: PathBuf,
    transition_lock: Mutex<()>,
}

impl PlanStore {
    pub fn new(plan_path: PathBuf, draft_path: PathBuf) -> Self {
        Self {
            plan_path,
            draft_path,
            transition_lock: Mutex::new(()),
        }
    }

    pub fn set_draft(&self, content: &str, max_bytes: usize) -> Result<()> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if content.len() > max_bytes {
            bail!(
                "plan draft is too large: {} bytes exceeds limit of {max_bytes} bytes",
                content.len()
            );
        }
        if content.is_empty() {
            return match std::fs::remove_file(&self.draft_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        match self.plan_path.try_exists() {
            Ok(true) => bail!("cannot create a plan draft while a confirmed plan exists"),
            Ok(false) => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot inspect current plan before saving draft: {error}"
                ));
            }
        }
        atomic_replace(&self.draft_path, content.as_bytes())
    }

    pub fn confirm(&self) -> Result<String> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let draft = match std::fs::read(&self.draft_path) {
            Ok(draft) => draft,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("no plan draft found to confirm")
            }
            Err(error) => return Err(anyhow::anyhow!("cannot read plan draft: {error}")),
        };
        if draft.iter().all(u8::is_ascii_whitespace) {
            bail!("no plan draft found to confirm");
        }
        let content = String::from_utf8(draft)
            .map_err(|error| anyhow::anyhow!("plan draft is not valid UTF-8: {error}"))?;
        match self.plan_path.try_exists() {
            Ok(true) => bail!("a confirmed plan already exists"),
            Ok(false) => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot inspect current plan before confirmation: {error}"
                ));
            }
        }
        ensure_parent(&self.plan_path)?;
        std::fs::rename(&self.draft_path, &self.plan_path)
            .map_err(|error| anyhow::anyhow!("cannot commit plan draft atomically: {error}"))?;
        Ok(content)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self
            .transition_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let current = match std::fs::read(&self.plan_path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("no confirmed plan found to clear")
            }
            Err(error) => return Err(anyhow::anyhow!("cannot read current plan: {error}")),
        };
        if current.iter().all(u8::is_ascii_whitespace) {
            bail!("no confirmed plan found to clear");
        }
        match std::fs::remove_file(&self.draft_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot remove stale plan draft before clearing confirmed plan: {error}"
                ));
            }
        }
        std::fs::remove_file(&self.plan_path)
            .map_err(|error| anyhow::anyhow!("cannot clear confirmed plan: {error}"))
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plan path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (PathBuf, PlanStore) {
        let root = std::env::temp_dir().join(format!(
            "mink-{name}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = PlanStore::new(root.join("plan.md"), root.join("plan.draft"));
        (root, store)
    }

    #[test]
    fn draft_confirm_clear_is_a_valid_lifecycle() {
        let (root, store) = store("plan-lifecycle");
        store.set_draft("# Plan\n", 1024).unwrap();
        assert_eq!(store.confirm().unwrap(), "# Plan\n");
        assert_eq!(
            std::fs::read_to_string(root.join("plan.md")).unwrap(),
            "# Plan\n"
        );
        assert!(!root.join("plan.draft").exists());
        store.clear().unwrap();
        assert!(!root.join("plan.md").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_transitions_fail_without_mutating_state() {
        let (root, store) = store("plan-invalid");
        assert!(store.confirm().is_err());
        assert!(store.clear().is_err());
        assert!(store.set_draft("oversized", 4).is_err());
        store.set_draft("", 4).unwrap();
        assert!(!root.join("plan.md").exists());
        assert!(!root.join("plan.draft").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn confirmed_plan_blocks_new_drafts_and_clear_removes_stale_draft() {
        let (root, store) = store("plan-confirmed-state");
        store.set_draft("current plan", 1024).unwrap();
        store.confirm().unwrap();

        let error = store.set_draft("next plan", 1024).unwrap_err().to_string();
        assert!(error.contains("confirmed plan exists"), "{error}");
        assert!(!root.join("plan.draft").exists());

        std::fs::write(root.join("plan.draft"), "legacy stale draft").unwrap();
        store.clear().unwrap();
        assert!(!root.join("plan.md").exists());
        assert!(!root.join("plan.draft").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn plan_content_message() -> serde_json::Value {
        serde_json::json!({
            "role": "system",
            "content": "<current-plan>\n1. implement\n2. verify\n</current-plan>",
        })
    }

    #[test]
    fn current_plan_projection_is_dynamic_and_not_persisted() {
        let (root, store) = store("plan-projection");
        let base = vec![
            serde_json::json!({"role": "system", "content": "<context-snapshot>old</context-snapshot>"}),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];

        // No plan file: both modes return the base untouched.
        assert_eq!(
            project_current_plan(&root.join("plan.md"), &base, false).unwrap(),
            base
        );
        assert_eq!(
            project_current_plan(&root.join("plan.md"), &base, true).unwrap(),
            base
        );

        store.set_draft("1. implement\n2. verify\n", 1024).unwrap();
        store.confirm().unwrap();

        // Legacy head projection: plan inserted after the leading system messages.
        let projected = project_current_plan(&root.join("plan.md"), &base, false).unwrap();
        assert_eq!(projected.len(), base.len() + 1);
        assert_eq!(projected[0], base[0]);
        assert_eq!(projected[1], plan_content_message());
        assert_eq!(projected[2], base[1]);
        assert_eq!(base.len(), 2);

        // Default tail projection: plan appended as the last message.
        let projected = project_current_plan(&root.join("plan.md"), &base, true).unwrap();
        assert_eq!(projected.len(), base.len() + 1);
        assert_eq!(projected[0], base[0]);
        assert_eq!(projected[1], base[1]);
        assert_eq!(projected[2], plan_content_message());

        store.clear().unwrap();
        assert_eq!(
            project_current_plan(&root.join("plan.md"), &base, true).unwrap(),
            base
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
