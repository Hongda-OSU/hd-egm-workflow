# Running the triager with `claude -p`

Measured on 2026-11-15 with Claude Code 2.1.292 (subscription login).

## Working invocation

```bash
claude -p --model claude-opus-5-5 --tools "" --strict-mcp-config \
  --disable-slash-commands --no-session-persistence --setting-sources "" \
  --system-prompt "<short triager prompt>" \
  --json-schema "<schemas/triage.schema.json without \$schema>" \
  --output-format json < <assembled-prompt>
```

Run from a fresh empty temp dir. The result is in the `structured_output`
field of the JSON on stdout; `is_error` flags failures.

## What each flag closes (probe results)

- `--tools ""`: no Bash/Read/etc. Without it the triager could read files.
- `--strict-mcp-config` (with no `--mcp-config`): no MCP servers, including
  claude.ai connectors such as Notion.
- `--setting-sources ""`: stops loading the global `~/.claude/CLAUDE.md` and
  parent-dir `CLAUDE.md` (a canary in a parent dir was obeyed without it).
- `--system-prompt`: replaces the coding-agent system prompt.
- `--bare` would be cleaner but only accepts `ANTHROPIC_API_KEY`, and the PRD
  rules out API keys.

## Known residue

Claude Code always injects a `userEmail` context block with the account
email. Neither `--setting-sources` nor `CLAUDE_CODE_USER_EMAIL` (empty or a
placeholder) removes it. Accepted: it goes only to Anthropic, is unrelated to
the PRD, and the schema leaves no field to echo it into.

## Gotcha

`--json-schema` rejects `"$schema": ".../draft/2020-12/schema"`; strip that
key before passing the schema (done in `agents::claude`).

## Cost

Round 2 of the EGM PRD (4 goldfish + triager): ~2.5 min wall time.
