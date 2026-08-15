use super::*;
use crate::session::paths::paths_for;

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mink-session-meta-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
async fn metadata_resolves_alias_and_id_prefix() {
    let home = temp_home("resolve");
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
    tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
    ensure_metadata(
        &paths,
        &cwd,
        SessionSeed {
            alias: Some("feature-x".into()),
            title: Some("Feature X work".into()),
            first_prompt: Some("implement feature x".into()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        resolve_session_reference(&home, &cwd, "feature-x")
            .await
            .unwrap(),
        Some("20260604-120000-abcd".into())
    );
    assert_eq!(
        resolve_session_reference(&home, &cwd, "20260604")
            .await
            .unwrap(),
        Some("20260604-120000-abcd".into())
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

#[tokio::test]
async fn metadata_resolves_sanitized_alias_reference() {
    let home = temp_home("resolve-sanitized");
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
    tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
    ensure_metadata(
        &paths,
        &cwd,
        SessionSeed {
            alias: Some("feature-x".into()),
            title: Some("Feature X work".into()),
            first_prompt: Some("implement feature x".into()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        resolve_session_reference(&home, &cwd, "feature x")
            .await
            .unwrap(),
        Some("20260604-120000-abcd".into())
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

#[tokio::test]
async fn malformed_metadata_is_reported_without_overwrite() {
    let home = temp_home("malformed");
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
    tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
    tokio::fs::write(&paths.metadata, "{not-json")
        .await
        .unwrap();
    tokio::fs::write(&paths.summary, "legacy summary\n")
        .await
        .unwrap();

    let error = list_project_sessions(&home, &cwd).await.unwrap_err();
    assert!(error.to_string().contains("corrupt session metadata"));
    assert_eq!(
        tokio::fs::read_to_string(&paths.metadata).await.unwrap(),
        "{not-json"
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

#[tokio::test]
async fn ensure_metadata_rejects_malformed_file_without_overwrite() {
    let home = temp_home("recover");
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let paths = paths_for(&home, &cwd, "20260604-120000-abcd");
    tokio::fs::create_dir_all(&paths.session_dir).await.unwrap();
    tokio::fs::write(&paths.metadata, "{not-json")
        .await
        .unwrap();

    let error = ensure_metadata(
        &paths,
        &cwd,
        SessionSeed {
            alias: Some("feature-x".into()),
            title: Some("Feature X work".into()),
            first_prompt: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("corrupt session metadata"));
    assert_eq!(
        tokio::fs::read_to_string(&paths.metadata).await.unwrap(),
        "{not-json"
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

#[tokio::test]
async fn legacy_directory_requires_parseable_matching_metadata() {
    let home = temp_home("legacy-filter");
    let cwd = home.join("workspace");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let legacy = home
        .join(".mink/projects")
        .join(crate::session::paths::legacy_project_key(&cwd));
    tokio::fs::create_dir_all(legacy.join("missing"))
        .await
        .unwrap();
    let malformed = legacy.join("malformed");
    tokio::fs::create_dir_all(&malformed).await.unwrap();
    tokio::fs::write(malformed.join("session.json"), "{bad")
        .await
        .unwrap();

    let error = list_project_sessions(&home, &cwd).await.unwrap_err();
    assert!(error.to_string().contains("corrupt session metadata"));
    assert!(
        resolve_session_reference(&home, &cwd, "missing")
            .await
            .is_err()
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

#[test]
fn title_from_prompt_uses_first_non_empty_line() {
    assert_eq!(
        title_from_prompt("\n  fix session naming\nmore").as_deref(),
        Some("fix session naming")
    );
}

#[test]
fn sanitize_alias_rejects_path_like_names() {
    assert_eq!(sanitize_alias("../x"), None);
    assert_eq!(sanitize_alias("feature x").as_deref(), Some("feature-x"));
}
