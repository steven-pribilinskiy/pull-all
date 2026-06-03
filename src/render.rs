
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RepoStatus};

const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

fn status_glyph_colored(status: &RepoStatus, tick: u64) -> Span<'static> {
    match status {
        RepoStatus::Queued => Span::styled("◯", Style::default().fg(Color::DarkGray)),
        RepoStatus::Running { .. } => {
            let frame = SPINNER_FRAMES[(tick as usize / 2) % SPINNER_FRAMES.len()];
            Span::styled(frame.to_string(), Style::default().fg(Color::Yellow))
        }
        RepoStatus::UpToDate => Span::styled("◌", Style::default().fg(Color::Gray)),
        RepoStatus::Updated => Span::styled("✓", Style::default().fg(Color::Green)),
        RepoStatus::Skipped => Span::styled("⊘", Style::default().fg(Color::DarkGray)),
        RepoStatus::Failed => Span::styled("✗", Style::default().fg(Color::Red)),
    }
}

fn truncate_str(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        s.to_string()
    } else {
        let mut result = String::new();
        let mut width = 0;
        for ch in s.chars() {
            let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if width + char_width + 1 > max_width {
                result.push('…');
                break;
            }
            result.push(ch);
            width += char_width;
        }
        result
    }
}

/// Render a single frame into `frame`.
pub fn render(frame: &mut Frame, app: &mut AppState, tick: u64) {
    let area = frame.area();

    // Layout: main area + two-line status bar at bottom
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(area);

    let main_area = vertical_chunks[0];
    let status_bar_area = vertical_chunks[1];

    // Split main area horizontally using the adjustable ratio.
    let left_width = ((f64::from(main_area.width)) * app.split_ratio).round() as u16;
    let left_width = left_width.clamp(1, main_area.width.saturating_sub(1).max(1));
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Min(0)])
        .split(main_area);

    let list_area = horizontal_chunks[0];
    let preview_area = horizontal_chunks[1];

    // Capture geometry for mouse hit-testing in the event loop.
    app.main_area = main_area;
    app.list_area = list_area;
    app.preview_area = preview_area;
    app.divider_col = preview_area.x;

    // Render left pane (returns the list's scroll offset for hit-testing).
    let list_offset = render_list(frame, app, list_area, tick);
    app.list_offset = list_offset;

    // Render right pane
    render_preview(frame, app, preview_area, tick);

    // Render status bar
    render_status_bar(frame, app, status_bar_area);

    // Help modal overlays everything else.
    if app.show_help {
        render_help(frame, app, area);
    }
}

