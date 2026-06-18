use crate::tui::state::WorkState;
use ratatui::style::{Color, Modifier, Style};

pub(crate) fn text() -> Style {
    Style::default()
}

pub(crate) fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub(crate) fn border() -> Style {
    muted()
}

pub(crate) fn primary() -> Style {
    Style::default().fg(Color::Blue)
}

pub(crate) fn primary_bold() -> Style {
    primary().add_modifier(Modifier::BOLD)
}

pub(crate) fn secondary() -> Style {
    Style::default().fg(Color::Cyan)
}

pub(crate) fn info() -> Style {
    Style::default().fg(Color::Yellow)
}

pub(crate) fn inline_code() -> Style {
    info()
}

pub(crate) fn error() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

pub(crate) fn diff_remove() -> Style {
    Style::default().fg(Color::Red)
}

pub(crate) fn success() -> Style {
    Style::default().fg(Color::Green)
}

pub(crate) fn sub_agent() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn link(base: Style) -> Style {
    base.fg(Color::Blue).add_modifier(Modifier::UNDERLINED)
}

pub(crate) fn table_header(base: Style) -> Style {
    base.fg(Color::Blue).add_modifier(Modifier::BOLD)
}

pub(crate) fn diff_header() -> Style {
    info().add_modifier(Modifier::BOLD)
}

pub(crate) fn work_state(state: WorkState) -> Style {
    match state {
        WorkState::Idle => muted(),
        WorkState::WaitingModel => info(),
        WorkState::StreamingThinking => muted(),
        WorkState::StreamingText => primary_bold(),
        WorkState::RunningTool => info().add_modifier(Modifier::BOLD),
        WorkState::RunningSubAgent => sub_agent(),
        WorkState::Compacting => secondary(),
        WorkState::Error => error(),
    }
}
