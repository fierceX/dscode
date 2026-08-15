use crate::context::AgentSharedContext;
use crate::guard::collector::{Signal, SignalCollector, SignalKind};
use crate::guard::evidence::EvidenceTracker;
use crate::tools::runner::ToolExecution;
use std::sync::Arc;

pub struct ToolSignalProcessor {
    collector: SignalCollector,
    tool_error_count: u32,
    signals: Vec<Signal>,
    evidence: EvidenceTracker,
}

impl ToolSignalProcessor {
    pub fn new() -> Self {
        Self {
            collector: SignalCollector::new(),
            tool_error_count: 0,
            signals: Vec::new(),
            evidence: EvidenceTracker::default(),
        }
    }

    pub fn reset(&mut self) {
        self.tool_error_count = 0;
        self.signals.clear();
        self.evidence.reset();
    }

    pub fn tool_error_count(&self) -> u32 {
        self.tool_error_count
    }

    #[cfg(test)]
    pub fn collected_signals(&self) -> &[Signal] {
        &self.signals
    }

    /// 本输入累计硬失败数（供 DecisionEngine::decide_with_signals）。
    pub fn hard_failures(&self) -> u32 {
        self.evidence.hard_failures
    }

    /// 本输入累计软失败数（供 DecisionEngine::decide_with_signals 的软失败阈值）。
    pub fn soft_failures(&self) -> u32 {
        self.evidence.soft_failures
    }

    /// Trajectory evidence accumulated during this input.
    pub fn evidence(&self) -> &EvidenceTracker {
        &self.evidence
    }

    pub fn evidence_mut(&mut self) -> &mut EvidenceTracker {
        &mut self.evidence
    }

    pub async fn process(
        &mut self,
        result: &mut ToolExecution,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
        ctx: &Arc<AgentSharedContext>,
        model_label: &str,
    ) {
        let signal_enabled = ctx.config.signal_policy.enabled();
        self.process_with_mode(result, belief, ctx, model_label, signal_enabled)
            .await;
    }

    async fn process_with_mode(
        &mut self,
        result: &mut ToolExecution,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
        ctx: &Arc<AgentSharedContext>,
        model_label: &str,
        signal_enabled: bool,
    ) {
        if !signal_enabled {
            result.signals.clear();
            return;
        }

        // 编译与测试诊断只扫描命令输出；执行状态已经由 ToolStatus 给出。
        let scan_error_patterns = matches!(
            result.result_kind,
            crate::tools::metadata::ToolResultKind::Command
        );
        let new_signals = self.collector.collect(
            &result.tool_name,
            result.status,
            &result.content,
            result.exit_code,
            scan_error_patterns,
        );
        result.signals = new_signals;
        self.signals.extend(result.signals.clone());

        let hard_count = result.signals.iter().filter(|s| s.kind.is_hard()).count();
        let hard = hard_count > 0
            || result
                .status
                .failure_kind()
                .is_some_and(|kind| kind.is_hard());
        let summary = result
            .status
            .failure_kind()
            .map(|kind| kind.label().to_string())
            .or_else(|| {
                result
                    .signals
                    .first()
                    .map(|signal| signal.message.clone())
                    .or_else(|| {
                        (!result.succeeded()).then(|| {
                            result
                                .content
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_string()
                        })
                    })
            })
            .unwrap_or_default();
        let paths = edited_paths(&result.tool_name, &result.tool_args);
        self.evidence.record(
            &result.tool_name,
            &result.tool_args,
            &summary,
            hard,
            !result.succeeded() || !result.signals.is_empty(),
            paths,
        );

        if let Some(bt) = belief {
            bt.observe(&result.signals);
            crate::ui::render_title_snapshot(ctx, model_label, bt.belief()).await;
        }

        if result.signals.iter().any(|s| {
            matches!(
                s.kind,
                SignalKind::ToolError | SignalKind::TestFailure | SignalKind::CompileError
            )
        }) {
            self.tool_error_count += 1;
        }

        for signal in &result.signals {
            ctx.display.render_signal(
                &format!("{:?}", signal.kind),
                signal.severity,
                &signal.message,
            );
            ctx.log_typed_event(crate::events::EventLog::Signal {
                version: 1,
                signal_kind: format!("{:?}", signal.kind),
                severity: signal.severity,
                source_tool: signal.source_tool.clone(),
                exit_code: signal.exit_code,
                matched_pattern: signal.matched_pattern.clone(),
                message: signal.message.clone(),
            });
        }
    }
}

impl Default for ToolSignalProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// 提取一次写类调用涉及的路径，供窗口化回滚定位。
fn edited_paths(tool_name: &str, args: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut paths = Vec::new();
    match tool_name {
        "Write" => {
            if let Some(path) = args.get("path") {
                paths.push(path.clone());
            }
        }
        "Edit" => {
            if let Some(path) = args.get("path") {
                paths.push(path.clone());
            } else if let Some(input) = args.get("input") {
                // Hashline 形态：解析每个 section 头 [PATH#TAG]。
                for line in input.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('[')
                        && let Some(close) = trimmed.find('#')
                    {
                        let path = trimmed[1..close].to_string();
                        if !path.is_empty() {
                            paths.push(path);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    paths
}

#[cfg(test)]
mod tests {
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
            sub_agent_description: None,
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
                source: "Bash".into(),
                detail: "old".into(),
                source_tool: "Bash".into(),
                exit_code: None,
                matched_pattern: None,
                message: "old".into(),
            }],
            plan_command: None,
            needs_finalization: false,
            state_metadata: None,
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
}
