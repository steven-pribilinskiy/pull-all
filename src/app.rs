use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

/// Maximum lines in the per-repo ring buffer.
pub const RING_BUFFER_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoStatus {
    Queued,
    Running { pid: u32 },
    UpToDate,
    Updated,
    Skipped,
    Failed,
}

impl RepoStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RepoStatus::UpToDate
                | RepoStatus::Updated
                | RepoStatus::Skipped
                | RepoStatus::Failed
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, RepoStatus::Failed)
    }
}

/// Ring buffer capped at `RING_BUFFER_CAPACITY` lines.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: VecDeque<String>,
}

impl LogBuffer {
    pub fn push(&mut self, line: String) {
        if self.lines.len() >= RING_BUFFER_CAPACITY {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> &VecDeque<String> {
        &self.lines
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

#[derive(Debug)]
pub struct RepoState {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub status: RepoStatus,
    /// Log ring buffer (stdout + stderr from git pull).
    pub log: LogBuffer,
    /// Whether the preview pane should auto-scroll to bottom.
    pub auto_scroll: bool,
    /// Preview pane scroll offset (lines from top).
    pub preview_scroll: usize,
    /// When this repo's pull began (after acquiring the concurrency permit).
    pub start: Option<Instant>,
    /// Wall-clock time spent on this repo, set when a terminal status is assigned.
    pub elapsed: Option<Duration>,
}

impl RepoState {
    pub fn new(name: impl Into<String>, path: PathBuf) -> Self {
        RepoState {
            name: name.into(),
            path,
            branch: None,
            status: RepoStatus::Queued,
            log: LogBuffer::default(),
            auto_scroll: true,
            preview_scroll: 0,
            start: None,
            elapsed: None,
        }
    }
}

pub type SharedRepoState = Arc<Mutex<RepoState>>;

/// Worktree entry discovered from `<repo>.worktrees/<branch>/.git`.
#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub repo: String,
    pub branch: String,
}

/// The overall application state, shared between the async worker tasks and the UI.
pub struct AppState {
    /// Repos in alphabetical order.
    pub repos: Vec<SharedRepoState>,
    /// Worktree entries (discovered asynchronously).
    pub worktrees: Vec<WorktreeEntry>,
    /// Worktree discovery complete?
    pub worktrees_done: bool,
    /// Index of the selected item in the list (0 = first repo, repos.len() = Result).
    pub selected: usize,
    /// Whether the user has manually moved the selection (disables auto-select).
    pub user_navigated: bool,
    /// Whether focus is on the preview pane (for preview scroll keys).
    pub preview_focused: bool,
    /// Filter string (from `/` mode).
    pub filter: Option<String>,
    /// Filter input mode active?
    pub filter_input_mode: bool,
    /// Wall-clock start time.
    pub start: Instant,
    /// All pulls are done?
    pub all_done: bool,
    /// Number of jobs configured.
    pub max_jobs: usize,
    /// Left-pane width as a fraction of the main area (clamped MIN_SPLIT..MAX_SPLIT).
    pub split_ratio: f64,
    /// When true, the preview shows the Result summary regardless of selection.
    pub result_overlay: bool,
    /// Main content area (above the status bar) — captured each render for hit-testing.
    pub main_area: Rect,
    /// Left list pane rect — captured each render for hit-testing.
    pub list_area: Rect,
    /// Right preview pane rect — captured each render for hit-testing.
    pub preview_area: Rect,
    /// Column of the divider between the panes (= preview_area.x).
    pub divider_col: u16,
    /// Scroll offset of the list widget, read back after render for row hit-testing.
    pub list_offset: usize,
}

impl AppState {
    pub fn new(repos: Vec<SharedRepoState>, max_jobs: usize) -> Self {
        AppState {
            repos,
            worktrees: Vec::new(),
            worktrees_done: false,
            selected: 0,
            user_navigated: false,
            preview_focused: false,
            filter: None,
            filter_input_mode: false,
            start: Instant::now(),
            all_done: false,
            max_jobs,
            split_ratio: Self::DEFAULT_SPLIT,
            result_overlay: false,
            main_area: Rect::default(),
            list_area: Rect::default(),
            preview_area: Rect::default(),
            divider_col: 0,
            list_offset: 0,
        }
    }

    pub const DEFAULT_SPLIT: f64 = 0.4;
    pub const MIN_SPLIT: f64 = 0.2;
    pub const MAX_SPLIT: f64 = 0.7;

    /// Nudge the split ratio by `delta`, clamped to the allowed range.
    pub fn adjust_split(&mut self, delta: f64) {
        self.split_ratio = (self.split_ratio + delta).clamp(Self::MIN_SPLIT, Self::MAX_SPLIT);
    }

    /// Set the split ratio from an absolute divider column (mouse drag).
    pub fn set_split_from_col(&mut self, col: u16) {
        if self.main_area.width == 0 {
            return;
        }
        let rel = f64::from(col.saturating_sub(self.main_area.x)) / f64::from(self.main_area.width);
        self.split_ratio = rel.clamp(Self::MIN_SPLIT, Self::MAX_SPLIT);
    }

    /// Map mouse coordinates to a list selection index, or None for the
    /// separator row / outside the list. Result maps to `visible_len`.
    pub fn list_selection_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.list_area;
        if area.width < 2 || area.height < 2 {
            return None;
        }
        let inner_x = area.x + 1;
        let inner_y = area.y + 1;
        let inner_right = inner_x + (area.width - 2);
        let inner_bottom = inner_y + (area.height - 2);
        if col < inner_x || col >= inner_right || row < inner_y || row >= inner_bottom {
            return None;
        }
        let row_idx = (row - inner_y) as usize + self.list_offset;
        let visible_len = self.visible_indices().len();
        if row_idx < visible_len {
            Some(row_idx)
        } else if row_idx == visible_len + 1 {
            Some(visible_len)
        } else {
            None
        }
    }

    /// Returns indices of repos visible given the current filter.
    pub fn visible_indices(&self) -> Vec<usize> {
        match &self.filter {
            None => (0..self.repos.len()).collect(),
            Some(filter) => {
                let filter_lower = filter.to_lowercase();
                self.repos
                    .iter()
                    .enumerate()
                    .filter(|(_, repo)| {
                        let state = repo.lock().unwrap();
                        state.name.to_lowercase().contains(&filter_lower)
                    })
                    .map(|(index, _)| index)
                    .collect()
            }
        }
    }

    /// Total items in the list (visible repos + 1 Result item).
    pub fn list_len(&self) -> usize {
        self.visible_indices().len() + 1
    }

    /// Count of repos in each state.
    pub fn counts(&self) -> (usize, usize, usize, usize, usize, usize) {
        let mut queued = 0;
        let mut running = 0;
        let mut updated = 0;
        let mut up_to_date = 0;
        let mut skipped = 0;
        let mut failed = 0;
        for repo in &self.repos {
            let state = repo.lock().unwrap();
            match &state.status {
                RepoStatus::Queued => queued += 1,
                RepoStatus::Running { .. } => running += 1,
                RepoStatus::Updated => updated += 1,
                RepoStatus::UpToDate => up_to_date += 1,
                RepoStatus::Skipped => skipped += 1,
                RepoStatus::Failed => failed += 1,
            }
        }
        (queued, running, updated, up_to_date, skipped, failed)
    }

    pub fn done_count(&self) -> usize {
        let (_, _, updated, up_to_date, skipped, failed) = self.counts();
        updated + up_to_date + skipped + failed
    }

    pub fn failed_repos(&self) -> Vec<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, repo)| repo.lock().unwrap().status.is_failed())
            .map(|(index, _)| index)
            .collect()
    }

    /// Navigate selection up, returns true if changed.
    pub fn nav_up(&mut self) -> bool {
        self.user_navigated = true;
        self.result_overlay = false;
        if self.selected > 0 {
            self.selected -= 1;
            true
        } else {
            false
        }
    }

    /// Navigate selection down, returns true if changed.
    pub fn nav_down(&mut self) -> bool {
        self.user_navigated = true;
        self.result_overlay = false;
        let max = self.list_len().saturating_sub(1);
        if self.selected < max {
            self.selected += 1;
            true
        } else {
            false
        }
    }

    pub fn nav_top(&mut self) {
        self.user_navigated = true;
        self.result_overlay = false;
        self.selected = 0;
    }

    pub fn nav_bottom(&mut self) {
        self.user_navigated = true;
        self.result_overlay = false;
        self.selected = self.list_len().saturating_sub(1);
    }

    /// Returns the repo index for the current selection, or None if Result is selected.
    pub fn selected_repo_index(&self) -> Option<usize> {
        let visible = self.visible_indices();
        if self.selected < visible.len() {
            Some(visible[self.selected])
        } else {
            None
        }
    }
}
