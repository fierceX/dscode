//! Shared utility functions used across the codebase.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Truncate a string to at most `n` bytes on a UTF-8 character boundary,
/// appending "..." if truncation occurred.
pub(crate) fn truncate_str(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Format a token count for display.
/// Examples: 0 → "0", 500 → "500", 1234 → "1.2K", 1234567 → "1.23M"
pub(crate) fn fmt_k(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n >= 1_000_000 {
        let m = n / 1_000_000;
        let rest = n % 1_000_000;
        format!("{}.{:02}M", m, rest / 10_000)
    } else {
        let k = n / 1000;
        let rem = n % 1000;
        format!("{}.{}K", k, rem / 100)
    }
}

/// Put spawned Unix children in their own process group so timeout/cancel can
/// clean up grandchildren that keep pipes or files open.
#[cfg(unix)]
pub(crate) fn configure_child_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub(crate) fn configure_child_process_group(_cmd: &mut Command) {}

pub(crate) fn terminate_child_process_tree(child: &mut Child) {
    terminate_child_process_tree_with_grace(child, Duration::from_millis(250));
}

pub(crate) fn terminate_child_process_tree_with_grace(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        let pgid = -(child.id() as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        let start = Instant::now();
        while start.elapsed() < grace {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
        let _ = child.wait();
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}
