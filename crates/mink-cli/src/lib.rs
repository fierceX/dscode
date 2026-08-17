pub(crate) use mink::{runtime, sdk_protocol};

pub(crate) mod config;

pub(crate) mod capabilities {
    pub(crate) use mink::runtime::CapabilitySnapshot;
}

pub(crate) mod sandbox {
    pub(crate) use mink::runtime::reexec_in_sandbox;
}

pub(crate) mod session {
    pub(crate) mod metadata {
        pub(crate) use mink::runtime::session::{SessionRecord, title_from_prompt};

        pub(crate) async fn list_project_sessions(
            home: &std::path::Path,
            cwd: &std::path::Path,
        ) -> anyhow::Result<Vec<SessionRecord>> {
            mink::runtime::session::SessionCatalog::new(home, cwd)
                .list()
                .await
        }
    }

    pub(crate) mod store {
        pub(crate) use mink::runtime::session::{build_tool_summary_from_json, first_line};
    }

    #[cfg(feature = "tui")]
    pub(crate) mod artifacts {
        pub(crate) use mink::runtime::session::ArtifactManager;
    }

    #[cfg(feature = "tui")]
    pub(crate) mod todo {
        pub(crate) use mink::runtime::session::{TodoSnapshot, TodoStatus};
    }
}

pub(crate) mod util {
    pub(crate) fn truncate_str(value: &str, max_chars: usize) -> String {
        let mut chars = value.chars();
        let truncated = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            format!("{truncated}…")
        } else {
            truncated
        }
    }

    pub(crate) fn fmt_k(value: u64) -> String {
        if value >= 1_000_000 {
            format!("{:.1}m", value as f64 / 1_000_000.0)
        } else if value >= 1_000 {
            format!("{:.1}k", value as f64 / 1_000.0)
        } else {
            value.to_string()
        }
    }
}

pub mod cli;
pub(crate) mod ui;

#[cfg(feature = "tui")]
pub(crate) mod tui;
