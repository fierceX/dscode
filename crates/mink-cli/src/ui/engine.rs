use super::{Display, StatsSnapshot};
use crate::util::fmt_k;
use std::io::{self, Write};
use std::sync::Mutex;

pub struct TerminalDisplay {
    interactive: bool,
    stream_json: bool,
    stdout: Mutex<io::Stdout>,
    stderr: Mutex<io::Stderr>,
    state: Mutex<DisplayState>,
}

#[derive(Default)]
struct DisplayState {
    last_char: String,
    prev_was_thinking: bool,
}

impl TerminalDisplay {
    pub fn new(interactive: bool, stream_json: bool) -> Self {
        Self {
            interactive,
            stream_json,
            stdout: Mutex::new(io::stdout()),
            stderr: Mutex::new(io::stderr()),
            state: Mutex::new(DisplayState::default()),
        }
    }

    fn lock_stdout(&self) -> std::sync::MutexGuard<'_, io::Stdout> {
        self.stdout.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_stderr(&self) -> std::sync::MutexGuard<'_, io::Stderr> {
        self.stderr.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DisplayState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write_out(&self, s: &str) {
        if self.stream_json {
            return;
        }
        let normalized = normalize_display_text(s, self.interactive);
        let mut stdout = self.lock_stdout();
        let _ = write!(stdout, "{normalized}");
        let _ = stdout.flush();
    }

    fn write_err(&self, s: &str) {
        let mut stderr = self.lock_stderr();
        let _ = write!(stderr, "{s}");
        let _ = stderr.flush();
    }

    fn update_last_char(&self, text: &str) {
        let mut state = self.lock_state();
        if text.ends_with('\n') {
            state.last_char = "\n".into();
        } else if let Some(c) = text.chars().last() {
            state.last_char = c.to_string();
        }
    }
}

impl Display for TerminalDisplay {
    fn render_thinking(&self, content: &str) {
        self.write_out(&format!("\x1b[90m{content}\x1b[0m"));
        self.update_last_char(content);
        self.lock_state().prev_was_thinking = true;
    }

    fn render_text(&self, content: &str) {
        {
            let state = self.lock_state();
            if state.prev_was_thinking && state.last_char != "\n" {
                drop(state);
                self.write_out("\n");
                self.lock_state().last_char = "\n".into();
            }
        }
        self.write_out(content);
        self.update_last_char(content);
        self.lock_state().prev_was_thinking = false;
    }

    fn render_tool_call(&self, call: &crate::ui::ToolCallDisplay<'_>) {
        {
            let state = self.lock_state();
            if state.last_char != "\n" {
                drop(state);
                self.write_out("\n");
                self.lock_state().last_char = "\n".into();
            }
        }
        self.write_out(&format!("\x1b[33m[tool] {}\x1b[0m\n", call.summary));
        self.lock_state().last_char = "\n".into();
        self.lock_state().prev_was_thinking = false;
    }

    fn render_tool_result(&self, result: &crate::ui::PresentedToolResultDisplay<'_>) {
        {
            let state = self.lock_state();
            if state.prev_was_thinking && state.last_char != "\n" {
                drop(state);
                self.write_out("\n");
                self.lock_state().last_char = "\n".into();
            }
        }
        self.lock_state().prev_was_thinking = false;
        if !result.base.content_preview.is_empty() {
            self.write_out(result.base.content_preview);
            self.update_last_char(result.base.content_preview);
        }
    }

    fn render_stop(&self, _reason: &str) {
        let state = self.lock_state();
        if state.last_char != "\n" {
            drop(state);
            self.write_out("\n");
            self.lock_state().last_char = "\n".into();
        }
    }

    fn render_error(&self, message: &str) {
        let state = self.lock_state();
        if state.last_char != "\n" {
            drop(state);
            self.write_err("\n");
        }
        self.write_err(&format!("\x1b[31mError: {message}\x1b[0m\n"));
    }

    fn render_retry(&self) {
        self.write_err("RETRY\n");
    }

    fn render_signal(&self, _signal_kind: &str, _severity: f64, _message: &str) {}

    fn render_info(&self, msg: &str) {
        self.write_err(&format!("\x1b[36m{msg}\x1b[0m\n"));
    }

    fn render_title_update(&self, model: &str, stats: &StatsSnapshot) {
        let total_in = stats.total_input_tokens + stats.total_cache_read_tokens;
        let belief_str = if stats.belief > 0.0 {
            format!(" B:{:.2}", stats.belief)
        } else {
            String::new()
        };
        let msg = format!(
            "\x1b]0;{}{belief_str} T:{} R:{} I:{}({}) O:{} C:{}({})\x07",
            model,
            StatsSnapshot::fmt_num(stats.current_turn_count),
            StatsSnapshot::fmt_num(stats.agent_request_count),
            fmt_k(total_in),
            stats.cache_pct(),
            fmt_k(stats.total_output_tokens),
            fmt_k(stats.current_context_tokens),
            stats.ctx_pct(),
        );
        self.write_err(&msg);
    }

    fn render_sub_agent_status(
        &self,
        session_id: &str,
        status: &str,
        in_tokens: u64,
        out_tokens: u64,
    ) {
        if status == "ok" || status == "launched" || status == "running" {
            self.write_err(&format!(
                "\x1b[35m[sub-agent {}] {} (in={}, out={})\x1b[0m\n",
                session_id, status, in_tokens, out_tokens
            ));
        } else {
            self.write_err(&format!(
                "\x1b[31m[sub-agent {}] failed\x1b[0m\n",
                session_id
            ));
        }
    }

    fn render_sub_agent_output(
        &self,
        session_id: &str,
        status: &str,
        thinking: &str,
        text: &str,
        in_tokens: u64,
        out_tokens: u64,
    ) {
        self.write_out(&format!(
            "[sub-agent {}] {} (in={}, out={})\n",
            session_id, status, in_tokens, out_tokens,
        ));
        if !thinking.is_empty() {
            self.write_out("── Thinking ──\n");
            self.write_out(thinking);
            if !thinking.ends_with('\n') {
                self.write_out("\n");
            }
        }
        if !text.is_empty() {
            self.write_out("── Text ──\n");
            self.write_out(text);
            if !text.ends_with('\n') {
                self.write_out("\n");
            }
        }
    }

    fn render_prompt(&self) {
        self.write_err("\x1b[32m> \x1b[0m");
    }

    fn render_clear_line(&self) {
        self.write_err("\r\x1b[2K");
    }
}

fn normalize_display_text(s: &str, interactive: bool) -> String {
    let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
    if interactive {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}
