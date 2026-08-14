use crate::context::AgentSharedContext;
use crate::guard::collector::{Signal, SignalCollector, SignalKind};
use crate::guard::evidence::EvidenceTracker;
use crate::tools::runner::ToolRunResult;
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

    /// 轨迹证据跟踪器（SIGNAL_RESPONSE_REDESIGN R1）。
    pub fn evidence(&self) -> &EvidenceTracker {
        &self.evidence
    }

    pub fn evidence_mut(&mut self) -> &mut EvidenceTracker {
        &mut self.evidence
    }

    pub async fn process(
        &mut self,
        result: &mut ToolRunResult,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
        ctx: &Arc<AgentSharedContext>,
        model_label: &str,
    ) {
        let signal_enabled = crate::config::SignalMode::from_env().enabled();
        self.process_with_mode(result, belief, ctx, model_label, signal_enabled)
            .await;
    }

    async fn process_with_mode(
        &mut self,
        result: &mut ToolRunResult,
        belief: Option<&mut crate::agent::belief::BeliefTracker>,
        ctx: &Arc<AgentSharedContext>,
        model_label: &str,
        signal_enabled: bool,
    ) {
        if !signal_enabled {
            result.signals.clear();
            return;
        }

        // 正则错误模式只适用于命令/诊断输出（Bash/Python 等 Command 结果）。
        // 内容返回型工具（Read/Glob/Grep 等 FileRead/Search）的输出是文件内容或
        // 搜索结果，对其做模式匹配会产生误报（源码中的 "timeout"、"error[E0425]"
        // 等字样），因此只保留 exit_code 和 "Error:" 前缀检测。
        let scan_error_patterns = matches!(
            result.result_kind,
            crate::tools::metadata::ToolResultKind::Command
        );
        let new_signals = self.collector.collect(
            &result.tool_name,
            &result.content,
            result.exit_code,
            &result.content,
            scan_error_patterns,
        );
        result.signals = new_signals;
        // 内容型工具（Read/Write）失败文本是 "Error: ..." 形态，但结果摘要头
        //（"Read(path)"）会挡住收集器的 "Error:" 前缀检测；success=false 与
        // exit_code=None 组合是执行路径的权威判定。此处补一条硬信号，恢复
        // "Error 前缀 = ToolFailed" 的设计意图，保证信念随确定性失败下降
        //（B2 invariant 测试暴露：失败 Read 曾被信念当作成功观察）。
        // SignalRecoveryGuard 的拦截已由 apply_signal_recovery_guard 单独喂回
        // 信念，这里排除以免双重计数。
        if !result.success
            && result.exit_code.is_none()
            && result.tool_name != "SignalRecoveryGuard"
            && !result.signals.iter().any(|s| s.kind.is_hard())
        {
            let message = result
                .content
                .lines()
                .find(|line| line.starts_with("Error:"))
                .or_else(|| result.content.lines().next())
                .unwrap_or("Error")
                .to_string();
            result.signals.push(Signal::synthetic(
                SignalKind::ToolFailed,
                1.0,
                result.tool_name.clone(),
                message,
            ));
        }
        self.signals.extend(result.signals.clone());

        // 轨迹证据（SIGNAL_RESPONSE_REDESIGN R1/S2）：把本批调用压缩成统计事实。
        let hard_count = result.signals.iter().filter(|s| s.kind.is_hard()).count();
        let hard = hard_count > 0 || result.error_code.is_some_and(|kind| kind.is_hard());
        let summary = result
            .error_code
            .map(|kind| kind.label().to_string())
            .or_else(|| {
                if !result.success {
                    result
                        .signals
                        .first()
                        .map(|s| s.message.clone())
                        .or_else(|| result.content.lines().next().map(|line| line.to_string()))
                } else {
                    None
                }
            })
            .unwrap_or_default();
        // C4 清理：恢复守卫的拦截结果是运行时内部反馈（拦截本身已由
        // apply_signal_recovery_guard 合成真实信号喂回信念），不得再进证据统计——
        // 否则一次拦截被双重计为硬失败，污染失败聚类与 hard/soft 计数。
        if result.tool_name != "SignalRecoveryGuard" {
            let paths = edited_paths(&result.tool_name, &result.tool_args);
            // failed 权威判定：success=false 或存在信号或存在结构化错误码。
            let failed =
                !result.success || !result.signals.is_empty() || result.error_code.is_some();
            self.evidence.record(
                &result.tool_name,
                &result.tool_args,
                &summary,
                hard,
                failed,
                paths,
            );
        }

        // 守卫拦截结果不得作为"干净调用"喂入信念：拦截本身已由
        // apply_signal_recovery_guard 以合成失败信号喂回一次，这里若把
        // signals 为空的拦截结果当作完整成功观察（Observation::from_signals
        // 对空信号给出 success_weight=1.0），会抬升信念、压制 Warning 升级。
        if let Some(bt) = belief
            && result.tool_name != "SignalRecoveryGuard"
        {
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

/// 提取一次写类调用涉及的路径（R2 回滚定位用）。
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
    use crate::tools::runner::ToolRunResult;
    use std::collections::BTreeMap;

    fn tool_result(content: &str) -> ToolRunResult {
        ToolRunResult {
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
            success: false,
            error_code: Some(crate::tools::metadata::ToolErrorKind::ProcessFailed),
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
        let mut result = ToolRunResult {
            tool_name: "Read".into(),
            content: "Read(missing.txt)\nError: tool execution failed: Error: file not found or unreadable: missing.txt".into(),
            exit_code: None,
            error_code: None,
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
            result
                .signals
                .iter()
                .any(|s| s.kind.is_hard()),
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
