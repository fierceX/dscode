use super::embedded_skills;

#[test]
fn embedded_skills_are_discoverable() {
    let all = embedded_skills::all();
    assert!(!all.is_empty(), "should have at least one embedded skill");
    for skill in &all {
        assert!(!skill.name.is_empty(), "skill name must not be empty");
        assert!(
            !skill.description.is_empty(),
            "skill description must not be empty"
        );
        assert!(!skill.content.is_empty(), "skill content must not be empty");
    }
}

#[test]
fn embedded_debugging_skill_has_phases() {
    let skill = embedded_skills::find("debugging").expect("debugging skill should be embedded");
    assert!(
        skill.content.contains("Phase 1"),
        "debugging skill should have Phase 1"
    );
    assert!(
        skill.content.contains("Iron Law"),
        "debugging skill should have Iron Law"
    );
}

#[test]
fn embedded_verification_skill_has_gate() {
    let skill =
        embedded_skills::find("verification").expect("verification skill should be embedded");
    assert!(
        skill.content.contains("IDENTIFY"),
        "verification skill should have IDENTIFY step"
    );
}

#[test]
fn embedded_skill_find_returns_none_for_unknown() {
    assert!(embedded_skills::find("nonexistent-skill").is_none());
}

#[test]
fn embedded_tdd_skill_has_cycle() {
    let skill = embedded_skills::find("tdd").expect("tdd skill should be embedded");
    assert!(
        skill.content.contains("RED"),
        "tdd skill should mention RED phase"
    );
    assert!(
        skill.content.contains("GREEN"),
        "tdd skill should mention GREEN phase"
    );
}

#[test]
fn embedded_pre_code_check_has_checklist() {
    let skill =
        embedded_skills::find("pre-code-check").expect("pre-code-check skill should be embedded");
    assert!(
        skill.description.contains("blind edits"),
        "description should match"
    );
    assert!(
        skill.content.contains("The Checklist"),
        "content should include the pre-code checklist"
    );
    assert!(
        skill.content.contains("active content-search provider"),
        "content should require search through an active provider"
    );
}
