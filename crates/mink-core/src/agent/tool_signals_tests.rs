use super::*;
use crate::tools::runner::ToolExecution;
use std::collections::BTreeMap;

fn tool_result(content: &str) -> ToolExecution {
    ToolExecution {
        tool_use_id: "call".into(),
        tool_name: "Bash".into(),
        tool_args: BTreeMap::new(),
        content: content.into(),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_fork: false,
        exit_code: None,
        status: crate::tools::metadata::ToolStatus::Failed(
            crate::tools::metadata::ToolFailureKind::ProcessFailed,
        ),
        result_kind: crate::tools::metadata::ToolResultKind::Command,
        presentation: None,
        artifacts: Vec::new(),
        signals: vec![Signal {
            kind: SignalKind::ToolFailed,
            severity: 1.0,
            source_tool: "Bash".into(),
            exit_code: None,
            matched_pattern: None,
            message: "old".into(),
        }],
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
        image_attachment: None,
    }
}

#[tokio::test]
async fn disabled_mode_clears_existing_signals() {
    let mut processor = ToolSignalProcessor::new();
    let mut result = tool_result("error[E0425]: cannot find value");
    processor
        .process_with_mode(
            &mut result,
            None,
            &crate::regression::test_context_for_agent("tool-signals-off")
                .await
                .unwrap(),
            "flash",
            false,
        )
        .await;
    assert!(result.signals.is_empty());
}

#[tokio::test]
async fn content_tool_failure_with_summary_header_still_produces_hard_signal() {
    let mut processor = ToolSignalProcessor::new();
    let mut result = ToolExecution {
            tool_name: "Read".into(),
            content: "Read(missing.txt)\nError: tool execution failed: Error: file not found or unreadable: missing.txt".into(),
            exit_code: None,
            status: crate::tools::metadata::ToolStatus::Failed(
                crate::tools::metadata::ToolFailureKind::Unknown,
            ),
            result_kind: crate::tools::metadata::ToolResultKind::FileRead,
            signals: Vec::new(),
            ..tool_result("unused")
        };
    let mut belief = crate::agent::belief::BeliefTracker::new(16);
    processor
        .process_with_mode(
            &mut result,
            Some(&mut belief),
            &crate::regression::test_context_for_agent("tool-signals-read-fail")
                .await
                .unwrap(),
            "flash",
            true,
        )
        .await;
    assert!(
        result.signals.iter().any(|s| s.kind.is_hard()),
        "content-tool failure must produce a hard signal even behind the summary header"
    );
    assert!(
        belief.belief() < 0.75,
        "hard failure must drop belief, belief={}",
        belief.belief()
    );
}

#[tokio::test]
async fn compile_error_increments_tool_error_count() {
    let mut processor = ToolSignalProcessor::default();
    let mut result = tool_result("error[E0425]: cannot find value");
    processor
        .process_with_mode(
            &mut result,
            None,
            &crate::regression::test_context_for_agent("tool-signals-error")
                .await
                .unwrap(),
            "flash",
            true,
        )
        .await;
    assert_eq!(processor.tool_error_count(), 1);
    assert!(!processor.collected_signals().is_empty());
}

#[tokio::test]
async fn interrupted_tool_is_not_counted_as_hard_failure() {
    let mut processor = ToolSignalProcessor::default();
    let mut result = ToolExecution {
        status: crate::tools::metadata::ToolStatus::Interrupted,
        signals: Vec::new(),
        ..tool_result("")
    };
    processor
        .process_with_mode(
            &mut result,
            None,
            &crate::regression::test_context_for_agent("tool-signals-interrupted")
                .await
                .unwrap(),
            "flash",
            true,
        )
        .await;
    assert!(result.signals.is_empty());
    assert_eq!(processor.hard_failures(), 0);
    assert_eq!(processor.tool_error_count(), 0);
}

#[test]
fn edited_paths_uses_authoritative_hashline_header_grammar() {
    let mut args = BTreeMap::new();
    args.insert(
        "input".to_string(),
        "[\"dir/a.rs\"#A1B2]\n[*Update File: quoted.rs#BEEF]\nPUT 1.=1:\n+x\n".to_string(),
    );
    assert_eq!(
        edited_paths("Edit", &args),
        vec!["dir/a.rs".to_string(), "quoted.rs".to_string()]
    );
}
