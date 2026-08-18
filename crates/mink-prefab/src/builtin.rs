//! Bundled prefab templates.
//!
//! - `anchored-standard`: Prefab Anchored Standard, aligned with
//!   `dsh-anchored-standard/prefab`.
//! - `router-flash-weak`: Flash weak internal-routing trajectory, aligned with
//!   `dsh-routing-suite` WEAK_FLASH.

use crate::template::{PrefabTemplate, TemplateMeta, parse_conversation_str, validate};
use anyhow::{Result, bail};

const DEFAULT_META_JSON: &str = include_str!("../templates/anchored-standard/meta.json");
const DEFAULT_CONVERSATION_JSONL: &str =
    include_str!("../templates/anchored-standard/conversation.jsonl");

const ROUTER_FLASH_WEAK_META_JSON: &str = include_str!("../templates/router-flash-weak/meta.json");
const ROUTER_FLASH_WEAK_CONVERSATION_JSONL: &str =
    include_str!("../templates/router-flash-weak/conversation.jsonl");

/// Build the bundled generic prefab template.
pub fn default_template() -> Result<PrefabTemplate> {
    named_template("default")
}

/// Build a bundled template by name.
///
/// Supported names: `default`, `pro`, `flash`, `router-flash-weak`.
pub fn named_template(name: &str) -> Result<PrefabTemplate> {
    let (meta_json, conversation_jsonl) = match name {
        "default" | "anchored-standard" | "pro" => (DEFAULT_META_JSON, DEFAULT_CONVERSATION_JSONL),
        "router-flash-weak" | "flash" => (
            ROUTER_FLASH_WEAK_META_JSON,
            ROUTER_FLASH_WEAK_CONVERSATION_JSONL,
        ),
        other => bail!("unknown bundled prefab template '{other}'; expected 'pro' or 'flash'"),
    };
    let meta: TemplateMeta = serde_json::from_str(meta_json)?;
    let conversation = parse_conversation_str(conversation_jsonl)?;
    validate(&meta, &conversation)?;
    Ok(PrefabTemplate { meta, conversation })
}
