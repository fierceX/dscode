//!
//! 运行时相对模型的唯一信息优势在于它持有完整的结构化轨迹。本模块把滑动窗口
//! 内的工具调用压缩成"模型无法从已有上下文推导的统计事实"（重复调用、失败聚
//! 类、预算消耗），供证据注入与状态操作的编辑路径定位使用。
//! 证据必须逐条可回溯到 conversation.jsonl 的工具调用记录。

use std::collections::{BTreeMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

/// 一次工具调用的精简轨迹记录。
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub tool: String,
    /// 规范参数串（tool_args 为 BTreeMap，键序天然稳定）。
    pub canonical_args: String,
    /// 该调用编辑过的路径（Edit/Write 的 path 参数），供回滚定位。
    pub paths: Vec<String>,
}

/// 渲染后的证据批（文本 + 新鲜度哈希）。
pub struct EvidenceBatch {
    pub text: String,
    pub hash: u64,
}

pub struct EvidenceTracker {
    records: VecDeque<CallRecord>,
    max_records: usize,
    /// 按 (tool, canonical_args) 聚合的最近失败摘要（渲染用）。
    failures: BTreeMap<(String, String), (u32, String)>,
    /// 本输入内累计硬/软失败计数。
    pub hard_failures: u32,
    pub soft_failures: u32,
    /// 本输入内编辑过的路径（去重、保序）。
    pub edited_paths: Vec<String>,
    /// 证据新鲜度去重：最近注入过的哈希。
    recent_injections: VecDeque<u64>,
    dedup_window: usize,
}

impl EvidenceTracker {
    pub fn new(max_records: usize, dedup_window: usize) -> Self {
        Self {
            records: VecDeque::with_capacity(max_records),
            max_records: max_records.max(1),
            failures: BTreeMap::new(),
            hard_failures: 0,
            soft_failures: 0,
            edited_paths: Vec::new(),
            recent_injections: VecDeque::with_capacity(dedup_window.max(1)),
            dedup_window: dedup_window.max(1),
        }
    }

    /// 记录一次工具调用的结果。`summary` 为空且 `failed` 为 false 表示成功。
    /// `failed` 是调用方对失败状态的权威判定（success=false / 有信号 / 有错误码），
    /// 用于软失败计数；成功调用不得推高 soft_failures。
    pub fn record(
        &mut self,
        tool: &str,
        args: &BTreeMap<String, String>,
        summary: &str,
        hard: bool,
        failed: bool,
        paths: Vec<String>,
    ) {
        if self.records.len() >= self.max_records {
            self.records.pop_front();
        }
        let canonical_args = serde_json::to_string(args).unwrap_or_default();
        self.records.push_back(CallRecord {
            tool: tool.to_string(),
            canonical_args: canonical_args.clone(),
            paths: paths.clone(),
        });
        for path in paths {
            if !self.edited_paths.iter().any(|p| p == &path) {
                self.edited_paths.push(path);
            }
        }
        if !summary.is_empty() {
            let key = (tool.to_string(), canonical_args);
            let entry = self.failures.entry(key).or_insert((0, String::new()));
            entry.0 += 1;
            entry.1 = summary.to_string();
        }
        if hard {
            self.hard_failures += 1;
        } else if failed {
            self.soft_failures += 1;
        }
    }

    /// 统计连续重复的相同调用（按 tool + canonical args）。
    fn consecutive_repeats(&self) -> Vec<(String, String, usize)> {
        let mut out: Vec<(String, String, usize)> = Vec::new();
        let mut prev: Option<(&str, &str)> = None;
        let mut run = 0usize;
        for record in &self.records {
            match prev {
                Some((t, a)) if t == record.tool && a == record.canonical_args => run += 1,
                _ => {
                    if run >= 2
                        && let Some((t, a)) = prev
                    {
                        out.push((t.to_string(), a.to_string(), run));
                    }
                    run = 1;
                }
            }
            prev = Some((&record.tool, &record.canonical_args));
        }
        if run >= 2
            && let Some((t, a)) = prev
        {
            out.push((t.to_string(), a.to_string(), run));
        }
        out
    }

    /// 渲染证据文本（纯事实，无祈使句），受 `budget_chars` 上限约束。
    pub fn render(&self, budget_chars: usize, belief: f64) -> EvidenceBatch {
        let mut lines: Vec<String> = Vec::new();
        lines.push("[trajectory]".to_string());

        for (tool, args, count) in self.consecutive_repeats() {
            let arg_short = truncate_chars(&args, 80);
            lines.push(format!(
                "- {tool}({arg_short}) repeated {count} consecutive times"
            ));
        }

        // 失败聚类：按调用签名聚合，保留最近摘要。
        type FailureEntry<'a> = (&'a (String, String), &'a (u32, String));
        let mut failures: Vec<FailureEntry<'_>> = self.failures.iter().collect();
        failures.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
        for ((tool, args), (count, summary)) in failures.iter().take(4) {
            let arg_short = truncate_chars(args, 60);
            let summary_short = truncate_chars(summary, 120);
            lines.push(format!(
                "- {tool}({arg_short}) failed {count} time(s): {summary_short}"
            ));
        }

        if self.hard_failures + self.soft_failures == 0 && lines.len() == 1 {
            lines.push("- no failures recorded".to_string());
        }

        lines.push(format!(
            "[detector] hard failures {}, soft {}; belief {:.2} (reference only)",
            self.hard_failures, self.soft_failures, belief
        ));

        let mut text = lines.join(
            "
",
        );
        if text.chars().count() > budget_chars {
            // 按优先级截断：detector 行保留，事实行从头截。
            let detector = lines.pop().unwrap_or_default();
            let mut kept: Vec<String> = Vec::new();
            let mut used = detector.chars().count();
            for line in lines {
                let cost = line.chars().count() + 1;
                if used + cost > budget_chars {
                    break;
                }
                kept.push(line);
                used += cost;
            }
            text = format!(
                "{}
{detector}",
                kept.join(
                    "
"
                )
            );
        }
        let hash = hash_text(&text);
        EvidenceBatch { text, hash }
    }

    /// 证据新鲜度去重：同哈希在最近 dedup_window 次注入中出现过则跳过。
    pub fn is_fresh(&self, hash: u64) -> bool {
        !self.recent_injections.contains(&hash)
    }

    pub fn mark_injected(&mut self, hash: u64) {
        if self.recent_injections.len() >= self.dedup_window {
            self.recent_injections.pop_front();
        }
        self.recent_injections.push_back(hash);
    }

    /// 新用户输入时清空（跨输入由 BeliefTracker::decay 负责）。
    /// recent_injections 一并清空：去重窗口是"最近 K 步"，步计数按输入重置，
    /// 跨输入保留哈希会让新输入里的同类证据被错误抑制。
    pub fn reset(&mut self) {
        self.records.clear();
        self.failures.clear();
        self.hard_failures = 0;
        self.soft_failures = 0;
        self.edited_paths.clear();
        self.recent_injections.clear();
    }

    /// 最近 `steps` 条记录内被编辑过的路径（去重、保序）。
    /// 合法编辑保持不动。
    pub fn edited_paths_since(&self, steps: usize) -> Vec<String> {
        let skip = self.records.len().saturating_sub(steps);
        let mut paths: Vec<String> = Vec::new();
        for record in self.records.iter().skip(skip) {
            for path in &record.paths {
                if !paths.iter().any(|p| p == path) {
                    paths.push(path.clone());
                }
            }
        }
        paths
    }
}

impl Default for EvidenceTracker {
    fn default() -> Self {
        Self::new(24, 6)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push_str("...");
    }
    out
}

fn hash_text(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