fn render_list(frame: &mut Frame, app: &AppState, area: Rect, tick: u64) -> usize {
    let visible = app.visible_indices();
    let total_repos = app.repos.len();
    let elapsed = app.start.elapsed().as_secs_f64();

    let done = app.done_count();
    let title = format!(
        " pull-all · {done}/{total_repos} · {elapsed:.1}s "
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Compute column widths
    let max_name_len = app
        .repos
        .iter()
        .map(|repo| repo.lock().unwrap().name.len())
        .max()
        .unwrap_or(10)
        .max(10);

    // icon + space + name + space + branch
    // Name column: max_name_len
    let name_col_width = max_name_len;
    let icon_width = 2; // glyph + space
    let separator_width = 1; // space before branch

    let inner_width = inner.width as usize;
    let branch_col_width = inner_width
        .saturating_sub(icon_width + name_col_width + separator_width + 2);

    let mut items: Vec<ListItem> = visible
        .iter()
        .map(|&repo_idx| {
            let state = app.repos[repo_idx].lock().unwrap();
            let glyph = status_glyph_colored(&state.status, tick);

            let name_padded = format!("{:<width$}", state.name, width = name_col_width);
            let branch_str = state
                .branch
                .as_deref()
                .unwrap_or("—")
                .to_string();
            let branch_truncated = truncate_str(&branch_str, branch_col_width.max(1));

            let name_style = match &state.status {
                RepoStatus::Failed => Style::default().fg(Color::Red),
                RepoStatus::Updated => Style::default().fg(Color::Green),
                RepoStatus::Skipped => Style::default().fg(Color::DarkGray),
                RepoStatus::Running { .. } => Style::default().fg(Color::Yellow),
                _ => Style::default(),
            };

            let line = Line::from(vec![
                glyph,
                Span::raw(" "),
                Span::styled(name_padded, name_style),
                Span::raw(" "),
                Span::styled(branch_truncated, Style::default().fg(Color::Cyan)),
            ]);
            ListItem::new(line)
        })
        .collect();

    // Add separator and Result item
    items.push(ListItem::new(Line::from(vec![Span::styled(
        "─".repeat(inner_width.saturating_sub(2)),
        Style::default().fg(Color::DarkGray),
    )])));

    let result_glyph = if app.all_done {
        let (_, _, _, _, _, failed) = app.counts();
        if failed > 0 {
            Span::styled("✗", Style::default().fg(Color::Red))
        } else {
            Span::styled("✓", Style::default().fg(Color::Green))
        }
    } else {
        Span::styled("—", Style::default().fg(Color::DarkGray))
    };

    let result_style = if app.selected == visible.len() + 1 {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    items.push(ListItem::new(Line::from(vec![
        result_glyph,
        Span::raw(" "),
        Span::styled("Result", result_style),
    ])));

    let mut list_state = ListState::default();
    // Map selected index to list index (skipping the separator line)
    if app.selected < visible.len() {
        list_state.select(Some(app.selected));
    } else {
        // +1 for separator
        list_state.select(Some(visible.len() + 1));
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("→ ");

    frame.render_stateful_widget(list, inner, &mut list_state);

    list_state.offset()
}

fn render_preview(frame: &mut Frame, app: &AppState, area: Rect, _tick: u64) {
    let visible = app.visible_indices();

    // When the Result overlay is active, show the summary regardless of selection.
    let show_result = app.result_overlay || app.selected >= visible.len();

    let (header_text, log_lines, scroll_offset, _auto_scroll) =
        if !show_result {
            let repo_idx = visible[app.selected];
            let state = app.repos[repo_idx].lock().unwrap();
            let pid_str = match &state.status {
                RepoStatus::Running { pid } => format!("pid {pid}"),
                _ => "pid —".to_string(),
            };
            let elapsed_str = match state.elapsed {
                Some(elapsed) => format!(" · {:.2}s", elapsed.as_secs_f64()),
                None => match state.start {
                    Some(start) => format!(" · {:.2}s", start.elapsed().as_secs_f64()),
                    None => String::new(),
                },
            };
            let header = format!(
                " {} · {} · {}{} ",
                state.name,
                match &state.status {
                    RepoStatus::Queued => "queued",
                    RepoStatus::Running { .. } => "running",
                    RepoStatus::UpToDate => "up-to-date",
                    RepoStatus::Updated => "updated",
                    RepoStatus::Skipped => "skipped",
                    RepoStatus::Failed => "failed",
                },
                pid_str,
                elapsed_str
            );
            let lines: Vec<String> = state.log.lines().iter().cloned().collect();
            let scroll = state.preview_scroll;
            let auto = state.auto_scroll;
            (header, lines, scroll, auto)
        } else {
            // Result item
            let summary = build_result_summary(app);
            (" Result ".to_string(), summary, 0, true)
        };

    let focused = app.preview_focused;
    let border_style = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(header_text)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_height = inner.height as usize;
    let total_lines = log_lines.len();

    // Convert log lines to ratatui Text with ANSI color support
    let text_lines: Vec<Line> = log_lines
        .iter()
        .map(|line| ansi_line_to_ratatui(line))
        .collect();

    // Compute actual scroll: if auto_scroll, pin to bottom
    let effective_scroll = if scroll_offset > total_lines.saturating_sub(inner_height) {
        total_lines.saturating_sub(inner_height)
    } else {
        scroll_offset
    };

    let text = Text::from(text_lines);
    let para = Paragraph::new(text)
        .scroll((effective_scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Convert a string that may contain ANSI escape codes to a ratatui Line.
/// We use a simple parser for the common SGR codes git produces.
fn ansi_line_to_ratatui(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut current_style = Style::default();
    let mut current_text = String::new();

    let bytes = line.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        if bytes[pos] == b'\x1b' && pos + 1 < bytes.len() && bytes[pos + 1] == b'[' {
            // ESC [ ... m — SGR sequence
            if !current_text.is_empty() {
                spans.push(Span::styled(current_text.clone(), current_style));
                current_text.clear();
            }
            pos += 2;
            let start = pos;
            while pos < bytes.len() && bytes[pos] != b'm' {
                pos += 1;
            }
            if pos < bytes.len() {
                let code_str = std::str::from_utf8(&bytes[start..pos]).unwrap_or("");
                current_style = apply_sgr(current_style, code_str);
                pos += 1; // skip 'm'
            }
        } else {
            current_text.push(bytes[pos] as char);
            pos += 1;
        }
    }

    if !current_text.is_empty() {
        spans.push(Span::styled(current_text, current_style));
    }

    Line::from(spans)
}

fn apply_sgr(style: Style, code_str: &str) -> Style {
    for code in code_str.split(';') {
        let code = code.trim().parse::<u8>().unwrap_or(0);
        match code {
            0 => return Style::default(),
            1 => return style.add_modifier(Modifier::BOLD),
            2 => return style.add_modifier(Modifier::DIM),
            4 => return style.add_modifier(Modifier::UNDERLINED),
            7 => return style.add_modifier(Modifier::REVERSED),
            30 => return style.fg(Color::Black),
            31 => return style.fg(Color::Red),
            32 => return style.fg(Color::Green),
            33 => return style.fg(Color::Yellow),
            34 => return style.fg(Color::Blue),
            35 => return style.fg(Color::Magenta),
            36 => return style.fg(Color::Cyan),
            37 => return style.fg(Color::White),
            90 => return style.fg(Color::DarkGray),
            91 => return style.fg(Color::LightRed),
            92 => return style.fg(Color::LightGreen),
            93 => return style.fg(Color::LightYellow),
            94 => return style.fg(Color::LightBlue),
            95 => return style.fg(Color::LightMagenta),
            96 => return style.fg(Color::LightCyan),
            97 => return style.fg(Color::Gray),
            _ => {}
        }
    }
    style
}

fn build_result_summary(app: &AppState) -> Vec<String> {
    let mut lines = Vec::new();

    let (_, _, updated_count, up_to_date_count, skipped_count, failed_count) = app.counts();

    let total = updated_count + up_to_date_count + skipped_count + failed_count;

    lines.push("Pull completed!".to_string());
    lines.push(String::new());

    if total == 0 {
        lines.push(format!(
            "   No git repositories found."
        ));
        return lines;
    }

    let mut parts = Vec::new();
    if updated_count > 0 {
        parts.push(format!("{updated_count} updated"));
    }
    if up_to_date_count > 0 {
        parts.push(format!("{up_to_date_count} up-to-date"));
    }
    if skipped_count > 0 {
        parts.push(format!("{skipped_count} skipped"));
    }
    if failed_count > 0 {
        parts.push(format!("{failed_count} failed"));
    }

    lines.push(format!("   {total} total: {}", parts.join(", ")));

    // Compute padding width — include worktree repo names too
    let mut pad = 0;
    for repo in &app.repos {
        let name_len = repo.lock().unwrap().name.len();
        if name_len > pad {
            pad = name_len;
        }
    }
    for wt in &app.worktrees {
        if wt.repo.len() > pad {
            pad = wt.repo.len();
        }
    }

    // Collect repos by status
    let collect_by_status = |status_fn: &dyn Fn(&RepoStatus) -> bool| -> Vec<(String, String)> {
        app.repos
            .iter()
            .filter(|repo| {
                let state = repo.lock().unwrap();
                status_fn(&state.status)
            })
            .map(|repo| {
                let state = repo.lock().unwrap();
                (
                    state.name.clone(),
                    state.branch.clone().unwrap_or_else(|| "?".to_string()),
                )
            })
            .collect()
    };

    let updated_repos = collect_by_status(&|status| matches!(status, RepoStatus::Updated));
    let up_to_date_repos =
        collect_by_status(&|status| matches!(status, RepoStatus::UpToDate));
    let skipped_repos = collect_by_status(&|status| matches!(status, RepoStatus::Skipped));
    let failed_repos = collect_by_status(&|status| matches!(status, RepoStatus::Failed));

    let print_section = |lines: &mut Vec<String>, header: &str, repos: &[(String, String)]| {
        if repos.is_empty() {
            return;
        }
        lines.push(String::new());
        lines.push(header.to_string());
        for (name, branch) in repos {
            lines.push(format!("   - {name:<pad$}  {branch}"));
        }
    };

    print_section(&mut lines, "+ Updated repositories:", &updated_repos);
    print_section(&mut lines, "= Unchanged repositories:", &up_to_date_repos);
    print_section(
        &mut lines,
        "! Skipped repositories (uncommitted changes):",
        &skipped_repos,
    );
    print_section(&mut lines, "x Failed repositories:", &failed_repos);

    if !app.worktrees.is_empty() {
        lines.push(String::new());
        lines.push("> Active worktrees:".to_string());
        for wt in &app.worktrees {
            lines.push(format!("   - {:<pad$}  {}", wt.repo, wt.branch));
        }
    }

    lines
}


fn render_status_bar(frame: &mut Frame, app: &AppState, area: Rect) {
    let (_, running, _, _, _, _) = app.counts();
    let done = app.done_count();
    let total = app.repos.len();
    let elapsed = app.start.elapsed().as_secs_f64();

    // Row 1 — move & view (or the live filter prompt when filtering).
    let row1 = if app.filter_input_mode {
        format!("Filter: {}", app.filter.as_deref().unwrap_or(""))
    } else {
        let filter_tag = match &app.filter {
            Some(filter) if !filter.is_empty() => format!("[{filter}] "),
            _ => String::new(),
        };
        format!(
            "{filter_tag}j/k ↑/↓ move · g/G top/end · click select · wheel scroll · space result · ? help"
        )
    };

    // Row 2 — act & layout, plus live run stats. Action letters dim when they're a no-op:
    // r/R (retry) need a failed/skipped repo; f/F (refetch) need a repo that isn't in progress.
    let hint = Style::default().fg(Color::DarkGray);
    let active = Style::default().fg(Color::Gray);
    let dimmed = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);

    let style_retry_one = if app.selected_repo_retryable() { active } else { dimmed };
    let style_retry_all = if app.any_retryable() { active } else { dimmed };
    let style_refetch_one = if app.selected_repo_refetchable() { active } else { dimmed };
    let style_refetch_all = if app.any_refetchable() { active } else { dimmed };

    let stats = format!(
        "  ·  {} jobs · {done}/{total} done · {running} running · {elapsed:.1}s",
        app.max_jobs
    );

    let row2 = Line::from(vec![
        Span::styled("r", style_retry_one),
        Span::styled("/", hint),
        Span::styled("R", style_retry_all),
        Span::styled(" retry · ", hint),
        Span::styled("f", style_refetch_one),
        Span::styled("/", hint),
        Span::styled("F", style_refetch_all),
        Span::styled(" refetch · / filter · [ ] / drag resize · tab focus · q quit", hint),
        Span::styled(stats, hint),
    ]);

    let text = Text::from(vec![Line::from(row1), row2]);
    let para = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(para, area);
}

/// A centered rect of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// Render the `?` help modal: clickable links, subcommands, flags/env, grouped hotkeys,
/// exit codes, and the repo list (each row clickable to open its remote). Records the
/// screen row of every clickable line into `app.help_links` for mouse hit-testing.
fn render_help(frame: &mut Frame, app: &mut AppState, area: Rect) {
    const GITHUB_URL: &str = "https://github.com/steven-pribilinskiy/pull-all";
    const NOTES_BAKEOFF: &str =
        "https://notes.lvh.me/library/default/devtools/pull-all-tui-bake-off-2026.md";
    const NOTES_FEATURES: &str =
        "https://notes.lvh.me/library/default/devtools/pull-all-tui-interaction-features-2026.md";

    let header_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::Gray);
    let link_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::UNDERLINED);
    let dim_style = Style::default().fg(Color::DarkGray);

    // Each item is a line plus an optional URL that makes the whole row clickable.
    let mut items: Vec<(Line<'static>, Option<String>)> = Vec::new();
    let header = |text: &str| (Line::from(Span::styled(text.to_string(), header_style)), None);
    let plain = |text: &str| (Line::from(text.to_string()), None);
    let link = |label: &str, url: &str| {
        let line = Line::from(vec![
            Span::styled(format!("{label:<9}"), label_style),
            Span::styled(url.to_string(), link_style),
        ]);
        (line, Some(url.to_string()))
    };

    items.push((
        Line::from(Span::styled(
            "pull-all — interactive multi-repo git pull dashboard".to_string(),
            header_style,
        )),
        None,
    ));
    items.push(plain(""));
    items.push(link("GitHub", GITHUB_URL));
    items.push(link("Notes", NOTES_BAKEOFF));
    items.push(link("", NOTES_FEATURES));
    items.push(plain(""));

    items.push(header("SUBCOMMANDS  (forward to sibling builds; args passed through)"));
    items.push(plain("  pull-all go  [args]   Go / bubbletea build"));
    items.push(plain("  pull-all bun [args]   Bun / ink build (JIT)"));
    items.push(plain("  pull-all cli [args]   bash streaming version"));
    items.push(plain(""));

    items.push(header("FLAGS & ENVIRONMENT"));
    items.push(plain("  [DIR]                          directory to scan (default: cwd)"));
    items.push(plain("  -j N  / PULL_JOBS=N            concurrency (default: nproc)"));
    items.push(plain("  --timeout S / PULL_TIMEOUT=S   per-pull timeout seconds (default: 30)"));
    items.push(plain("  --no-tui                       plain streaming output (no TUI)"));
    items.push(plain("  --no-worktrees                 skip worktree discovery"));
    items.push(plain("  --profile / PULL_PROFILE=1     per-repo timing report (slowest first)"));
    items.push(plain("  --profile-out FILE             write the profile report to FILE"));
    items.push(plain(""));

    items.push(header("HOTKEYS"));
    items.push(plain("  Move     j/k  ↑/↓  ·  g/G top/end  ·  wheel scroll  ·  click a row to select"));
    items.push(plain("  View     space result overlay  ·  tab list/preview focus  ·  PgUp/PgDn scroll preview  ·  End resume autoscroll"));
    items.push(plain("  Retry    r selected · R all          (repos that failed or were skipped)"));
    items.push(plain("  Refetch  f selected · F all          (re-pull anything; skips in-progress)"));
    items.push(plain("  Layout   [ ] resize panes  ·  drag the divider to resize"));
    items.push(plain("  Filter   / filter by name  ·  Esc clear filter"));
    items.push(plain("  Other    c clear log  ·  ? this help  ·  q quit  ·  Ctrl-C exit"));
    items.push(plain(""));

    items.push(header("EXIT CODES"));
    items.push(plain("  0 all ok  ·  1 any failed  ·  2 quit mid-run  ·  130 Ctrl-C"));
    items.push(plain(""));

    items.push(header("REPOSITORIES  (click a row to open it on its host)"));
    let name_pad = app
        .repos
        .iter()
        .map(|repo| repo.lock().unwrap().name.chars().count())
        .max()
        .unwrap_or(0)
        .min(30);
    for repo in &app.repos {
        let state = repo.lock().unwrap();
        let name = state.name.clone();
        let branch = state.branch.clone().unwrap_or_else(|| "?".to_string());
        let prefix = Span::styled(format!("  {name:<name_pad$}  "), label_style);
        let branch_span = Span::styled(format!("{branch:<16}"), Style::default().fg(Color::Cyan));
        match &state.remote_url {
            Some(url) => {
                let line = Line::from(vec![
                    prefix,
                    branch_span,
                    Span::styled(url.clone(), link_style),
                ]);
                items.push((line, Some(url.clone())));
            }
            None => {
                let line = Line::from(vec![
                    prefix,
                    branch_span,
                    Span::styled("(no remote)".to_string(), dim_style),
                ]);
                items.push((line, None));
            }
        }
    }

    let modal_width = area.width.saturating_sub(4).min(110).max(40);
    let modal_height = area.height.saturating_sub(2).max(8);
    let modal_area = centered_rect(modal_width, modal_height, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" pull-all — help ")
        .title_bottom(Line::from(" ↑/↓ scroll · click a link · ?/Esc close ").right_aligned());
    let inner = block.inner(modal_area);

    // Clamp scroll to the content, then window the visible slice.
    let inner_height = inner.height as usize;
    let max_scroll = items.len().saturating_sub(inner_height);
    if app.help_scroll > max_scroll {
        app.help_scroll = max_scroll;
    }
    let start = app.help_scroll;
    let end = (start + inner_height).min(items.len());

    app.help_links.clear();
    let mut lines: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));
    for (offset, (line, url)) in items[start..end].iter().enumerate() {
        if let Some(url) = url {
            app.help_links.push((inner.y + offset as u16, url.clone()));
        }
        lines.push(line.clone());
    }

    frame.render_widget(Clear, modal_area);
    frame.render_widget(block, modal_area);
    frame.render_widget(Paragraph::new(lines), inner);
}
