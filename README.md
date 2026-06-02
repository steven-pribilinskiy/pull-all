# pull-all-tui

Interactive multi-repo git pull dashboard. Pulls every git repo in a directory in parallel with live per-repo logs, retry support, and a two-pane TUI layout.

## Features

- Parallel pulls with configurable concurrency (default: nproc)
- Live log streaming per repo in a scrollable preview pane
- Status glyphs: queued / running / up-to-date / updated / skipped / failed
- Retry failed repos without re-running the rest (`r` / `R`)
- Worktree discovery (`.worktrees/*/.git`)
- Filter repos by name with `/`
- Non-TUI fallback (same output as bash reference) when not on a TTY or with `--no-tui`
- Exit codes: 0 (all ok), 1 (any failed), 2 (user quit mid-run), 130 (Ctrl-C)

## Building

```bash
# Requires Rust stable (cargo)
make build
# Binary at: bin/pull-all-tui
```

## Running

```bash
# TUI mode (auto-detected when stderr is a TTY)
bin/pull-all-tui [DIR]

# Plain streaming output (matches bash reference)
bin/pull-all-tui --no-tui [DIR]

# Custom concurrency
bin/pull-all-tui -j 8 [DIR]
# or
PULL_JOBS=8 bin/pull-all-tui [DIR]

# Custom timeout per pull (default: 30s)
bin/pull-all-tui --timeout 60 [DIR]

# Skip worktree discovery
bin/pull-all-tui --no-worktrees [DIR]
```

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `↓` | Next repo |
| `k` / `↑` | Previous repo |
| `g` | Jump to top |
| `G` | Jump to bottom (Result item) |
| `Space` | Toggle the Result summary in the preview without moving selection (any navigation clears it) |
| `[` / `]` | Narrow / widen the left pane |
| `Tab` | Toggle focus: list ↔ preview |
| `PgUp` / `PgDn` | Scroll preview (when focused) |
| `End` | Resume auto-scroll in preview |
| `r` / `Enter` | Retry selected failed repo |
| `R` | Retry all failed repos |
| `c` | Clear log buffer for selected repo |
| `/` | Filter repos by name |
| `Esc` | Clear filter (or quit when no filter) |
| `q` | Quit |
| `Ctrl-C` | Quit (exit 130) |

### Mouse

Click a repo row to select it, scroll the wheel over the left pane to move the
selection or over the right pane to scroll the preview, and drag the divider
between the panes to resize. While the TUI is running it captures the mouse, so
native terminal text-selection is suspended until you quit (same tradeoff as
lazygit/htop).

## Testing

```bash
make test
```

## Benchmark

```bash
make bench
```

## Architecture

- `src/main.rs` — CLI entry point, TUI setup, event loop
- `src/app.rs` — Application state types (`AppState`, `RepoState`, `LogBuffer`)
- `src/git.rs` — Git operations (`discover_repos`, `get_branch`, `is_dirty`, `diff_stat`, `classify_pull_output`) + unit tests
- `src/worker.rs` — Async pull workers with semaphore concurrency control
- `src/render.rs` — Ratatui rendering (list pane, preview pane, status bar, ANSI color support)
- `src/plain.rs` — Non-TUI streaming output (byte-compatible with bash reference)
