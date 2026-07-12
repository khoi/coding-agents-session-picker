# casp — coding agents session picker

Lists local AI coding agent sessions. Machine-readable JSON when piped; an interactive fuzzy picker when run on a terminal.

## Supported agents

| agent | source |
|---|---|
| `claude-code` | `~/.claude/projects/**/*.jsonl` (respects `CLAUDE_CONFIG_DIR`) |
| `codex` | `~/.codex/state_5.sqlite`, falling back to scanning `~/.codex/sessions/**` rollouts; respects `CODEX_HOME` |
| `cursor` | `~/Library/Application Support/Cursor/.../state.vscdb` + `~/.cursor/projects` |
| `pi` | `~/.pi/agent/sessions/**/*.jsonl` |

Adding an agent = one module in `src/providers/` implementing `Provider`, one `Agent` enum variant, one registry line in `providers::all`.

## Usage

Following the TTY convention: stdout on a terminal → picker; stdout piped/captured → JSON list. `-f` forces list output either way — agents driving a PTY should pass `-f json`.

```sh
casp                       # terminal: picker · piped: JSON array, newest first
casp | jq                  # JSON list (stdout is a pipe)
casp -n 20 -a codex -f table
casp --cwd ~/code/myproj -f json
claude --resume (casp -a claude-code)     # pick, then resume
cd (casp --print cwd)                     # jump to a session's directory
```

```
-f, --format <FORMAT>    force list output: json | ndjson | table
-a, --agent <AGENT>      claude-code, codex, cursor, pi (repeatable or comma-separated)
    --cwd <PATH>         only sessions whose working directory is PATH or inside it
-n, --limit <N>          at most N sessions, applied after sorting
    --include-archived   include archived Codex threads
    --root <DIR>         resolve agent stores under DIR instead of $HOME
    --all                picker: start showing all directories, not just the current one
    --print <FIELD>      picker: what selection prints to stdout: id (default) | path | cwd | json
```

## Picker

The picker renders on `/dev/tty`; stdout stays clean and carries only the selected field, so it composes with command substitution. Scoped to the current directory by default.

```
> sidebar▏
scope: cwd (~/code/github.com/supabitapp/supaterm) · agent: all
▶ codex        2h ago   Revamp sidebar projects            khoi/new-tabbar
  codex        3h ago   We want to do a revamped of how …  khoi/new-tabbar
  claude-code  5h ago   Build CLI tool to list and resum…  main
~/code/github.com/supabitapp/supaterm
12/7382 · ↑↓ move · enter select · tab cwd/all · ctrl-a agent · alt-1..4 solo · esc quit
```

Keys: type to fuzzy-filter · `↑/↓` or `ctrl-p/n` move · `pgup/pgdn` jump · `tab` toggle current-directory/all scope · `ctrl-a` cycle agent · `alt-1..4` solo one agent · `enter` select · `esc` cancel.

## Schema

Every session object always has all seven keys; missing values are `null`.

```json
{
  "agent": "codex",
  "id": "019f57d5-5f74-7530-8c02-0b22d5e08eae",
  "title": "Revamp sidebar projects",
  "cwd": "/Users/you/code/proj",
  "branch": "main",
  "updated_at": "2026-07-12T20:14:33.116Z",
  "path": "/Users/you/.codex/sessions/2026/07/12/rollout-....jsonl"
}
```

Sorted by `updated_at` descending. Sessions and stderr never mix: data goes to stdout, warnings to stderr.

## Exit codes

| code | meaning |
|---|---|
| 0 | complete picture (missing stores and skipped malformed files are normal) |
| 1 | at least one agent's store failed to read; stdout still holds valid JSON of the rest |
| 2 | usage error |
| 130 | picker cancelled (esc/ctrl-c), nothing printed |

## Notes

- SQLite stores are opened read-only and WAL-aware; running agents are never blocked.
- Zstd-compressed Codex rollouts (`.jsonl.zst`) are ignored by the fallback scanner.
- Cursor stores no git branch, so `branch` is always `null` there.

## Build

```sh
cargo build --release   # binary at target/release/casp
cargo test
```
