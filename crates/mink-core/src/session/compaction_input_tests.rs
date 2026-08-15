use super::*;
use serde_json::json;

#[test]
fn reduction_preserves_semantics_and_removes_thinking() {
    let messages = vec![
        json!({"role":"user","content":"fix the build without changing the API"}),
        json!({"role":"assistant","content":[
            {"type":"thinking","thinking":"private chain of thought"},
            {"type":"text","text":"I will inspect the failing module."},
            {"type":"tool_use","id":"read-1","name":"Read","input":{"path":"src/lib.rs:1-80"}},
            {"type":"tool_use","id":"test-1","name":"Bash","input":{"command":"cargo test"}}
        ]}),
        json!({"role":"user","content":[
            {"type":"tool_result","tool_use_id":"read-1","content":"fn main() {}"},
            {"type":"tool_result","tool_use_id":"test-1","content":"Process completed with exit code 1.\nartifact://bash-0001"}
        ]}),
    ];

    let reduced = reduce_for_summary(&messages);
    assert!(reduced.contains("fix the build without changing the API"));
    assert!(reduced.contains("I will inspect the failing module."));
    assert!(reduced.contains("path=src/lib.rs:1-80"));
    assert!(reduced.contains("command=cargo test"));
    assert!(reduced.contains("Process completed with exit code 1."));
    assert!(reduced.contains("artifact://bash-0001"));
    assert!(!reduced.contains("private chain of thought"));
}

#[test]
fn long_tool_results_keep_both_ends_with_unicode_boundaries() {
    let content = format!(
        "{}\nerror[E0308]: mismatched types\n{}\nartifact://bash-0009",
        "普通输出".repeat(600),
        "更多输出".repeat(600)
    );
    let reduced = reduce_for_summary(&[json!({
        "role":"user",
        "content":[{"type":"tool_result","tool_use_id":"tool-1","content":content}]
    })]);

    assert!(reduced.contains("tool content reduced"));
    assert!(reduced.contains("error[E0308]: mismatched types"));
    assert!(reduced.contains("artifact://bash-0009"));
    assert!(reduced.is_char_boundary(reduced.len()));
}
