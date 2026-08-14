use once_cell::sync::Lazy;
use regex::Regex;

static RE_FIND_DELETE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(^|[;&|])\s*find\b.*\bdelete\b").expect("regex"));
static RE_FORK_BOMB: Lazy<Regex> = Lazy::new(|| Regex::new(r":\(\)\{:\|:&\};:").expect("regex"));
static RE_BLOCK_DEVICE_WRITE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(^|\s)(of=|>|1>|>>|1>>)\s*/dev/(sd[a-z][0-9]*|disk[0-9]+|rdisk[0-9]+|nvme[0-9]+n[0-9]+(p[0-9]+)?|vd[a-z][0-9]*|xvd[a-z][0-9]*|hd[a-z][0-9]*)(\s|$)")
        .expect("regex")
});

pub fn deny_bash_command_reason(command: &str) -> Option<&'static str> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Some("empty command");
    }

    let lower = trimmed.to_lowercase();
    for p in [
        "sudo ", "shutdown", "reboot", "halt", "poweroff", "mkfs", "fdisk",
    ] {
        if lower.starts_with(p) {
            return Some("dangerous command prefix");
        }
    }
    if lower.contains("rm -rf /") || lower.contains("rm -fr /") {
        return Some("destructive root delete pattern");
    }
    if RE_FIND_DELETE.is_match(trimmed) {
        return Some("blocked destructive find -delete pattern");
    }
    if RE_FORK_BOMB.is_match(trimmed) {
        return Some("fork bomb pattern");
    }
    if RE_BLOCK_DEVICE_WRITE.is_match(trimmed) {
        return Some("block device write pattern");
    }
    None
}

#[cfg(test)]
mod tests {
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
}
