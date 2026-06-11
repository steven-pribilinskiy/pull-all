---
title: Architecture
description: How the pull-all Rust crate is organized.
---

`pull-all` is a small Rust crate built on [ratatui](https://ratatui.rs) (TUI), [crossterm](https://github.com/crossterm-rs/crossterm)
(terminal/input), and [tokio](https://tokio.rs) (async pulls).

## Modules

| File | Responsibility |
|------|----------------|
| `src/main.rs` | CLI entry point, sibling dispatch, TUI setup, and the event loop. |
| `src/app.rs` | Application state types (`AppState`, `RepoState`, `LogBuffer`, `TreeNode`/`build_tree`, `ThrottleControl`, page/diff/confirm models) and retry/refetch eligibility + tree/fold helpers. |
| `src/git.rs` | Git operations plus the **recursive repo walker** (`spawn_repo_walker`, `should_descend`, `discover_repos_recursive`, `relative_path`) and `classify_pull_output` (incl. throttle detection); unit tests. |
| `src/worker.rs` | Async workers bounded by the shared `ThrottleControl` semaphore — streaming discovery (`run_discovery`), the throttle governor (`run_governor`), pulls, page loads (incl. `run_branch_stats`, a detached per-branch A/M/D stats pass), diffs, and the branch/worktree/stash/discard mutations. |
| `src/render.rs` | Ratatui rendering — list pane (flat/grouped/tree), preview pane, status bar, repo page (with its column system + info panel), diff modal, confirm/settings/help modals, the throttle banner, and ANSI color support. |
| `src/plain.rs` | Non-TUI streaming output, byte-compatible with the bash reference for a single-level scan. |
| `src/groups.rs` | Repo grouping — `groups.json` config (pattern/repos/command/url sources), the wildcard matcher, JSON extraction, the dynamic-membership cache, and the async resolution task. |
| `src/persist.rs` | UI preferences saved to `~/.config/pull-all/state.json` (columns, sort, icon style, padding, theme, background, contrast, help tab, splitter, grouping, collapsed groups, tree toggle, collapsed folders, repo-page columns, repo-page info panel). Per-field tolerant deserializers absorb removed enum values (old `sort_column: "discovery"` → `Name`) and a missing/corrupt file loads from `{}` so field defaults apply. |
| `src/theme.rs` | Color palettes composed from two independent axes — **background** (surface tones) × **contrast** (text/accent saturation), each dark/light — the per-frame ANSI→RGB remap, and terminal background detection for the auto theme. |
| `src/profile.rs` | The optional `--profile` per-repo timing report. |

## How a pull flows

1. `run_discovery` walks the target directory **recursively** (pruned; `--depth N` caps it),
   streaming each found repo into `app.repos` and discovering `.worktrees/*/.git` once the
   walk completes.
2. Each discovered repo's pull is spawned immediately, bounded by the shared `ThrottleControl`
   semaphore, streaming output into its repo's `LogBuffer`. On remote throttling the governor
   drops the effective cap and the repo is re-queued with backoff.
3. `render` redraws the TUI each tick from the shared `AppState`.
4. Key and mouse events flow through the event loop in `main.rs`, which mutates state and
   spawns mutation workers (checkout, fast-forward, delete, drop, remove, discard).

## Input enhancements

`main.rs` pushes the Kitty keyboard-protocol enhancement flags when the terminal supports
them, so modified keys like `Shift`+`Enter` are reported distinctly. The flags are popped
on teardown, on panic, and while a suspended external session (a `c`-launched claude or an
`l`-launched [lazygit](https://github.com/jesseduffield/lazygit)) has the terminal.

## Geometry capture & hit-testing

`render.rs` writes the exact `Rect`s it drew back onto `AppState` each frame (the repo-rows
area, the column-header cells, scrollbar tracks, diff-modal panels, the divider column). Mouse
handlers hit-test against those captured rects rather than recomputing them from borders, so
clicks stay correct regardless of panel padding or the column header.
