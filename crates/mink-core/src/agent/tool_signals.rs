use crate::context::AgentSharedContext;
use crate::guard::collector::{Signal, SignalCollector, SignalKind};
use crate::tools::runner::ToolRunResult;
use std::sync::Arc;

pub struct ToolSignalProcessor {
    collector: SignalCollector,
    tool_error_count: u32,
    signals: Vec<Signal>,
}

impl ToolSignalProcessor {
    pub fn new() -> Self {
        Self {
            collector: SignalCollector::new(),
            tool_error_count: 0,
            signals: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.tool_error_count = 0;
        self.signals.clear();
    }

    pub fn tool_error_count(&self) -> u32 {
        self.tool_error_count
    }

    pub fn collected_signals(&self) -> &[Signal] {
        &self.signals
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

        let new_signals = self.collector.collect(
            &result.tool_name,
            &result.content,
            result.exit_code,
            &result.content,
        );
        result.signals = new_signals;
        self.signals.extend(result.signals.clone());

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
