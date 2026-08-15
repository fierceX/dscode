//!
//! ScriptedBackend 捕获每一个 LlmRequest，断言任意请求的模型可见输入都能从
//! 会话耐久产物离线重建：
//! 1. system_prompt == events.jsonl 最后一个 prefix_snapshot 的 system_prompt；
//! 2. tools == 同一快照的 tools_json（即构建时 tool_surface.schemas() 的序列化）；
//! 3. messages == conversation.jsonl 行前缀 + 计划尾置投影（语义等值）。
//!
//! 场景覆盖：确认计划 + 工具循环 + 失败工具（可能触发轨迹证据注入，注入行
//! 同样落在 conversation.jsonl，重建不受影响）。

use anyhow::{Result, anyhow};
use mink::runtime::{
    AgentOptions, AgentRuntime, LlmBackend, LlmEvent, LlmRequest, LlmResponseStream, LlmStopEvent,
    LlmTextEvent, LlmToolCallEvent,
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct CapturedRequest {
    system_prompt: String,
    messages: Vec<Value>,
    tools: Vec<Value>,
}

struct ScriptedBackend {
    scripts: Mutex<VecDeque<Vec<LlmEvent>>>,
    captured: Mutex<Vec<CapturedRequest>>,
}

impl ScriptedBackend {
    fn new(scripts: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            captured: Mutex::new(Vec::new()),
        }
    }

    fn captured(&self) -> Vec<CapturedRequest> {
        let mut guard = self.captured.lock().unwrap();
        let mut out = Vec::new();
        out.append(&mut guard);
        out
    }
}

#[async_trait::async_trait]
impl LlmBackend for ScriptedBackend {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn stream(&self, request: LlmRequest) -> Result<LlmResponseStream> {
        let record = CapturedRequest {
            system_prompt: request.system_prompt.clone(),
            messages: request.messages.clone(),
            tools: request.tools.clone(),
        };
        self.captured.lock().unwrap().push(record);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("scripted backend ran out of scripts"))?;
        let items: Vec<Result<LlmEvent>> = events.into_iter().map(Ok).collect();
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::iter(items)),
            attempt_count: 1,
        })
    }
}

fn read_tool_call(id: &str, path: &str) -> LlmEvent {
    let input = serde_json::json!({"path": path});
    let fields: BTreeMap<String, String> = input
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    let order: Vec<String> = fields.keys().cloned().collect();
    LlmEvent::ToolCall(LlmToolCallEvent {
        name: "Read".to_string(),
        id: id.to_string(),
        input_json: input,
        fields,
        order,
    })
}

fn text_stop(text: &str, reason: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::Text(LlmTextEvent {
            content: text.to_string(),
        }),
        LlmEvent::Stop(LlmStopEvent {
            reason: reason.to_string(),
        }),
    ]
}

fn prefix_snapshots(events_path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(events_path)?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|evt| evt.get("type").and_then(Value::as_str) == Some("prefix_snapshot"))
        .collect())
}

fn conversation_rows(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).map_err(|e| anyhow!("bad conversation row: {e}"))
        })
        .collect()
}

fn temp_pair(name: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("mink-invariants-{}-{}", std::process::id(), name));
    let home = root.join("home");
    let cwd = root.join("cwd");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    (home, cwd)
}

#[tokio::test]
async fn requests_rebuild_from_durable_session_logs() -> Result<()> {
    let (home, cwd) = temp_pair("rebuild");
    std::fs::write(cwd.join("fixture.txt"), "alpha beta")?;

    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![
            read_tool_call("call_read", "fixture.txt"),
            LlmEvent::Stop(LlmStopEvent {
                reason: "tool_use".to_string(),
            }),
        ],
        text_stop("done", "end_turn"),
        text_stop("ok", "end_turn"),
        vec![
            read_tool_call("call_missing", "missing.txt"),
            LlmEvent::Stop(LlmStopEvent {
                reason: "tool_use".to_string(),
            }),
        ],
        text_stop("recovered", "end_turn"),
    ]));

    let options = AgentOptions::new(home.clone(), cwd.clone()).with_llm_backend(backend.clone());
    let runtime = AgentRuntime::start(options)
        .await
        .map_err(|e| anyhow!("start: {e}"))?;

    // 确认计划：写 runtime 读取的同一个 plan 文件。
    let plan_path = runtime.session_info().plan_path.clone();
    std::fs::write(&plan_path, "# Verified plan")?;

    for prompt in ["read fixture", "confirm", "read missing"] {
        runtime
            .run_turn(prompt)
            .await
            .map_err(|e| anyhow!("turn: {e}"))?;
    }

    let captured = backend.captured();
    assert_eq!(
        captured.len(),
        5,
        "three turns consume exactly five requests"
    );

    let snapshots = prefix_snapshots(&runtime.session_info().events_path)?;
    assert_eq!(snapshots.len(), 1, "one prefix build for the whole session");
    let snapshot = &snapshots[0];
    let snapshot_prompt = snapshot["system_prompt"]
        .as_str()
        .ok_or_else(|| anyhow!("snapshot missing system_prompt"))?;
    let snapshot_tools = snapshot["tools_json"]
        .as_array()
        .ok_or_else(|| anyhow!("snapshot missing tools_json"))?;

    let rows = conversation_rows(&runtime.session_info().conversation_path)?;

    // 场景自证：失败工具必须触发轨迹证据注入（默认 signal 配置），
    // 注入行同样落在 conversation.jsonl，被下面的前缀重建覆盖。
    let has_trajectory = rows.iter().any(|row| {
        row.get("content")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("[trajectory]"))
    });
    assert!(
        has_trajectory,
        "failed tool must trigger trajectory evidence injection when signal policy is enabled"
    );

    let nl = std::char::from_u32(10).unwrap().to_string();
    let plan_content = std::fs::read_to_string(&plan_path)?.trim().to_string();
    let plan_msg = serde_json::json!({
        "role": "system",
        "content": format!("<current-plan>{nl}{plan_content}{nl}</current-plan>")
    });

    // 每个捕获请求必须能从耐久产物重建。
    let mut previous_k = 0usize;
    for request in &captured {
        assert_eq!(request.system_prompt, snapshot_prompt);
        assert_eq!(request.tools.as_slice(), snapshot_tools.as_slice());
        assert!(!request.messages.is_empty());
        assert_eq!(
            request.messages.last().unwrap(),
            &plan_msg,
            "plan must be the last projected message"
        );
        let matched_k = (previous_k..=rows.len())
            .find(|k| {
                let mut expected = rows[..*k].to_vec();
                expected.push(plan_msg.clone());
                request.messages == expected
            })
            .ok_or_else(|| anyhow!("no durable row prefix rebuilds request messages"))?;
        previous_k = matched_k;
    }

    runtime
        .shutdown()
        .await
        .map_err(|e| anyhow!("shutdown: {e}"))?;
    let _ = std::fs::remove_dir_all(home.parent().unwrap());
    Ok(())
}
