use crate::protocol::{
    ErrorEvent, Event, RetryEvent, StopEvent, TextEvent, ThinkingEvent, UsageEvent,
};
use crate::sse::toolcall::build_tool_call_event;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};

// ---- SSE frame parsing (reuses extract_frames from claude module) ----
// OpenAI SSE also uses `data:` lines delimited by `\n\n`.

#[derive(Default, Clone)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

/// Incremental OpenAI SSE parser.
#[derive(Default)]
pub struct OpenAIParser {
    stop_reason: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    saw_text: bool,
    pending_calls: BTreeMap<i64, PendingCall>,
    pending_usage: Option<UsageEvent>,
    pending_stop: Option<String>,
    saw_done: bool,
}

impl OpenAIParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one raw SSE line (e.g. `data: {"id":"chatcmpl-...",...}`).
    /// Returns Ok(true) when stream is done ([DONE]).
    pub fn process_line(
        &mut self,
        line: &str,
        emit: &mut dyn FnMut(Event) -> Result<()>,
    ) -> Result<bool> {
        let l = line.trim_end_matches(['\n', '\r']);

        if l == "RETRY:" {
            self.reset_state();
            emit(Event::Retry(RetryEvent {}))?;
            return Ok(false);
        }

        if l.is_empty() || !l.starts_with("data: ") {
            return Ok(false);
        }

        let payload = &l[6..];
        if payload == "[DONE]" {
            self.saw_done = true;
            self.prepare_terminal_events(emit)?;
            return Ok(true);
        }

        let body: Value = serde_json::from_str(payload)?;
        if let Some(msg) = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            emit(Event::Error(ErrorEvent {
                message: msg.into(),
            }))?;
            return Ok(false);
        }

        let usage = body.get("usage").cloned().unwrap_or(Value::Null);
        if let Some(v) = usage.get("completion_tokens").and_then(Value::as_i64) {
            self.output_tokens = v;
        }
        // cached_tokens may be at top level or nested under prompt_tokens_details (OpenAI/DeepSeek format)
        if let Some(v) = usage
            .get("cached_tokens")
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
            })
            .and_then(Value::as_i64)
        {
            self.cache_read_input_tokens = v;
        }
        if let Some(v) = usage.get("prompt_tokens").and_then(Value::as_i64) {
            self.input_tokens = if self.cache_read_input_tokens > 0 {
                v - self.cache_read_input_tokens
            } else {
                v
            };
        }

        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or(Value::Null);

        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str)
            && !r.is_empty()
            && r != "null"
        {
            self.stop_reason = r.into();
        }

        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            let c = if !self.saw_text {
                content.trim_start_matches(['\n', '\r']).to_string()
            } else {
                content.to_string()
            };
            if !c.is_empty() {
                self.saw_text = true;
                emit(Event::Text(TextEvent { content: c }))?;
            }
        }

        if let Some(reasoning) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            && !reasoning.is_empty()
        {
            emit(Event::Thinking(ThinkingEvent {
                content: reasoning.into(),
            }))?;
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let idx = tc.get("index").and_then(Value::as_i64).unwrap_or(0);
                let entry = self.pending_calls.entry(idx).or_default();
                if let Some(v) = tc.get("id").and_then(Value::as_str) {
                    entry.id = v.into();
                }
                if let Some(v) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    entry.name = v.into();
                }
                if let Some(v) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    entry.arguments.push_str(v);
                }
            }
        }

        if choice.get("finish_reason").and_then(Value::as_str) == Some("tool_calls") {
            self.emit_pending(emit)?;
        }

        Ok(false)
    }

    /// Flush pending usage + stop events.
    pub fn flush(&mut self, emit: &mut dyn FnMut(Event) -> Result<()>) -> Result<()> {
        if let Some(usage) = self.pending_usage.take() {
            emit(Event::Usage(usage))?;
        }
        if let Some(reason) = self.pending_stop.take() {
            emit(Event::Stop(StopEvent { reason }))?;
        }
        Ok(())
    }

    /// Finalize a stream that reached transport EOF. Some compatible
    /// providers close after a finish_reason frame without sending [DONE].
    pub fn finish_eof(&mut self, emit: &mut dyn FnMut(Event) -> Result<()>) -> Result<()> {
        if self.pending_stop.is_some() || self.saw_done {
            return Ok(());
        }
        if self.stop_reason.is_empty() {
            anyhow::bail!("stream ended before finish_reason");
        }
        self.prepare_terminal_events(emit)
    }

    fn prepare_terminal_events(&mut self, emit: &mut dyn FnMut(Event) -> Result<()>) -> Result<()> {
        self.emit_pending(emit)?;
        if self.stop_reason.is_empty() {
            self.stop_reason = "done".into();
        }
        self.pending_usage = Some(UsageEvent {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: 0,
        });
        self.pending_stop = Some(self.stop_reason.clone());
        Ok(())
    }

    fn emit_pending(&mut self, emit: &mut dyn FnMut(Event) -> Result<()>) -> Result<()> {
        for call in self.pending_calls.values_mut() {
            if call.arguments.is_empty() {
                continue;
            }
            let evt = match build_tool_call_event(&call.name, &call.id, &call.arguments) {
                Ok(evt) => evt,
                Err(original) => {
                    let repaired = crate::repair::repair_truncated_json(&call.arguments);
                    if repaired.changed && !repaired.fallback {
                        build_tool_call_event(&call.name, &call.id, &repaired.repaired)?
                    } else {
                        anyhow::bail!("parse tool call {} input: {}", call.name, original);
                    }
                }
            };
            emit(Event::ToolCall(evt))?;
            call.arguments.clear();
        }
        Ok(())
    }

    fn reset_state(&mut self) {
        self.stop_reason.clear();
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.cache_read_input_tokens = 0;
        self.saw_text = false;
        self.pending_calls.clear();
        self.pending_usage = None;
        self.pending_stop = None;
        self.saw_done = false;
    }
}

