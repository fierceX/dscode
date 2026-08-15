use super::*;

#[test]
fn text_shape_round_trips_bom_and_crlf() {
    let original = "\u{feff}a\r\nb\r\n";
    let (shape, normalized) = decode_text_shape(original);
    assert_eq!(normalized, "a\nb\n");
    assert_eq!(restore_text_shape(&shape, &normalized), original);
}

#[test]
fn numbered_hashline_format_uses_bracket_header() {
    assert_eq!(
        format_hashline_read("src/a.rs", "A1B2", 4, "a\nb"),
        "[src/a.rs#A1B2]\n4:a\n5:b"
    );
}

#[test]
fn mismatch_context_marks_anchors_separates_runs_and_truncates_utf8_safely() {
    let mut lines = (1..=100)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>();
    lines[49] = "界".repeat(300);
    let content = lines.join("\n") + "\n";
    let anchors = (10..=100).step_by(10).collect::<BTreeSet<_>>();
    let rendered = format_mismatch_anchor_context(&content, &anchors);

    assert!(rendered.contains("* 10:line-10"));
    assert!(rendered.contains("\n  …\n"));
    assert!(rendered.contains("[line truncated]"));
    assert!(rendered.contains("Anchor context truncated to safety limits"));
    assert!(std::str::from_utf8(rendered.as_bytes()).is_ok());
    let displayed = rendered
        .lines()
        .filter(|line| line.starts_with("* ") || line.starts_with("  ") && line.contains(':'))
        .count();
    assert!(displayed <= MISMATCH_CONTEXT_MAX_LINES);
}

#[test]
fn replace_suffix_recovery_rejects_ambiguity() {
    let root = std::env::temp_dir().join(format!("mink-replace-suffix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("a/x.rs"), "a").unwrap();
    std::fs::write(root.join("b/x.rs"), "b").unwrap();
    assert!(
        resolve_replace_target(&root, "x.rs")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    let _ = std::fs::remove_dir_all(root);
}
