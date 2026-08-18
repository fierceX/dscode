//! Flash reasoning-mode router for Mink.
//!
//! This crate is intentionally independent from `mink-core`'s agent loop. It
//! implements the pure routing strategy from `pi-deepseek-route` and exposes a
//! thin [`LlmBackend`] decorator so callers can enable routing by swapping the
//! LLM backend.
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use mink::runtime::LlmBackend;
//! use mink_router::{RouterConfig, RouterLlmBackend};
//!
//! # fn main() {}
//! ```

pub mod backend;
pub mod config;
pub mod core;
pub mod prefab;

pub use backend::RouterLlmBackend;
pub use config::RouterConfig;
pub use core::{
    MODE_MIXED, MODE_REACT, MODE_SPEC, MODE_WEAK, band_for, band_of, classify_task, core_for,
    guide_for, is_chat_task, is_complex_task, is_flash_model, message_text, parse_mode,
    persona_for,
};
pub use prefab::{
    count_real_user_rounds, extract_real_user_messages, filter_core_tools, first_real_user_index,
    first_real_user_message, has_flash_persona, has_tool_use, has_tool_use_after_real_user,
    is_prefab_warmup_message, last_is_real_user,
};
