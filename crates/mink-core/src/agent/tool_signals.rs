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

/// EvidenceTracker 的最小轨迹容量：seq_window 小于该值时按该值保留，
/// 注入渲染仍有足够上下文；配置更大的 seq_window 时不会被截断。
const MIN_EVIDENCE_RECORDS: usize = 24;

impl ToolSignalProcessor {
    /// Test-only convenience constructor. Production paths must use
    /// `from_config` with the runtime `SignalConfig`.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::from_config(&crate::config::SignalConfig::default())
    }

    pub(crate) fn from_config(config: &crate::config::SignalConfig) -> Self {
        Self {
            collector: SignalCollector::with_weights(
                config.seq_window,
                config.edit_loop_weights.clone(),
            ),
            tool_error_count: 0,
            signals: Vec::new(),
            evidence: EvidenceTracker::new(
                config.seq_window.max(MIN_EVIDENCE_RECORDS),
                config.evidence_dedup_window,
            ),
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

        // 对外指标：硬失败（ToolFailed/SafetyBlocked/Timeout/ProcessFailed 等
        // 执行状态失败）与软诊断（ToolError/TestFailure/CompileError）都计入；
        // 此前只计软嗅探、Bash 非零退出等硬失败被漏掉，指标失真。
        if hard
            || result.signals.iter().any(|s| {
                matches!(
                    s.kind,
                    SignalKind::ToolError | SignalKind::TestFailure | SignalKind::CompileError
                )
            })
        {
            self.tool_error_count += 1;
        }

        for signal in &result.signals {
            ctx.display.render_signal(
                &format!("{:?}", signal.kind),
                signal.severity,
                &signal.message,
            );
            ctx.log_event(crate::events::EventLog::Signal {
                version: Some(1),
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

#[cfg(test)]
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
                // Hashline 形态：通过权威 header 解析器取每个 section 的路径。
                for line in input.lines() {
                    if let Some(path) = crate::tools::hashline::section_header_path(line) {
                        paths.push(path);
                    }
                }
            }
        }
        _ => {}
    }
    paths
}

#[cfg(test)]
#[path = "tool_signals_tests.rs"]
mod tests;
