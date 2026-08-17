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
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    /// Truncate to a visual terminal width, preserving CJK/wide-character
    /// boundaries. `truncate_visual` uses the single-cell ellipsis used by the
    /// TUI renderer; `truncate_display` uses the conventional three-dot suffix
    /// for CLI text output.
    pub(crate) fn truncate_visual(value: &str, max_width: usize) -> String {
        truncate_with_ellipsis(value, max_width, "…")
    }

    pub(crate) fn truncate_display(value: &str, max_width: usize) -> String {
        truncate_with_ellipsis(value, max_width, "...")
    }

    fn truncate_with_ellipsis(value: &str, max_width: usize, ellipsis: &str) -> String {
        if max_width == 0 {
            return String::new();
        }
        if UnicodeWidthStr::width(value) <= max_width {
            return value.to_string();
        }
        let ellipsis_width = UnicodeWidthStr::width(ellipsis);
        let keep = max_width.saturating_sub(ellipsis_width);
        let mut out = String::new();
        let mut width = 0usize;
        for ch in value.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > keep {
                break;
            }
            out.push(ch);
            width += ch_width;
        }
        out.push_str(ellipsis);
        out
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

pub(crate) mod local;
pub(crate) mod replay;

pub mod cli;
pub(crate) mod ui;

#[cfg(feature = "tui")]
pub(crate) mod tui;
