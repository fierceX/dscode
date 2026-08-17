use super::*;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mink-cap-skills-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn write_skill(root: &Path, relative: &str, content: &str) {
    let dir = root.join(relative);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), content).unwrap();
}

#[test]
fn skill_snapshot_prefers_project_over_builtin() {
    let root = temp_root("override");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        ".claude/skills/debugging",
        "---\ndescription: \"Local debugging\"\n---\n\nlocal body",
    );

    let snapshot = build_default_skill_snapshot(&cwd, &home, &[]).unwrap();

    let loaded = snapshot.by_name.get("debugging").unwrap();
    assert_eq!(loaded.skill.description, "Local debugging");
    assert!(loaded.skill.content.contains("local body"));
    assert_eq!(loaded.source.level, SourceLevel::Project);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn skill_shadow_warning_is_reported() {
    let root = temp_root("shadow-warning");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        ".claude/skills/debugging",
        "---\ndescription: \"Local debugging\"\n---\n\nlocal body",
    );

    let snapshot = build_default_skill_snapshot(&cwd, &home, &[]).unwrap();

    let warning = snapshot
        .warnings
        .iter()
        .find(|warning| warning.message.contains("skill 'debugging'"))
        .expect("debugging shadow warning");
    assert_eq!(warning.provider_id, "built-in-skills");
    assert!(
        warning.message.contains("shadowed by .claude/skills"),
        "{}",
        warning.message
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn skill_snapshot_model_addressable_hidden_from_index() {
    let root = temp_root("hidden");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        "skills/hidden-review",
        "---\ndescription: \"Hidden review\"\nhide: true\n---\n\nbody",
    );

    let snapshot = build_default_skill_snapshot(&cwd, &home, &[]).unwrap();

    assert!(snapshot.by_name.contains_key("hidden-review"));
    assert!(
        !snapshot
            .discoverable
            .iter()
            .any(|skill| skill.skill.name == "hidden-review")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn selected_model_addressable_skill_enters_prompt() {
    let root = temp_root("selected-hidden");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        "skills/hidden-review",
        "---\ndescription: \"Hidden review\"\ndisable-model-invocation: true\n---\n\nbody",
    );

    let snapshot =
        build_default_skill_snapshot(&cwd, &home, &["hidden-review".to_string()]).unwrap();

    assert_eq!(snapshot.selected.len(), 1);
    assert_eq!(snapshot.selected[0].info.name, "hidden-review");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn selected_skill_rejects_whitespace_padding() {
    let root = temp_root("selected-whitespace");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        "skills/hidden-review",
        "---\ndescription: \"Hidden review\"\n---\n\nbody",
    );

    let err = build_default_skill_snapshot(&cwd, &home, &[" hidden-review".to_string()])
        .unwrap_err()
        .to_string();

    assert!(err.contains("invalid selected skill name"), "{err}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn runtime_skill_rejects_whitespace_padding() {
    let provider = RuntimeSkillProvider::from_runtime_skills(vec![RuntimeSkill::new(
        " runtime-guide",
        "Runtime guide",
        "body",
    )]);
    let root = temp_root("runtime-whitespace");
    let home = root.join("home");
    let cwd = root.join("workspace");

    let err = provider
        .load_skills(&LoadContext {
            cwd: &cwd,
            home: &home,
        })
        .unwrap_err()
        .to_string();

    assert!(err.contains("invalid runtime skill name"), "{err}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn skill_snapshot_dependency_fingerprint_changes_on_content_change() {
    let root = temp_root("fingerprint");
    let home = root.join("home");
    let cwd = root.join("workspace");
    write_skill(
        &cwd,
        "skills/local-review",
        "---\ndescription: \"Local review\"\n---\n\nbody 1",
    );
    let first = build_default_skill_snapshot(&cwd, &home, &[]).unwrap();
    write_skill(
        &cwd,
        "skills/local-review",
        "---\ndescription: \"Local review\"\n---\n\nbody 2",
    );
    let second = build_default_skill_snapshot(&cwd, &home, &[]).unwrap();

    assert_ne!(first.dependency_fingerprint, second.dependency_fingerprint);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn extract_skill_summary_from_frontmatter() {
    let content =
        "---\nname: test-skill\ndescription: \"Test skill description\"\n---\n\nSkill content";
    let summary = extract_skill_summary(content);
    assert_eq!(summary, "Test skill description");
}

#[test]
fn extract_skill_summary_without_frontmatter_returns_fallback() {
    let content = "No frontmatter here.\n\nSome content";
    let summary = extract_skill_summary(content);
    assert!(!summary.is_empty());
    assert!(summary.contains("No frontmatter here."));
}

#[test]
fn extract_skill_summary_falls_back_to_first_non_empty_line() {
    let content = "\n\n---\nname: test\n---\n\nActual content";
    let summary = extract_skill_summary(content);
    assert!(!summary.is_empty());
}
