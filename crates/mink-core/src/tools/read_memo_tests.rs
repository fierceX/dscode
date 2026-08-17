use super::*;

fn entry_times() -> (SystemTime, SystemTime) {
    let now = SystemTime::now();
    let earlier = now - std::time::Duration::from_secs(60);
    (now, earlier)
}

#[test]
fn repeat_full_read_hits() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    memo.record(Path::new("a.md"), 100, now, false, None, None, 0, 0);
    assert!(memo.hit(Path::new("a.md"), 100, now, false, 0, 0, None, None));
}

#[test]
fn sub_range_hits_full_and_range_entries() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    memo.record(Path::new("a.md"), 100, now, false, Some(1), Some(200), 0, 0);
    assert!(memo.hit(
        Path::new("a.md"),
        100,
        now,
        false,
        0,
        0,
        Some(50),
        Some(100)
    ));
    assert!(!memo.hit(
        Path::new("a.md"),
        100,
        now,
        false,
        0,
        0,
        Some(150),
        Some(250)
    ));
    // A range entry must NOT satisfy a full request.
    assert!(!memo.hit(Path::new("a.md"), 100, now, false, 0, 0, None, None));
    // An entry running to EOF covers "start..EOF".
    memo.record(Path::new("a.md"), 100, now, false, Some(10), None, 0, 0);
    assert!(memo.hit(Path::new("a.md"), 100, now, false, 0, 0, Some(10), None));
    assert!(!memo.hit(Path::new("a.md"), 100, now, false, 0, 0, Some(1), None));
}

#[test]
fn mtime_change_invalidates() {
    let mut memo = ReadMemo::new();
    let (now, earlier) = entry_times();
    memo.record(Path::new("a.md"), 100, earlier, false, None, None, 0, 0);
    assert!(!memo.hit(Path::new("a.md"), 100, now, false, 0, 0, None, None));
}

#[test]
fn epoch_change_invalidates() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    memo.record(Path::new("a.md"), 100, now, false, None, None, 0, 0);
    assert!(!memo.hit(Path::new("a.md"), 100, now, false, 1, 0, None, None));
}

#[test]
fn mutation_epoch_change_invalidates() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    memo.record(Path::new("a.md"), 100, now, false, None, None, 0, 0);
    assert!(!memo.hit(Path::new("a.md"), 100, now, false, 0, 1, None, None));
}

#[test]
fn lru_eviction_bounds_memory() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    for i in 0..MEMO_MAX_ENTRIES + 10 {
        let path = Path::new("/tmp/memo-lru").join(format!("f{i}.md"));
        memo.record(&path, 10, now, false, None, None, 0, 0);
    }
    assert!(memo.total <= MEMO_MAX_ENTRIES);
    // The oldest entry was evicted.
    assert!(!memo.hit(
        Path::new("/tmp/memo-lru/f0.md"),
        10,
        now,
        false,
        0,
        0,
        None,
        None
    ));
    // The newest still hits.
    let last = Path::new("/tmp/memo-lru").join(format!("f{}.md", MEMO_MAX_ENTRIES + 9));
    assert!(memo.hit(&last, 10, now, false, 0, 0, None, None));
}

#[test]
fn same_range_replacement_keeps_single_entry() {
    let mut memo = ReadMemo::new();
    let (now, _) = entry_times();
    memo.record(Path::new("a.md"), 100, now, false, Some(1), Some(50), 0, 0);
    memo.record(Path::new("a.md"), 100, now, false, Some(1), Some(50), 0, 0);
    assert_eq!(memo.total, 1);
}
