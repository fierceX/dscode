//! Session bridge: `read_history` / `read_conversation` 分页读取
//! （events.jsonl 历史与 conversation.jsonl 轮次历史）。
//! Tail-SSE 实现已随架构迁移移除（生产 SSE 走 runtime 广播）。

use anyhow::Result;
use std::collections::VecDeque;
use std::io::BufRead;

pub async fn read_history(
    path: &std::path::Path,
    from_seq: u64,
    limit: usize,
    tail: bool,
    before_seq: Option<u64>,
) -> Result<Vec<serde_json::Value>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_jsonl_page(&path, from_seq, limit, tail, before_seq))
        .await?
}

fn read_jsonl_page(
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
    anyhow::ensure!(
        (1..=2000).contains(&limit),
        "history limit must be in 1..=2000"
    );
    let retain_tail = tail || before_seq.is_some();
    let mut events: VecDeque<(u64, serde_json::Value)> = VecDeque::with_capacity(limit);
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
            if retain_tail && events.len() == limit {
                events.pop_front();
            }
            events.push_back((line_no, v));
            if !retain_tail && events.len() == limit {
                break;
            }
        }
    }
    // 分页语义：tail 与 before_seq 都取“最接近目标”的 limit 条（保留末尾）；
    // 仅 from_seq 前向读取时保留开头。
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
