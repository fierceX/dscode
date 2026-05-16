use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTier {
    Conservative,
    Aggressive,
    ForceSummary,
    Emergency,
}

impl CompactionTier {
    pub fn from_ratio(current_tokens: usize, max_tokens: usize) -> Self {
        if max_tokens == 0 {
            return CompactionTier::Conservative;
        }
        let ratio = (current_tokens * 100) / max_tokens;
        if ratio >= 95 {
            CompactionTier::Emergency
        } else if ratio >= 80 {
            CompactionTier::ForceSummary
        } else if ratio >= 70 {
            CompactionTier::Aggressive
        } else {
            CompactionTier::Conservative
        }
    }

    pub fn tail_budget_ratio(&self) -> f64 {
        match self {
            CompactionTier::Conservative => 0.20,
            CompactionTier::Aggressive => 0.10,
            CompactionTier::ForceSummary => 0.05,
            CompactionTier::Emergency => 0.05,
        }
    }
}

/// compact_turn_keep: compute turn-aligned lines to keep using MinKeepRatio.
pub fn compact_turn_keep(lines: &[Value], min_keep_ratio: f64) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }

    let mut is_user = Vec::with_capacity(lines.len());
    let mut total_turns = 0usize;
    for line in lines {
        let user = line.get("role").and_then(Value::as_str) == Some("user")
            && line.get("content").is_some_and(|c| c.is_string());
        is_user.push(user);
        if user {
            total_turns += 1;
        }
    }

    let target = {
        let t = (total_turns as f64 * min_keep_ratio + 0.5) as usize;
        t.max(1).min(total_turns)
    };

    let mut keep = 0usize;
    let mut found = 0usize;
    for i in (0..lines.len()).rev() {
        if found >= target {
            break;
        }
        keep += 1;
        if is_user[i] {
            found += 1;
        }
    }
    if keep == 0 {
        return None;
    }
    Some(keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_conv(n_users: usize, bytes_per_msg: usize) -> Vec<Value> {
        let mut v = Vec::new();
        let pad = "x".repeat(bytes_per_msg);
        for i in 0..n_users {
            v.push(json!({"role":"user","content":format!("step {} {}", i, pad)}));
            v.push(json!({"role":"assistant","content":[{"type":"text","text":format!("response {}", pad)}]}));
            v.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("t{}",i),"content":format!("result {}", pad)}]}));
        }
        v
    }

    #[test]
    fn compact_turn_keep_respects_ratio() {
        let conv = make_conv(5, 50);
        let keep = compact_turn_keep(&conv, 0.4);
        assert!(keep.is_some());
        let k = keep.unwrap();
        assert!(k < conv.len());
        assert!(k > 0);
    }

    #[test]
    fn compact_turn_keep_empty_returns_none() {
        assert_eq!(compact_turn_keep(&[], 0.12), None);
    }

    #[test]
    fn turn_keep_aligns_to_user_boundary() {
        let conv = make_conv(3, 50);
        let keep = compact_turn_keep(&conv, 0.5);
        assert!(keep.is_some());
        let k = keep.unwrap();
        let first_kept = &conv[conv.len() - k];
        assert_eq!(first_kept["role"], "user");
    }

    #[test]
    fn tier_from_ratio_thresholds() {
        assert_eq!(CompactionTier::from_ratio(45, 100), CompactionTier::Conservative);
        assert_eq!(CompactionTier::from_ratio(55, 100), CompactionTier::Conservative);
        assert_eq!(CompactionTier::from_ratio(75, 100), CompactionTier::Aggressive);
        assert_eq!(CompactionTier::from_ratio(85, 100), CompactionTier::ForceSummary);
        assert_eq!(CompactionTier::from_ratio(97, 100), CompactionTier::Emergency);
    }

    #[test]
    fn tail_budget_ratios() {
        assert_eq!(CompactionTier::Conservative.tail_budget_ratio(), 0.20);
        assert_eq!(CompactionTier::Emergency.tail_budget_ratio(), 0.05);
    }
}
