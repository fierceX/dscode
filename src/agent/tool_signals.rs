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
