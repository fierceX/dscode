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
        events
            .iter()
            .any(|e| matches!(e, Event::Usage(u) if u.input_tokens == 10 && u.output_tokens == 5))
    );
}

#[test]
fn sse_data_without_space_after_colon_is_accepted() {
    let mut parser = OpenAIParser::new();
    let events = collect_lines(
        &mut parser,
        &[
            "data:{\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}",
            "data:[DONE]",
        ],
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t.content == "ok"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Usage(u) if u.input_tokens == 5 && u.output_tokens == 2))
    );
}

#[test]
fn cached_tokens_larger_than_prompt_tokens_does_not_underflow() {
    let mut parser = OpenAIParser::new();
    let events = collect_lines(
        &mut parser,
        &[
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":8}}}",
            "data: [DONE]",
        ],
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Usage(u) if u.input_tokens == 0 && u.output_tokens == 2)),
        "events: {events:?}"
    );
}

#[test]
fn missing_provider_usage_is_not_reported_as_zero_tokens() {
    let mut parser = OpenAIParser::new();
    let events = collect_lines(
        &mut parser,
        &[
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}",
            "data: [DONE]",
        ],
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::UsageUnavailable))
    );
    assert!(!events.iter().any(|event| matches!(event, Event::Usage(_))));
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
fn reasoning_content_strips_think_tags() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"<think>Plan step\\n</think>\\n\"}}]}",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"</think>\\n\\n\"}}]}",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}",
        "data: [DONE]",
    ];
    let events = collect_lines(&mut p, &lines);
    let thinking: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Thinking(t) => Some(t.content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, vec!["Plan step\n\n"]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t.content == "OK"))
    );
}

#[test]
fn text_content_preserves_literal_think_tags() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"literal </think>\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}",
        "data: [DONE]",
    ];
    let events = collect_lines(&mut p, &lines);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t.content == "literal </think>"))
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
fn legacy_function_call_becomes_tool_call() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"function_call\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/f.txt\\\"}\"}},\"finish_reason\":\"function_call\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}",
        "data: [DONE]",
    ];
    let events = collect_lines(&mut p, &lines);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolCall(c)
            if c.name == "Read" && c.fields.get("path").map(String::as_str) == Some("/tmp/f.txt")
    )));
}

#[test]
fn empty_tool_call_arguments_default_to_empty_object() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_empty\",\"type\":\"function\",\"function\":{\"name\":\"Glob\",\"arguments\":\"\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}",
        "data: [DONE]",
    ];
    let events = collect_lines(&mut p, &lines);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolCall(c) if c.name == "Glob" && c.input_json == serde_json::json!({})
    )));
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
    assert!(events.iter().any(
        |e| matches!(e, Event::ToolCall(c) if c.name == "Read" && c.fields["path"] == "/tmp/f.txt")
    ));
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
fn cache_creation_input_tokens_from_direct_field() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":2,\"cache_creation_input_tokens\":60,\"prompt_tokens_details\":{\"cached_tokens\":40}}}",
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
    assert_eq!(usage.input_tokens, 60); // 100 - 40
    assert_eq!(usage.cache_creation_input_tokens, 60);
    assert_eq!(usage.cache_read_input_tokens, 40);
}

#[test]
fn cache_creation_input_tokens_from_prompt_tokens_details() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":200,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":150,\"cache_creation\":48}}}",
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
    assert_eq!(usage.cache_creation_input_tokens, 48);
    assert_eq!(usage.cache_read_input_tokens, 150);
}

#[test]
fn cached_tokens_from_native_deepseek_spelling() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":1,\"prompt_cache_hit_tokens\":70,\"prompt_cache_miss_tokens\":30}}",
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
    assert_eq!(usage.cache_read_input_tokens, 70);
    assert_eq!(usage.input_tokens, 30); // 100 - 70（miss 隐含在减法中）
}

#[test]
fn cache_creation_input_tokens_not_reported_stays_zero() {
    let mut p = OpenAIParser::new();
    let lines = [
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"z\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":1}}",
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
    assert_eq!(usage.cache_creation_input_tokens, 0);
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
