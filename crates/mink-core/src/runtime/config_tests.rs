use super::*;

#[test]
fn from_config_defaults_to_project_scoped_sessions() {
    let runtime_config = AgentRuntimeConfig::from_config(
        Config::default(),
        PathBuf::from("/tmp/mink-home"),
        PathBuf::from("/tmp/project"),
    );

    assert_eq!(runtime_config.session_layout, SessionLayout::ProjectScoped);
}
