---
title: CLI flags & env
description: Every pull-all command-line flag, positional argument, and environment variable.
---

```
pull-all [OPTIONS] [DIR]
```

## Positional argument

| Argument | Default | Description |
|----------|---------|-------------|
| `DIR` | current directory | Directory to scan **recursively** for git repos to pull. |

The scan is recursive by default — it crawls the tree in parallel, pruning hidden dirs,
`node_modules`/`vendor`/`target`/`dist`/… and `*.worktrees`, and never descending into a
found repo. Use `--depth 1` (or `--no-recursive`) for the legacy single-level scan. A
directory literally named `go`, `bun`, or `cli` is reachable as `pull-all ./go` —
see [Sibling builds](../siblings/).

## Flags

| Flag | Env | Default | Description |
|------|-----|---------|-------------|
| `-j`, `--jobs <N>` | `PULL_JOBS` | `nproc` | Maximum concurrent pulls. Reduced automatically when a remote throttles, restored when it's quiet. |
| `--depth <N>` | | `16` | Maximum directory depth to scan (`1` = immediate subdirs only). |
| `--no-recursive` | | off | Scan only the immediate subdirectories (same as `--depth 1`). |
| `--timeout <SECS>` | `PULL_TIMEOUT` | `30` | Per-pull timeout in seconds. |
| `--no-tui` | | off | Force plain streaming output (no TUI). |
| `--no-worktrees` | | off | Skip `.worktrees/*/.git` discovery. |
| `--profile` | | off | Emit a per-repo timing report (slowest first) after the run. |
| `--profile-out <FILE>` | | stderr | Write the profile report to a file instead of stderr. |
| `--version` | | | Print the version and exit. |
| `--help` | | | Print help and exit. |

## Environment variables

| Variable | Description |
|----------|-------------|
| `PULL_JOBS` | Same as `-j`/`--jobs`. |
| `PULL_TIMEOUT` | Same as `--timeout`. |
| `PULL_CLAUDE_CMD` | Command run by the `c` key (default `cc`, i.e. `claude --dangerously-skip-permissions`). |
| `BROWSER` | Preferred opener for the `o` key (falls back to `wslview`, `xdg-open`, `open`). |

## Examples

```bash
pull-all                              # pull the current directory tree, TUI
pull-all ~/projects -j 16             # recursive scan, 16 parallel pulls
pull-all ~ --depth 4                  # crawl home, capped at 4 levels deep
pull-all --no-recursive ~/projects    # legacy single-level scan
PULL_JOBS=8 pull-all ~/projects       # concurrency via env
pull-all --no-tui ~/projects          # plain output for scripts/CI
pull-all --timeout 60 ~/work          # allow slow remotes 60s each
pull-all --profile --profile-out /tmp/pull.prof ~/projects
```
