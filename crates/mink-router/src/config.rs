//! Router configuration.

/// Configuration for [`crate::RouterLlmBackend`].
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Only route Flash-family models. Non-Flash requests pass through
    /// untouched.
    pub flash_only: bool,
    /// Skip Prefab warm-up messages when extracting real user messages and
    /// counting rounds.
    pub prefab_aware: bool,
    /// Narrow the first-turn tool surface to the classified mode's core tools.
    pub narrow_first_turn_tools: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            flash_only: true,
            prefab_aware: true,
            narrow_first_turn_tools: false,
        }
    }
}

impl RouterConfig {
    pub fn flash_only() -> Self {
        Self {
            flash_only: true,
            ..Self::default()
        }
    }

    pub fn with_prefab_aware(mut self, enabled: bool) -> Self {
        self.prefab_aware = enabled;
        self
    }

    pub fn with_narrow_first_turn_tools(mut self, enabled: bool) -> Self {
        self.narrow_first_turn_tools = enabled;
        self
    }
}
