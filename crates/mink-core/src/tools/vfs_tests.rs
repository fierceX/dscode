use super::*;

#[test]
fn normalizes_virtual_paths() {
    assert_eq!(normalize_virtual_root("./docs//api").unwrap(), "docs/api");
    assert_eq!(normalize_virtual_root("docs/../src").unwrap(), "src");
    assert_eq!(normalize_virtual_root("/").unwrap(), "");
    assert!(normalize_virtual_root("../private").is_err());
}

#[test]
fn virtual_glob_formatter_preserves_protocol_output() {
    let request = VfsGlobRequest {
        pattern: "**/*.md".into(),
        path: ".".into(),
        max_files: 100,
    };
    let result = VfsGlobResult {
        paths: vec!["docs/guide.md".into()],
        scanned_files: 1,
        truncated: false,
        skipped_files: 0,
    };
    assert_eq!(format_virtual_glob(&result, &request), "docs/guide.md");
}

#[test]
fn virtual_grep_formatter_renders_context_and_paths() {
    let request = VfsGrepRequest {
        pattern: "needle".into(),
        path: "./docs".into(),
        file_glob: "*.md".into(),
        context: Some(1),
        max_files: 100,
        max_results: 100,
    };
    let result = VfsGrepResult {
        entries: vec![
            VfsGrepEntry::Line {
                path: "docs/guide.md".into(),
                line_number: 1,
                content: "intro".into(),
                matched: false,
            },
            VfsGrepEntry::Line {
                path: "docs/guide.md".into(),
                line_number: 2,
                content: "needle".into(),
                matched: true,
            },
        ],
        match_count: 1,
        scanned_files: 1,
        truncated_results: false,
        truncated_files: false,
        skipped_files: 0,
    };
    let output = format_virtual_grep(&result, &request);
    assert!(output.contains("docs/guide.md:2:> needle"), "{output}");
    assert!(output.contains("docs/guide.md:1:  intro"), "{output}");
    assert!(!output.contains("src/lib.rs"), "{output}");
}

#[test]
fn virtual_line_selection_matches_read_ranges() {
    assert_eq!(
        select_virtual_lines("one\ntwo\nthree\n", Some(2), Some(2), "a.txt").unwrap(),
        "two\nthree"
    );
    assert!(select_virtual_lines("one\n", Some(2), None, "a.txt").is_err());
}

#[test]
fn virtual_grep_request_rejects_zero_result_limit() {
    let request = VfsGrepRequest {
        pattern: "needle".into(),
        path: ".".into(),
        file_glob: String::new(),
        context: None,
        max_files: 10,
        max_results: 0,
    };
    let error = validate_virtual_grep_request(&request).unwrap_err();
    assert!(
        error.to_string().contains("max search results"),
        "unexpected error: {error}"
    );
}

#[test]
fn virtual_grep_formatter_handles_zero_result_limit() {
    let request = VfsGrepRequest {
        pattern: "needle".into(),
        path: ".".into(),
        file_glob: String::new(),
        context: None,
        max_files: 10,
        max_results: 0,
    };
    let result = VfsGrepResult {
        truncated_results: true,
        ..VfsGrepResult::default()
    };
    let output = format_virtual_grep(&result, &request);
    assert_eq!(output, "... truncated at 0 results");
}
