use super::*;

// ---- Existing scavenge tests ----

#[test]
fn scavenge_xml_style() {
    let text = r#"I'll update it <tool_call>{"name":"Edit","arguments":{"path":"src/a.rs","patch":"@src/a.rs#0A3B\nreplace 2:\n+new()"}}</tool_call> now"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Edit");
    assert_eq!(
        result[0]["arguments"]["patch"],
        "@src/a.rs#0A3B\nreplace 2:\n+new()"
    );
}

#[test]
fn scavenge_bracket_style() {
    let text = r#"Let me check [TOOL_CALL]{"name":"Read","arguments":{"path":"/tmp/x:10-20"}}[/TOOL_CALL]"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Read");
    assert_eq!(result[0]["arguments"]["path"], "/tmp/x:10-20");
}

#[test]
fn scavenge_bare_json() {
    let text = r#"Here: {"name":"Read","arguments":{"path":"artifact://bash-0001:1-20"}}"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Read");
    assert_eq!(result[0]["arguments"]["path"], "artifact://bash-0001:1-20");
}

#[test]
fn no_tool_call_returns_none() {
    let text = "No tool calls here.";
    assert!(scavenge_tool_calls(text).is_none());
}

#[test]
fn escape_hatches_fallback() {
    let text = r#"some text and then {"name": "Read", "arguments": {"path": "skill://debugging"}} trailing"#;
    let result = scavenge_tool_calls(text);
    assert!(result.is_some());
}

// ---- New: DSML ----

#[test]
fn scavenge_dsml_half_width() {
    let text = r#"<|DSML|invoke name="Read">
<|DSML|parameter name="path" string="true">/tmp/x.txt<|DSML|parameter>
</|DSML|invoke>"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Read");
    assert_eq!(result[0]["arguments"]["path"], "/tmp/x.txt");
}

#[test]
fn scavenge_dsml_full_width() {
    let text = r#"<|DSML|invoke name="Grep">
<|DSML|parameter name="pattern" string="true">foo<|DSML|parameter>
</|DSML|invoke>"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Grep");
}

#[test]
fn scavenge_dsml_with_json_param() {
    let text = r#"<|DSML|invoke name="Edit">
<|DSML|parameter name="path" string="true">/tmp/x<|DSML|parameter>
<|DSML|parameter name="patch" string="true">@/tmp/x#0A3B
replace 1:
+new<|DSML|parameter>
</|DSML|invoke>"#;
    let result = scavenge_tool_calls(text).unwrap();
    assert_eq!(result[0]["name"], "Edit");
    assert_eq!(result[0]["arguments"]["path"], "/tmp/x");
    assert!(
        result[0]["arguments"]["patch"]
            .as_str()
            .unwrap()
            .contains("replace 1")
    );
}

// ---- New: 3-shape coerce ----

#[test]
fn coerce_openai_style() {
    let v: Value = serde_json::from_str(
        r#"{"type":"function","function":{"name":"Read","arguments":"{\"path\":\"/x\"}"}}"#,
    )
    .unwrap();
    let result = coerce_to_tool_call(&v);
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r["name"], "Read");
    assert_eq!(r["arguments"]["path"], "/x");
}

#[test]
fn coerce_tool_name_style() {
    let v: Value =
        serde_json::from_str(r#"{"tool_name":"Read","tool_args":{"path":"session://current"}}"#)
            .unwrap();
    let result = coerce_to_tool_call(&v);
    assert!(result.is_some());
    let result = result.unwrap();
    assert_eq!(result["name"], "Read");
    assert_eq!(result["arguments"]["path"], "session://current");
}

#[test]
fn coerce_standard_style() {
    let v: Value =
        serde_json::from_str(r#"{"name":"Glob","arguments":{"pattern":"*.rs"}}"#).unwrap();
    let result = coerce_to_tool_call(&v);
    assert!(result.is_some());
    assert_eq!(result.unwrap()["name"], "Glob");
}

// ---- New: Truncation repair ----

#[test]
fn truncation_short_circuit_valid() {
    let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"}}"#);
    assert!(!r.changed);
    assert!(!r.fallback);
}

#[test]
fn truncation_closes_brace() {
    let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"}"#);
    assert!(r.changed);
    assert!(!r.fallback);
    assert!(r.repaired.ends_with('}'));
    let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
    assert_eq!(parsed["arguments"]["command"], "ls");
}

#[test]
fn truncation_closes_string() {
    let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls"#);
    assert!(r.changed);
    assert!(!r.fallback);
    let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
    assert_eq!(parsed["name"], "Bash");
}

#[test]
fn truncation_fills_dangling_key() {
    let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"#);
    assert!(r.changed);
    let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
    assert_eq!(parsed["name"], "Bash");
}

#[test]
fn truncation_trailing_comma() {
    let r = repair_truncated_json(r#"{"name":"Bash","arguments":{"command":"ls",}}"#);
    assert!(r.changed);
    assert!(!r.fallback);
    let parsed: Value = serde_json::from_str(&r.repaired).unwrap();
    assert_eq!(parsed["arguments"]["command"], "ls");
}

#[test]
fn truncation_empty_fallback() {
    let r = repair_truncated_json("garbage");
    assert!(r.fallback);
    assert_eq!(r.repaired, "{}");
}

#[test]
fn truncation_empty_input() {
    let r = repair_truncated_json("");
    assert_eq!(r.repaired, "{}");
}

// ---- New: Combined scavenge ----

#[test]
fn scavenge_combined_dedup() {
    let text1 = r#"<tool_call>{"name":"Read","arguments":{"path":"/x:1-4"}}</tool_call>"#;
    let text2 = r#"{"name":"Read","arguments":{"path":"/x:1-4"}}"#;
    let (calls, _) = scavenge_combined(Some(text1), Some(text2), 10);
    assert_eq!(calls.len(), 1);
}

#[test]
fn scavenge_combined_respects_max() {
    let text = r#"<tool_call>{"name":"Read","arguments":{"path":"/a"}}</tool_call>
<tool_call>{"name":"Read","arguments":{"path":"/b"}}</tool_call>
<tool_call>{"name":"Read","arguments":{"path":"/c"}}</tool_call>"#;
    let (calls, notes) = scavenge_combined(Some(text), None, 2);
    assert_eq!(calls.len(), 2);
    assert!(notes.iter().any(|n| n.contains("reached max")));
}
