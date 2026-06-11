# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`pull-all` is an interactive multi-repo `git pull` dashboard: a Rust/ratatui TUI that recursively discovers every git repo under a directory and pulls them in parallel with live per-repo logs. Repos can be viewed flat, grouped (`groups.json`), or as a collapsible directory tree (or tree+groups). The Rust build is canonical; the same binary also fronts Go, Bun, and bash alternatives via `pull-all go|bun|cli` subcommands (which `exec` siblings from `pull-all-siblings/`, kept off `$PATH`).

Stack: Rust (stable) · ratatui 0.29 · crossterm 0.28 (event-stream) · tokio · clap · anyhow.

## Commands

```bash
make build          # cargo build --release → bin/pull-all
make test           # cargo test
make bench          # time bin/pull-all --no-tui on the cwd
cargo clippy        # lint (keep clean before committing)
cargo test <name>   # run a single test, e.g. cargo test classify_no_upstream
```

- **Unit tests live in `src/git.rs`** (`classify_pull_output`, the `parse_*` helpers) and `src/app.rs` (retry/refetch/sort logic). Pure functions only — the TUI itself is verified manually.
- **Run the TUI:** `pull-all [DIR]` (recursive by default; `--depth N` caps it, `--no-recursive`/`--depth 1` = legacy single-level). Plain streaming mode: `pull-all --no-tui [DIR]` (the TUI is gated on `stderr` being a TTY — redirecting stderr forces plain mode). `-j N` / `PULL_JOBS` sets concurrency; `--timeout S` per pull.
- **Verifying the TUI under tmux/`script`:** auto-responses from a detached harness confuse the event reader and small-width ptys can panic pre-existing clamps — drive a **real-sized** pty (e.g. python `pty.fork` + `TIOCSWINSZ` to 120×34, render with `pyte`) and set `COLORFGBG` to skip the OSC background probe. Don't trust a blank `tmux capture-pane` as "it crashed".
- **Tests must be hermetic vs `state.json`:** `app.rs` tests run on the user's real persisted prefs — the `normalized()` test helper resets sort/filter/grouping/**tree**/collapsed sets. A manual TUI session that collapses folders/groups persists them; forgetting to reset in a new test helper makes tree/group tests fail spuriously.
- **`make build` builds AND installs.** It compiles, refreshes the repo `bin/`, and installs the binary onto `$PATH` (`$(BINDIR)`, default `$(HOME)/bin`) via an **atomic rename** — `cp …/pull-all.new && mv -f` — because a plain `cp` over a running binary fails with "Text file busy", and the rename is what the in-app new-build `↺ [reload]` watcher keys on. So after `make build`, the `pull`/`p` aliases run the new build immediately; no separate install step. `make install` only adds the sibling backends on top. Override the target dir with `make BINDIR=/some/dir build`.
- **Bump `Cargo.toml` version on every change** (patch = fix, minor = feature) — this project treats it as release-worthy.

## Architecture

Source is a flat module set under `src/` (no submodules); each file is one concern:

