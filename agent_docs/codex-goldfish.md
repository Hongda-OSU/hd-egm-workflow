# Running a goldfish with `codex exec`

Measured on 2026-11-15 with codex-cli 0.169.0 (ChatGPT login). Re-check when
upgrading codex: feature names change between versions.

## The PRD flags are not enough

With only `--ephemeral --ignore-user-config --ignore-rules
--skip-git-repo-check -s read-only`, a probe showed the goldfish could:

- read `AGENTS.md` from **parent directories** (and obeyed a canary in it);
- read the global `$CODEX_HOME/AGENTS.md`;
- run shell commands (read-only sandbox blocks writes, not reads/exec);
- search the web;
- use ChatGPT-account apps, including a Notion connector with read **and
  write** tools — it could read the hidden key-decision list or edit the PRD.

Writes were blocked (`touch` failed, run dir stayed empty).

## Working invocation

```bash
codex exec --ephemeral --ignore-user-config --ignore-rules --skip-git-repo-check \
  -s read-only -m gpt-6.1-sol -c model_reasoning_effort='"medium"' \
  -c project_doc_max_bytes=0 -c web_search='"disabled"' \
  --disable shell_tool --disable unified_exec --disable code_mode_host \
  --disable apps --disable plugins --disable browser_use \
  --disable browser_use_external --disable computer_use \
  --disable in_app_browser --disable image_generation --disable view_image \
  --disable multi_agent --disable goals --disable hooks --disable skill_search \
  --disable tool_suggest --disable sleep_tool \
  --output-schema schemas/<role>.schema.json \
  --json -o <out>.json - < <assembled-prompt>
```

- Run it from a fresh empty dir, with the dedicated `CODEX_HOME` below.
- `-m` and reasoning effort must be explicit: `--ignore-user-config` drops the
  user's `config.toml`, so codex would otherwise fall back to its own default.
- `project_doc_max_bytes=0` stops project/parent `AGENTS.md` loading.
- `code_mode_host` off makes `functions.exec` fail ("code-mode host is
  disabled"); the tool names are still listed but cannot run.
- With all of the above, a probe could not read files, run commands, search,
  or reach apps.

## Dedicated CODEX_HOME (required)

The global `$CODEX_HOME/AGENTS.md` is loaded even with all flags above; no
config key disables it. Goldfish therefore run with

```bash
CODEX_HOME="$HOME/Library/Application Support/hd-egm-workflow/codex-home"
```

which holds only its own ChatGPT login (`CODEX_HOME=... codex login`, done
once by the user). Never copy or symlink `auth.json` between homes. With it,
the probe saw no AGENTS.md (global or parent canary), no shell, no search, no
apps. Codex still writes its own sqlite state/log files there.

## Still open

- `collaboration.*` tools remain listed; not yet probed.

## Cost (retell, real PRD, medium effort)

~50 s, ~16k input / ~1.7k output tokens. Output matched
`schemas/retell.schema.json` on the first try.

## Shell gotcha

The user's shell is zsh: an unquoted `$VAR` holding several flags is passed as
one argument. Use a bash array (or build args in Rust, which avoids this).
