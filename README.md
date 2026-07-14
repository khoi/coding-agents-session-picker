# ap

Pick and resume your Claude Code, Codex, Cursor, and Pi sessions from one place.

# Installation

```sh
cargo install --git https://github.com/khoi/coding-agents-session-picker
```

# Usage

```sh
ap
```

Fuzzy-pick a session from the current directory and resume it in its agent. Type to filter · `ctrl-a`/`ctrl-e` start/end of line · `ctrl-b`/`ctrl-f` move cursor · `ctrl-w` delete word · `space` preview · `tab` all directories · `ctrl-t` cycle agent · `enter` resume · `esc` quit.

```sh
ap pick                                # explicit form
ap pick --all                          # pick across every directory
claude --resume (ap pick --print id)   # print the id instead of resuming
codex resume (ap pick -a codex --print id)
pi --session (ap pick -a pi --print path)
cd (ap pick --print cwd)               # jump to a session's directory

ap -f json | jq '.[0]'                 # all sessions as JSON, newest first
ap -a codex -n 20 -f table             # 20 latest Codex threads as a table
```

Session metadata is cached in the system temporary directory (`$TMPDIR/ap/sessions.json` on macOS); only new or changed transcripts are re-parsed. Delete the file to force a full rescan.
