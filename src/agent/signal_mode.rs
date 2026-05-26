#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMode {
    Off,
    Full,
}

impl SignalMode {
    pub fn from_env() -> Self {
        match std::env::var("DSCODE_SIGNAL_MODE")
            .unwrap_or_else(|_| "full".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "false" | "0" | "none" | "disabled" => Self::Off,
            _ => Self::Full,
        }
    }

    pub fn enabled(self) -> bool {
        matches!(self, Self::Full)
    }
}
