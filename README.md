# ap

`codex resume` or `claude --resume` takes forever to load? Run `ap` it's super fast all coding agents session picker.

# Installation

```sh
cargo install coding-agents-session-picker
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
