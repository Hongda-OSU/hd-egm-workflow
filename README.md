<img src="src-tauri/icons/128x128@2x.png" width="128" height="128" alt="HD EGM Workflow icon: a smiling pixel-art grey elephant with a small orange goldfish">

# HD EGM Workflow

Finds the gaps in a PRD that its author can't see, by having a zero-context GPT review what Claude wrote.

## Demo

I ran the app on its own PRD. It took seven rounds to reach GO, and the first round did not finish. Of the 62 issues the reviewers raised, I accepted 46.

| Round | Issues (high) | Key decisions hit | Verdict |
|---|---|---|---|
| 2 | 10 (3) | 10 / 13 | NO-GO |
| 3 | 7 (3) | 13.5 / 16 | NO-GO |
| 4 | 13 (2) | 13 / 19 | NO-GO |
| 5 | 14 (1) | 9 / 10 | NO-GO |
| 6 | 10 (1) | 9.5 / 10 | NO-GO |
| 7 | 8 (1) | 9 / 10 | GO |

## Features

- Pick a PRD from a Notion page and run a round in one click. Four zero-context GPT reviewers run in parallel: retell, critic, ready-to-build and spec checklist. `claude -p` then merges their findings and advises accept or reject.
- Decide each issue yourself, with the reviewer's original words beside it. The accepted issues become a handoff document that you paste back to Claude.
- Each round reports a GO / NO-GO per reviewer and how many of the PRD's key decisions the retell got right. Issues you rejected stay rejected in later rounds.
- Every round is saved as plain files you can replay offline. Each issue shows how its section changed in the next round, and a PRD that reached GO can be reviewed again from scratch.
- Notion access is read-only, and no API keys are needed: reviews run on ChatGPT and Claude subscriptions, and each reviewer runs isolated in an empty directory.

## Tech Stack

- App: Tauri 2, Rust (tokio, reqwest, serde, similar)
- UI: Svelte 5, TypeScript, Vite (the interface is in Chinese)
- Reviewers: Codex CLI (`codex exec`) as the GPT goldfish, Claude Code (`claude -p`) as the triager
- Data: Notion API (read-only); local JSON and Markdown files, no database

## Getting Started

### Prerequisites

- macOS, Rust 1.85+, Node.js 22+, pnpm 9+
- [Codex CLI](https://github.com/openai/codex) and [Claude Code](https://docs.anthropic.com/en/docs/claude-code), both logged in
- A Notion integration with only the "Read content" capability, connected to the page that lists your PRDs

### Installation

```bash
git clone https://github.com/Hongda-OSU/hd-egm-workflow.git
cd hd-egm-workflow
pnpm install

# Goldfish use their own Codex login, so they never see your global AGENTS.md
CODEX_HOME="$HOME/Library/Application Support/hd-egm-workflow/codex-home" codex login

# Store the Notion token in the keychain; it prompts for the token
security add-generic-password -s hd-egm-workflow -a notion -w
```

Create `~/Library/Application Support/hd-egm-workflow/settings.json`. Rule pages are optional Notion pages with your writing rules; use `null` for any you don't have.

```json
{
  "data_dir": "~/hd-egm-runs",
  "project_page_id": "<Notion page that lists your PRDs>",
  "rule_pages": {
    "hd-prd-rules": null,
    "hd-workflow-rules": null,
    "hd-goldfish-rules": null,
    "hd-elephant-rules": null
  }
}
```

Each PRD needs a key decisions section at the end, under the heading set by `KEY_DECISIONS_HEADING` in [`crates/egm-core/src/prd.rs`](crates/egm-core/src/prd.rs). The app strips it before the goldfish read the PRD, then scores their retell against it.

## Usage

```bash
pnpm tauri dev
```

Pick a PRD, start a round, decide each issue, then copy the handoff and paste it into your Claude conversation. When Claude has updated the PRD in Notion, start the next round. A round makes four `codex exec` calls and one `claude -p` call, all on your subscriptions.

The same steps work from the command line:

```bash
cargo build
./target/debug/egm list              # PRDs on the project page
./target/debug/egm run <page-id>     # pull a snapshot and run a full round
./target/debug/egm resume <page-id>  # finish the latest round's failed steps
```

## License

Apache-2.0. Parts of the review prompts are adapted from Google's [Elephant-Goldfish Model](https://github.com/google/elephant-goldfish-model); see [NOTICE](NOTICE).
