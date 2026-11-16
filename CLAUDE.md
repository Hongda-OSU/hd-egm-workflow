# HD EGM Workflow

Local-only Mac desktop app for cross-model PRD review: Claude is the elephant
(writes the PRD), GPT goldfish with zero context review it, `claude -p` triages,
the user decides. It must never write to Notion.

Spec (source of truth): Notion page "HD EGM Workflow · 产品文档"
https://app.notion.com/p/40660d3cf9f381088712e6e662bb0f2c — read the relevant
section before implementing a feature; do not invent fields or flows.

## Stack

- Tauri 2 + Rust backend; Svelte 5 + Vite + TypeScript frontend
- Package manager: **pnpm** (not npm, not yarn)
- Rust 2024 workspace: `crates/egm-core` (logic, shared with the future
  Tauri app) + `crates/egm-cli` (binary `egm`, used until M4)
- Crates: `tokio`, `reqwest` (rustls), `serde`; `similar` comes with M5
- No Python, no Electron, no database, no cloud, no API keys
- Goldfish = `codex exec` (ChatGPT login); triager = `claude -p` (subscription)
- Settings: `~/Library/Application Support/hd-egm-workflow/settings.json`
  (page ids only); run data: `~/hd-egm-runs/`

## Commands

| Purpose | Command |
| --- | --- |
| Install frontend deps | `pnpm install` |
| Desktop app (dev) | `pnpm tauri dev` |
| Build | `cargo build` |
| All tests | `cargo test` |
| Lint | `cargo clippy --all-targets` |
| Frontend types | `pnpm check` |
| List PRDs | `./target/debug/egm list` |
| Pull a round | `./target/debug/egm pull <page-id>` |
| Full round | `./target/debug/egm run <page-id>` (uses subscription quota) |
| Finish latest round | `./target/debug/egm resume <page-id>` |
| Page as Markdown | `./target/debug/egm convert <page-id>` |

## Rules

- Notion access is **read-only**. Never add code that creates or updates
  Notion pages; the interactive Claude elephant is the only PRD writer.
- Never auto-accept or auto-apply review issues; the triager only advises.
- Each goldfish runs in a fresh temp dir with `--ephemeral
  --ignore-user-config --ignore-rules --skip-git-repo-check` and a read-only
  sandbox; prompt + PRD go in via stdin. Do not weaken this isolation.
- Strip the PRD's "关键决策清单" section before goldfish see it; only the
  triager gets it.
- Run data lives in `~/hd-egm-runs/<prd-slug>/round-<N>/`; never overwrite
  an earlier round's files.
- Output schemas live in `schemas/` (one per goldfish, passed to
  `--output-schema`); prompt templates live in `prompts/`. Keep field names
  identical to the spec's data model (§7).
- The Notion token lives in the macOS keychain only — never in files, logs,
  or test fixtures.
- Milestone gate: no UI work (M4+) until M3 produces a useful `triage.json`.
- Ask before adding a new dependency.

## Gotchas

- `codex` / `claude` read `AGENTS.md` / `CLAUDE.md` from the working dir, which
  is why goldfish must run in an empty temp dir, not the repo.
- The PRD's codex flags alone do NOT isolate a goldfish (parent `AGENTS.md`,
  shell reads, web search and Notion apps all leak). Use the full invocation
  in `agent_docs/codex-goldfish.md`.
- Notion API returns block JSON; Markdown conversion is ours. Unsupported
  blocks must leave a visible placeholder line, never be dropped silently.
- Read the token with `/usr/bin/security find-generic-password` (service
  `hd-egm-workflow`, account `notion`). Do not switch to the `keyring` crate:
  every rebuild changes the binary's signature and the keychain prompts again.
- Run `cargo clippy --all-targets`, `cargo test` and `pnpm check` before
  every commit.
- Keep `typescript` on ^6: `svelte-check` does not support TypeScript 7 yet.

## Read these only when relevant

- `agent_docs/codex-goldfish.md` — measured `codex exec` isolation flags,
  probe results, cost; read before touching goldfish invocation code.
- `agent_docs/claude-triager.md` — same for `claude -p`; read before
  touching triager invocation code.
