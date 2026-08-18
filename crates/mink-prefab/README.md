# mink-prefab

Prefab Anchored Standard session seeder for Mink.

Template sources: [dsh-anchored-standard](https://github.com/xiaobright/dsh-anchored-standard) and [dsh-routing-suite](https://github.com/yjh051108/dsh-routing-suite).

## Purpose

The purpose of Prefab is to restructure a session so that a validated,
successful trajectory is present before the first real task. This lets
deepseek-v4-flash and deepseek-v4-pro enter the target reasoning and tool-use
pattern immediately, effectively replicating the post-training trajectory that
`dsh-anchored-standard` / `dsh-routing-suite` explore to improve both models.

> Note: Prefab is a temporary feature. It may be removed after DeepSeek updates
> the models.

This crate provides a session restructure helper: it writes Mink-compatible
session content (`conversation.jsonl`, `events.jsonl`) from a bundled trajectory
template after Mink has initialized the session. It does not create extra
Prefab files; the special prefix is stored as a standard `prefix_snapshot`
event in `events.jsonl`.

When a Prefab `prefix_snapshot` event is present in `events.jsonl`, a
prefab-enabled Mink runtime rebuilds the complete system prompt and tool schema
from that event instead of using the compiled-in prompt builder. A normal
(non-prefab) runtime ignores it.

## Usage

The `prefab` feature is optional. `mink-cli`'s default `full-cli` feature
includes it; for a minimal binary enable it explicitly:

```bash
cargo build -p mink-cli --features prefab
```

```bash
# 使用默认模板（pro / anchored-standard）
mink --prefab "your task"

# 使用 flash 模板或本地模板目录
mink --prefab=flash "your task"
mink --prefab=./my-template "your task"
```

From Rust:

```rust
use mink::prelude::{AgentOptions, AgentRuntime};

let options = AgentOptions::new(home, cwd)
    .with_prefab_spec("flash")? // 或 .with_prefab_named("pro")? / .with_prefab_path("./my-template")?
    .with_tool_options(tool_options);
let runtime = AgentRuntime::start(options).await?;
```

## Templates

Bundled templates:

- `pro` (alias `default` / `anchored-standard`): Prefab Anchored Standard
  trajectory aligned with `dsh-anchored-standard/prefab/template.jsonl`:
  1. Read the workspace-root AGENTS.md via Bash.
  2. Reply `Instructions loaded.`
  3. Load the full instruction set via `load_full_instructions` (rendered as a virtual tool result).
  4. Reply `Ready.`
- `flash` (alias `router-flash-weak`): Flash weak internal-routing trajectory
  aligned with `dsh-routing-suite` WEAK_FLASH. The template starts with a small
  `Bash + Read` condition; after startup the prefab runtime promotes it to the
  current Mink tool surface (typically including Edit). It ends with `Ready.`

The runtime uses the bundled default template (`pro` /
`anchored-standard`) when only `with_prefab(true)` / `--prefab` is used.
Pass a template name or path to `with_prefab_named()` / `with_prefab_path()` /
`with_prefab_spec()` or `--prefab=...` to select a different template.

Placeholders:

- `{{CWD}}` — replaced with the target working directory.
- `{{AGENTS_MD}}` — replaced with the target AGENTS.md content.
- `{{SYSTEM_REMINDER_AGENTS}}` — full `<system-reminder>` user message.
- `{{SKILL_CATALOG_REMINDER}}` — skill catalog `<system-reminder>` user message.
- `{{INSTRUCTION_HINT}}` — workspace instruction hint.
- `{{FULL_SYSTEM_PROMPT}}` — full Mink system prompt injected as virtual tool result.
- `{{SKILL_RESULT_CODE}}` / `{{SKILL_RESULT_DOCUMENT}}` — live skill-search results.
- `{{SKILL_RESULT_LIST}}` — live `skill://list` result.

## Session behavior

- Seeding refuses to overwrite an existing `session.json` or non-empty `conversation.jsonl`.
- Resuming a prefab session does not re-seed or rewrite its conversation.
- Applying prefab to an existing normal session only writes a standard `prefix_snapshot` event; the conversation is untouched.
- Prefab does not create `prefab-prefix.json` or `prefab-phases.json`; the session directory keeps exactly the same file layout as a normal session.
