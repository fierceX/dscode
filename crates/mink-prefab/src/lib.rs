//! Prefab Anchored Standard session seeder for mink.
//!
//! This crate is intentionally independent of `mink-core`. It knows how to
//! load a bundled or on-disk prefab template and write a Mink-compatible
//! session directory (`session.json` + `conversation.jsonl`) before the normal
//! agent loop starts.
//!
//! The runtime never calls back into this crate after seeding; the feature is
//! a pure startup-time injection.

#[cfg(feature = "mink-integration")]
pub mod adapter;
pub mod builtin;
pub mod seed;
pub mod template;

pub use builtin::named_template as load_named;
pub use seed::{PrefabSeed, PrefabSeedOptions, restructure_session, seed_session};
pub use template::{PrefabTemplate, TemplateMeta, load_builtin, load_path};
