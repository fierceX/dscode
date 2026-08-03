//! Session bridge: `read_history` / `read_conversation` 分页读取
//! （events.jsonl 历史与 conversation.jsonl 轮次历史）。
//! Tail-SSE 实现已随架构迁移移除（生产 SSE 走 runtime 广播）。

use anyhow::Result;
use std::io::BufRead;

pub async fn read_history(
    path: &std::path::Path,
    from_seq: u64,
    limit: usize,
    tail: bool,
    before_seq: Option<u64>,
) -> Result<Vec<serde_json::Value>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let reader = std::io::BufReader::new(file);
    let mut events: Vec<(u64, serde_json::Value)> = Vec::new();
    let mut line_no = 0u64;
    for line in reader.lines() {
        line_no += 1;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(before) = before_seq {
            if line_no >= before {
                continue;
            }
        } else if !tail && line_no <= from_seq {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            events.push((line_no, v));
        }
    }
    // 分页语义：tail 与 before_seq 都取“最接近目标”的 limit 条（保留末尾）；
    // 仅 from_seq 前向读取时保留开头。
    if events.len() > limit {
        if tail || before_seq.is_some() {
            events.drain(0..events.len() - limit);
        } else {
            events.truncate(limit);
        }
    }
    // 注入真实行号（seq）——前端用于 SSE from_seq 与稳定 key
    Ok(events
        .into_iter()
        .map(|(line_no, mut v)| {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("seq".to_string(), serde_json::json!(line_no));
            }
            v
        })
        .collect())
}

/// Read conversation.jsonl（完整轮次视图：user/assistant/tool 消息）。
/// 与 read_history 相同的分页语义（tail / before_seq / from_seq），
/// 前端历史展示基于此文件（一轮一条，含完整工具调用）。
pub async fn read_conversation(
    path: &std::path::Path,
    from_seq: u64,
    limit: usize,
    tail: bool,
    before_seq: Option<u64>,
) -> Result<Vec<serde_json::Value>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let reader = std::io::BufReader::new(file);
    let mut events: Vec<(u64, serde_json::Value)> = Vec::new();
    let mut line_no = 0u64;
    for line in reader.lines() {
        line_no += 1;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(before) = before_seq {
            if line_no >= before {
                continue;
            }
        } else if !tail && line_no <= from_seq {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            events.push((line_no, v));
        }
    }
    if events.len() > limit {
        if tail || before_seq.is_some() {
            events.drain(0..events.len() - limit);
        } else {
            events.truncate(limit);
        }
    }
    Ok(events
        .into_iter()
        .map(|(line_no, mut v)| {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("seq".to_string(), serde_json::json!(line_no));
            }
            v
        })
        .collect())
}
