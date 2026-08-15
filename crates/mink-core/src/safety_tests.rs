use super::*;

#[test]
fn blocks_sudo() {
    assert!(deny_bash_command_reason("sudo rm /tmp/foo").is_some());
}

#[test]
fn blocks_rm_rf_root() {
    assert!(deny_bash_command_reason("rm -rf /").is_some());
    assert!(deny_bash_command_reason("rm -fr /").is_some());
}

#[test]
fn blocks_shutdown() {
    assert!(deny_bash_command_reason("shutdown now").is_some());
}

#[test]
fn blocks_find_delete() {
    assert!(deny_bash_command_reason("find . -name '*.tmp' -delete").is_some());
}

#[test]
fn allows_safe_commands() {
    assert!(deny_bash_command_reason("echo hello").is_none());
    assert!(deny_bash_command_reason("ls -la").is_none());
    assert!(deny_bash_command_reason("cat /tmp/file").is_none());
}

#[test]
fn allows_dev_null_redirection() {
    assert!(deny_bash_command_reason("echo harmless >/dev/null").is_none());
}

#[test]
fn blocks_empty_command() {
    assert!(deny_bash_command_reason("").is_some());
    assert!(deny_bash_command_reason("   ").is_some());
}
