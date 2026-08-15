use std::io::{self, Write};
use std::process::Command;
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskNotificationKind {
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskNotification {
    pub kind: TaskNotificationKind,
    pub title: String,
    pub body: String,
}

impl TaskNotification {
    pub(crate) fn new(kind: TaskNotificationKind, model: &str) -> Self {
        let title = match kind {
            TaskNotificationKind::Completed => "mink 任务完成",
            TaskNotificationKind::Failed => "mink 任务失败",
        };
        let body = match kind {
            TaskNotificationKind::Completed => format!("模型 {model} 已完成当前 TUI 任务。"),
            TaskNotificationKind::Failed => format!("模型 {model} 的当前 TUI 任务已失败。"),
        };
        Self {
            kind,
            title: title.into(),
            body,
        }
    }
}

pub(crate) fn send_task_notification(notification: &TaskNotification) {
    let _ = emit_terminal_notification(notification);
    let notification = notification.clone();
    thread::spawn(move || {
        let _ = send_platform_notification(&notification);
    });
}

fn emit_terminal_notification(notification: &TaskNotification) -> io::Result<()> {
    let title = terminal_osc_component(&notification.title);
    let body = terminal_osc_component(&notification.body);
    let mut out = io::stdout().lock();

    // iTerm2 notification.
    write!(out, "\x1b]9;{body}\x07")?;
    // WezTerm/rxvt-style desktop notification.
    write!(out, "\x1b]777;notify;{title};{body}\x07")?;
    // Terminal bell fallback. Many terminal apps can promote this to a notification.
    write!(out, "\x07")?;
    out.flush()
}

#[cfg(target_os = "macos")]
fn send_platform_notification(notification: &TaskNotification) -> std::io::Result<()> {
    if Command::new("terminal-notifier")
        .arg("-title")
        .arg(&notification.title)
        .arg("-message")
        .arg(&notification.body)
        .arg("-sound")
        .arg("Glass")
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }

    let script = format!(
        "display notification {} with title {} sound name \"Glass\"",
        apple_script_string(&notification.body),
        apple_script_string(&notification.title)
    );
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .status()
        .map(|_| ())
}

#[cfg(not(target_os = "macos"))]
fn send_platform_notification(notification: &TaskNotification) -> std::io::Result<()> {
    Command::new("notify-send")
        .arg(&notification.title)
        .arg(&notification.body)
        .status()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn terminal_osc_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\x1b' | '\x07' => ' ',
            ';' | '\n' | '\r' | '\t' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect()
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