// ---- Legacy synchronous parse ----

pub fn parse<R: Read>(reader: R, mut emit: impl FnMut(Event) -> Result<()>) -> Result<()> {
    let mut parser = OpenAIParser::new();
    let mut br = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = br.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        if parser.process_line(&line, &mut emit)? {
            break;
        }
    }
    parser.finish_eof(&mut emit)?;
    parser.flush(&mut emit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_lines(parser: &mut OpenAIParser, lines: &[&str]) -> Vec<Event> {
        let mut events = Vec::new();
        for &l in lines {
            let full = format!("{l}\n");
            parser
                .process_line(&full, &mut |e| {
                    events.push(e);
                    Ok(())
                })
                .unwrap();
        }
        parser
            .flush(&mut |e| {
                events.push(e);
                Ok(())
            })
            .unwrap();
        events
    }

    fn collect_lines_with_eof(parser: &mut OpenAIParser, lines: &[&str]) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        for &l in lines {
            let full = format!("{l}\n");
            parser.process_line(&full, &mut |e| {
                events.push(e);
                Ok(())
            })?;
        }
        parser.finish_eof(&mut |e| {
            events.push(e);
            Ok(())
        })?;
        parser.flush(&mut |e| {
            events.push(e);
            Ok(())
        })?;
        Ok(events)
    }

    #[test]
    fn basic_text_with_done() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" World\"},\"finish_reason\":null}]}",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        let texts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello", " World"]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Stop(s) if s.reason == "stop"))
        );
        assert!(
            events.iter().any(
                |e| matches!(e, Event::Usage(u) if u.input_tokens == 10 && u.output_tokens == 5)
            )
        );
    }

    #[test]
    fn reasoning_content_becomes_thinking() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Let me think...\"}}]}",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Thinking(t) if t.content == "Let me think..."))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Text(t) if t.content == "OK"))
        );
    }

    #[test]
    fn tool_calls_merged_across_chunks() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/f.txt\\\"\"}}]}}]}",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        let calls: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCall(c) => Some(c.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(calls, vec!["Read"]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Stop(s) if s.reason == "tool_calls"))
        );
    }

    #[test]
    fn finish_reason_without_done_flushes_stop_on_eof() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1}}",
        ];
        let events = collect_lines_with_eof(&mut p, &lines).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Stop(s) if s.reason == "stop"))
        );
    }

    #[test]
    fn eof_without_finish_reason_is_error() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}",
        ];
        let err = collect_lines_with_eof(&mut p, &lines)
            .unwrap_err()
            .to_string();
        assert!(err.contains("stream ended before finish_reason"), "{err}");
    }

    #[test]
    fn truncated_tool_call_arguments_are_repaired_before_emit() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/f.txt\\\"\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ToolCall(c) if c.name == "Read" && c.fields["path"] == "/tmp/f.txt"))
        );
    }

    #[test]
    fn cached_tokens_from_prompt_tokens_details() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":1,\"prompt_tokens_details\":{\"cached_tokens\":80}}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        let usage = events
            .iter()
            .find_map(|e| match e {
                Event::Usage(u) => Some(u),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage.input_tokens, 20); // 100 - 80
        assert_eq!(usage.cache_read_input_tokens, 80);
    }

    #[test]
    fn leading_newlines_trimmed_on_first_text() {
        let mut p = OpenAIParser::new();
        let lines = [
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\\n\\nHello\"}}]}",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" World\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}",
            "data: [DONE]",
        ];
        let events = collect_lines(&mut p, &lines);
        let texts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts[0], "Hello");
        assert_eq!(texts[1], " World");
    }

    #[test]
    fn error_in_response() {
        let mut p = OpenAIParser::new();
        let lines = ["data: {\"error\":{\"message\":\"rate limit exceeded\"}}"];
        let events = collect_lines(&mut p, &lines);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Error(err) if err.message.contains("rate limit")))
        );
    }
}
