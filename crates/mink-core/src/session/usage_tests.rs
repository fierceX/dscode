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
    assert_eq!(summary.cost.known_nano_cny, 140_800);
    assert_eq!(journal.summary(), summary);
    let reloaded = UsageJournal::new(path.clone());
    assert_eq!(reloaded.summary(), summary);
    let _ = std::fs::remove_file(path);
}

#[test]
fn unknown_models_are_reported_as_unpriced() {
    let path = temp_path("unpriced");
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

    let summary = journal.summary();
    assert_eq!(summary.cost.known_nano_cny, 0);
    assert_eq!(summary.cost.unpriced_requests, 1);
    assert!(journal.all_records().unwrap()[0].cost_nano_cny.is_none());
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
    assert!(records[0].cost_nano_cny.is_none());
    let _ = std::fs::remove_file(path);
}