- **`main.rs`** — clap CLI, sibling dispatch, terminal setup, and the **synchronous event loop** (`run_event_loop`). Owns all key + mouse handling, the leader-chord state machine, and "suspend the TUI to run an external program" flows.
- **`app.rs`** — all state types: `AppState` (the god-object), `RepoState`, the status/column/sort/filter/leader/icon enums, `IconSet`, and the **pure logic + hit-test helpers** (`visible_indices`, `list_selection_at`, `set_sort`, `counts`, etc.).
- **`render.rs`** — every ratatui draw call: the two main panes, status-bar footer, info block, help/settings/confirm/diff modals, and the full-screen repo page. No state mutation except writing captured geometry back to `AppState`.
- **`worker.rs`** — async tokio tasks: the pull workers (`pull_repo`, bounded by the shared `ThrottleControl` semaphore), the streaming discovery driver (`run_discovery`) and throttle governor (`run_governor`), refetch/retry batches, and the lazy loaders for repo details, diffs, the repo page, and diff-modal file lists.
- **`git.rs`** — every `git` subprocess call + output classification/parsing + the recursive repo walker. `classify_pull_output` maps stdout/stderr+exit to a `PullOutcome` (incl. throttle detection). `spawn_repo_walker`/`should_descend`/`discover_repos_recursive` do the pruned recursive scan; `relative_path` renders a repo's path relative to the scan root.
- **`plain.rs`** — the `--no-tui` path. Output is **byte-compatible with the original bash `pull-all-repos` script for a single-level scan** (`--depth 1` / a flat dir); a recursive scan additionally lists nested repos by their relative path. Grouping and the tree are TUI-only; plain mode does **not** do throttle adaptation/auto-retry (it's a one-shot batch).
- **`groups.rs`** — repo grouping: the `groups.json` config types (pattern / repos / command / url sources), the `*`-wildcard matcher, JSON-pointer extraction, the dynamic-membership cache (`groups-cache.json`, TTL'd), and the async `run_group_resolution` task. The row model (`ListRow`, `TreeNode`, `build_tree`, `visible_rows()`, `GroupRuntime`) lives in `app.rs`. A `pattern` containing `/` matches the repo's relative path; otherwise the basename.
- **`persist.rs`** — `~/.config/pull-all/state.json` (columns, sort, icon style, theme, **background**, contrast, padding, help tab, splitter, grouping toggle, collapsed groups, tree toggle, collapsed folders, **repo-page columns**, **repo-page info panel**). `#[serde(default)]` so old files load; `load()` deserializes a missing/corrupt file from `{}` so field-level serde defaults (e.g. `repo_page_info` true) apply instead of the derived `Default`. Per-field tolerant deserializers absorb removed enum values (the old `sort_column: "discovery"` → `Name`) without resetting the whole file. The user-edited `groups.json` and the auto-written `groups-cache.json` live beside it but are owned by `groups.rs`.
- **`theme.rs`** — the `Palette` structs (dark/light × normal/soft), `palette(dark, background, contrast)` **composes** a palette from two independent axes — `Background` picks the surface tones (bg/selection/shadow), `Contrast` picks the text + accent + semantic colors — `detect_dark_background()` (OSC 11 → `COLORFGBG` → WSL registry → macOS `defaults` → dark), and the ANSI→RGB remap applied to the whole buffer at the end of every frame. Draw code in `render.rs` keeps using semantic ANSI colors (`Color::Cyan` etc.); never hardcode RGB in widgets.
- **`profile.rs`** — the optional `--profile` per-repo timing report.

### Concurrency model

`AppState` is shared as `Arc<Mutex<AppState>>` between the synchronous event loop and spawned tokio tasks. Each repo is an independent `Arc<Mutex<RepoState>>` (`SharedRepoState`). Workers mutate per-repo state; the loop reads it to render. **Before spawning a task or doing anything slow, `drop(app)` to release the `AppState` lock** — holding it across `.await` or a subprocess deadlocks the UI. The loop locks `AppState` once per iteration to render and once to handle each event.

**Repos are append-only.** Recursive discovery (`run_discovery`) streams repos in batches, appending to `app.repos` (never reordering/removing), so absolute repo indices (`repo_page`, `retry_queue`, `RepoState.index`) stay valid for the whole run. Each batch re-runs `recompute_group_assignments` + `rebuild_tree` and `reselect_repo(prev)` to keep the selection on the same repo. The "all done" edge is gated on `discovery_done && !repos.is_empty()` so an empty set never settles prematurely.

**One shared concurrency gate.** Every pull path (initial discovery, retry, refetch) acquires from the single `Arc<ThrottleControl>` semaphore on `AppState`, sized to `max_jobs`. `run_governor` enforces a reduced `effective` cap by holding "ballast" permits (acquiring `configured - effective` of them) and restores the full cap once the remote is quiet — it holds **no** `AppState` lock across its `.await`s. On a `Throttled` outcome a worker calls `control.on_throttle()` (halves the cap, debounced) and `schedule_retry`; the event loop drains `take_due_retries()` into its retry queue (exponential backoff).

### Render-every-tick

The loop polls events with a 50ms timeout and calls `terminal.draw` every iteration regardless of input. Animations (spinner, refetch attention-flash, divider drag highlight) rely on this — they're derived from `Instant`/tick at render time, not driven by events.

### UI philosophy: it's a web app in a terminal

Treat every interactive element as clickable. Filter/sort/column chips, column headers, status-filter chips, menu entries, settings radios, the repo-page column-toggle chips and `[esc back]` button — all have a mouse counterpart wired through the capture-then-hit-test pattern. Disabled states render **dim and inert** (no click region), never hidden, so the affordance stays discoverable. A new keyboard binding without a clickable counterpart (or vice versa) is incomplete — add both.

### Geometry capture → hit-testing (load-bearing)

`render.rs` writes the **exact** `Rect`s it drew into back onto `AppState` every frame (`list_rows_area`, `header_area`/`header_click`, `preview_scroll_area`, `diff_files_area`, `diff_body_area`, `diff_chips_click`, `repo_page_toggle_click`, `divider_col`, `clickable`, …). Mouse handlers in `main.rs` hit-test against those captured rects — they must **not** recompute geometry from borders/padding. Hardcoding "+1 for the border" silently breaks when panel padding or the column header shifts content; always capture-then-hit-test.

### Leader chords

`app.pending_leader` (`Toggle` = `t`, `Filter` = `f`, `Sort` = `s`, `View` = `v`, `Fold` = `z`) is a two-key chord: the first key arms it, the next picks. Handled in `main.rs` *before* the normal-key match. Current top-level keymap (see README for the full table): `t` columns · `s` sort · `f` status-filter · `v` view (`vg` groups · `vt` tree) · `z` fold (`za`/`zo`/`zc`/`zO`/`zM`/`zR`) · `-`/`+`/`*` fold-all/expand-all/expand-subtree · `←`/`→` collapse-or-jump / expand · `/` name-filter · `r`/`R` retry · `e`/`E` refetch · `c` claude · `l` lazygit · `1`/`2` pane focus. `Z` (shift) re-resolves dynamic groups.

### Icon abstraction

`IconStyle` (Unicode vs emoji) selects an `&'static IconSet`; all glyphs route through it. Render pads columns by **display width** (`pad_display`/`unicode-width`) because emoji are 2 cells. Only single-codepoint emoji are allowed — variation-selector sequences (e.g. `⏭️`, `⚠️`) render at inconsistent widths across terminals and desync/garble columns.

### Suspend-to-launch

`c` (claude) and `l` (lazygit) set a `pending_*: Option<PathBuf>` in the key handler; at the top of the next loop iteration the TUI pops keyboard-enhancement flags, leaves the alt screen, runs the external program to completion, then restores (`launch_claude` / `launch_lazygit`). ANSI parsing in `ansi_line_to_ratatui` iterates **chars, not bytes** (byte-as-char corrupts multi-byte UTF-8).

### Adding a `RepoStatus`/`PullOutcome` variant

`counts()` returns a fixed-arity tuple (**append new fields at the end** — `.5` = failed and `.7` = throttled are accessed positionally) and many `match`es over `RepoStatus` are exhaustive — a new variant ripples to `app.rs` (`is_terminal`/`is_retryable`/`sort_rank`/`counts`/`done_count`, `IconSet` glyph in both sets), `render.rs` (`status_glyph_colored`, repo-row `name_style`, `status_label`, `status_tail_for`, `build_result_summary`/`build_group_summary`/`build_folder_summary`, legend), `worker.rs` (outcome→status), `main.rs` (`build_profile_rows`), `plain.rs` (state string + summary + section + profile map), and `profile.rs`. Classification of new outcomes happens in `git.rs::classify_pull_output`.

## Repo conventions

- **This is a public personal repo: keep it free of any employer/organization-internal names** (internal service names, hosts, property IDs, private URLs, org details) in source, tests, comments, commit messages, or PR bodies — the tool scans whatever real repos you point it at, but none of that belongs in tracked content. Grep the diff before committing.
- **Verifying TUI changes:** run it under tmux and drive it with SGR mouse sequences (`\e[<0;col;row M`/`m` click, `\e[<64/65..M` wheel); `tmux capture-pane -e -p` shows color escapes for asserting active-pane borders, flashes, etc. "typecheck + clippy pass" is not "done" for visual changes.

## Docs are part of every change

The docs site (`docs/`, Astro Starlight → GitHub Pages, auto-deploys on any push touching `docs/`) and `README.md` are **updated in the same commit as any user-facing change** — adding/changing/removing a keybinding, flag, status, glyph, modal, or pane behavior — not as a follow-up.

- `docs/src/data/keymap.ts` is the **single source of truth for the keybinding table** (the in-page explorer renders from it). Keep it in sync with `main.rs`, and mirror changes in the relevant `docs/src/content/docs/**` page(s) and the `README.md` table / feature list.
- The site shows each page's git **`lastUpdated`** date (enabled in `docs/astro.config.mjs`; the deploy uses `fetch-depth: 0` so dates are real) — so a page that hasn't moved while the code churned is visibly stale.
