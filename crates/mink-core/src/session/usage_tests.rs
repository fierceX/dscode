use super::*;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mink-usage-{name}-{}-{}.jsonl",
        std::process::id(),
        next_id("test")
    ))
}

#[test]
fn journal_filters_and_summarizes_turn_records() {
    let path = temp_path("summary");
    let journal = UsageJournal::new(path.clone());
    let turn = journal.begin_turn();
    let capture = journal.capture(
        journal.scope(UsageKind::Agent, "session-1"),
        "deepseek-v4-flash",
    );
    capture
        .reported(
            &UsageEvent {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: 40,
                cache_creation_input_tokens: 0,
            },
            2,
        )
        .unwrap();

    let records = journal.records_for(&turn).unwrap();
    let summary = UsageSummary::from_records(&records);
    assert_eq!(records.len(), 1);
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.attempt_count, 2);
    assert_eq!(summary.tokens.input_tokens, 100);
    // 费用统计已移除：已上报记录写 0（兼容既有 session 文件语义）。
    assert_eq!(records[0].cost_nano_cny, Some(0));
    assert_eq!(journal.summary(), summary);
    let reloaded = UsageJournal::new(path.clone());
    assert_eq!(reloaded.summary(), summary);
    let _ = std::fs::remove_file(path);
}

#[test]
fn reported_records_always_carry_zero_cost_for_compat() {
    let path = temp_path("zero-cost");
    let journal = UsageJournal::new(path.clone());
    journal.begin_turn();
    journal
        .capture(
            journal.scope(UsageKind::Agent, "session-1"),
            "private-model",
        )
        .reported(
            &UsageEvent {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            1,
        )
        .unwrap();

    let records = journal.all_records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].cost_nano_cny, Some(0));
    // 汇总不再包含费用字段。
    let summary = journal.summary();
    assert_eq!(summary.tokens.input_tokens, 10);
    assert_eq!(summary.tokens.output_tokens, 5);
    let _ = std::fs::remove_file(path);
}

#[test]
fn resilient_reader_skips_corrupt_lines_but_strict_reader_errors() {
    let path = temp_path("resilient");
    let record = UsageRecord {
        version: USAGE_RECORD_VERSION,
        billing_turn_id: "turn-1".into(),
        request_id: "req-1".into(),
        kind: UsageKind::Agent,
        origin_session_id: "session-1".into(),
        model: "deepseek-v4-flash".into(),
        attempt_count: 1,
        status: UsageStatus::Reported,
        tokens: Some(TokenUsage {
            input_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 0,
        }),
        cost_nano_cny: Some(0),
        reason: None,
        completed_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut data = String::from("this is not json\n");
    data.push_str(&serde_json::to_string(&record).unwrap());
    data.push('\n');
    data.push_str("{also not json\n");
    std::fs::write(&path, data).unwrap();

    assert!(read_records(&path).is_err());
    let records = read_records_resilient(&path).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id, "req-1");
    let _ = std::fs::remove_file(path);
}

#[test]
fn journal_records_for_and_all_records_skip_corrupt_lines() {
    let path = temp_path("journal-resilient");
    let record = UsageRecord {
        version: USAGE_RECORD_VERSION,
        billing_turn_id: "turn-abc".into(),
        request_id: "req-abc".into(),
        kind: UsageKind::Agent,
        origin_session_id: "session-1".into(),
        model: "deepseek-v4-flash".into(),
        attempt_count: 1,
        status: UsageStatus::Reported,
        tokens: Some(TokenUsage {
            input_tokens: 10,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 2,
        }),
        cost_nano_cny: Some(0),
        reason: None,
        completed_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut data = String::from("not-json\n");
    data.push_str(&serde_json::to_string(&record).unwrap());
    data.push('\n');
    data.push_str("{also not json\n");
    std::fs::write(&path, data).unwrap();

    let journal = UsageJournal::new(path.clone());
    let all = journal.all_records().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].request_id, "req-abc");

    let turn_records = journal.records_for("turn-abc").unwrap();
    assert_eq!(turn_records.len(), 1);
    assert_eq!(turn_records[0].request_id, "req-abc");
    let _ = std::fs::remove_file(path);
}

#[test]
fn unreported_record_does_not_fabricate_zero_tokens() {
    let path = temp_path("unreported");
    let journal = UsageJournal::new(path.clone());
    journal.begin_turn();
    journal
        .capture(
            journal.scope(UsageKind::Compaction, "session-1"),
            "deepseek-v4-flash",
        )
        .unreported(1, "provider_usage_missing")
        .unwrap();

    let records = journal.all_records().unwrap();
    assert_eq!(records[0].status, UsageStatus::Unreported);
    assert!(records[0].tokens.is_none());
    // 未上报 usage 不伪造费用，保持 None。
    assert!(records[0].cost_nano_cny.is_none());
    let _ = std::fs::remove_file(path);
}
