use mink::tools::{hashline, replace};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Corpus {
    upstream: Upstream,
    hashes: Vec<HashCase>,
    hashline: Vec<HashlineCase>,
    replace: Vec<ReplaceCase>,
}

#[derive(Debug, Deserialize)]
struct HashCase {
    id: String,
    content: String,
    expected: String,
}

#[derive(Debug, Deserialize)]
struct Upstream {
    commit: String,
}

#[derive(Debug, Deserialize)]
struct HashlineCase {
    id: String,
    content: String,
    input: String,
    error_contains: Option<String>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct ReplaceCase {
    id: String,
    content: String,
    old_text: String,
    new_text: String,
    all: bool,
    fuzzy: bool,
    threshold: f64,
    error_contains: Option<String>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    ok: bool,
    content: Option<String>,
    count: Option<usize>,
    warnings: Option<Vec<String>>,
    file_op: Option<serde_json::Value>,
}

fn corpus() -> Corpus {
    serde_json::from_str(include_str!("fixtures/edit_alignment.json"))
        .expect("edit alignment fixture must be valid JSON")
}

#[test]
fn fixture_is_pinned_to_an_upstream_commit() {
    let corpus = corpus();
    assert_eq!(corpus.upstream.commit.len(), 40);
    assert!(
        corpus
            .upstream
            .commit
            .chars()
            .all(|ch| ch.is_ascii_hexdigit())
    );
    assert!(corpus.hashline.len() >= 37);
    assert!(corpus.replace.len() >= 15);
    assert!(corpus.hashes.len() >= 5);
}

#[test]
fn snapshot_hash_matches_upstream_goldens() {
    for case in corpus().hashes {
        assert_eq!(
            mink::tools::snapshot::compute_file_tag(&case.content),
            case.expected,
            "{}",
            case.id
        );
    }
}

#[test]
fn hashline_matches_upstream_goldens() {
    for case in corpus().hashline {
        let actual = hashline::parse(&case.input).and_then(|patch| {
            let section = patch
                .sections
                .first()
                .ok_or_else(|| anyhow::anyhow!("missing parsed section"))?;
            let mut clipboard = hashline::Clipboard::default();
            hashline::apply(&case.content, section, &mut clipboard)
        });
        if case.expected.ok {
            let actual = actual.unwrap_or_else(|error| panic!("{}: {error:#}", case.id));
            assert_eq!(Some(actual.text), case.expected.content, "{}", case.id);
            assert_eq!(
                (!actual.warnings.is_empty()).then_some(actual.warnings),
                case.expected.warnings,
                "{} warnings",
                case.id
            );
            let file_op = hashline::parse(&case.input)
                .unwrap()
                .sections
                .first()
                .and_then(|section| section.operations.last())
                .and_then(|operation| match operation {
                    hashline::Operation::Move { destination } => {
                        Some(serde_json::json!({ "kind": "move", "dest": destination }))
                    }
                    hashline::Operation::Remove => Some(serde_json::json!({ "kind": "rem" })),
                    _ => None,
                });
            assert_eq!(file_op, case.expected.file_op, "{} file op", case.id);
        } else {
            let error = actual
                .expect_err(&format!("{}: expected an error", case.id))
                .to_string();
            if let Some(part) = case.error_contains {
                assert!(
                    error.to_lowercase().contains(&part.to_lowercase()),
                    "{}: expected error containing {part:?}, got {error:?}",
                    case.id
                );
            }
        }
    }
}

#[test]
fn replace_matches_upstream_goldens() {
    for case in corpus().replace {
        let actual = replace::replace_text(
            &case.content.replace("\r\n", "\n").replace('\r', "\n"),
            &case.old_text.replace("\r\n", "\n").replace('\r', "\n"),
            &case.new_text.replace("\r\n", "\n").replace('\r', "\n"),
            case.all,
            case.fuzzy,
            case.threshold,
            &case.id,
        );
        if case.expected.ok {
            let actual = actual.unwrap_or_else(|error| panic!("{}: {error:#}", case.id));
            assert_eq!(Some(actual.content), case.expected.content, "{}", case.id);
            assert_eq!(Some(actual.count), case.expected.count, "{}", case.id);
        } else {
            let error = actual
                .expect_err(&format!("{}: expected an error", case.id))
                .to_string();
            if let Some(part) = case.error_contains {
                assert!(
                    error.to_lowercase().contains(&part.to_lowercase()),
                    "{}: expected error containing {part:?}, got {error:?}",
                    case.id
                );
            }
        }
    }
}
