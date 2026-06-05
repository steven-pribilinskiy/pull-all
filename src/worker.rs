use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::app::{
    AppState, PageRow, PageRowKind, RepoPageData, RepoStatus, SharedRepoState, WorktreeEntry,
};
use crate::git::{
    checkout_branch, classify_pull_output, delete_branch, diff_stat, discover_worktrees,
    fetch_ff_branch, fetch_remote, get_branch, get_diff, get_remote_url, get_repo_details,
    is_dirty, list_local_branches, list_worktrees, pull_all_branches, pull_ff_only, PullOutcome,
};

/// Pull a single repository, updating `repo_state` as progress arrives.
/// Signals completion via the state's status field.
pub async fn pull_repo(
    repo_state: SharedRepoState,
    semaphore: Arc<Semaphore>,
    timeout_secs: u64,
) -> Result<()> {
    let _permit = semaphore.acquire_owned().await?;

    let started = std::time::Instant::now();
    let (path, name) = {
        let mut state = repo_state.lock().unwrap();
        state.start = Some(started);
        state.elapsed = None;
        (state.path.clone(), state.name.clone())
    };

    // Get branch before anything else
    let branch = get_branch(&path).await.unwrap_or_else(|_| "?".to_string());
    {
        let mut state = repo_state.lock().unwrap();
        state.branch = Some(branch);
    }

    // Check for dirty state
    let dirty = is_dirty(&path).await.unwrap_or(false);
    if dirty {
        let mut state = repo_state.lock().unwrap();
        state.elapsed = Some(std::time::Duration::ZERO);
        state.status = RepoStatus::Skipped;
        state
            .log
            .push(format!("⊘ Skipping {name} (has uncommitted changes)"));
        return Ok(());
    }

    // Spawn git pull and track PID
    let mut child = Command::new("timeout")
        .args([
            &timeout_secs.to_string(),
            "git",
            "-C",
            path.to_str().unwrap_or("."),
            "pull",
            "--ff-only",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let pid = child.id().unwrap_or(0);
    {
        let mut state = repo_state.lock().unwrap();
        state.status = RepoStatus::Running { pid };
    }

    // Stream stdout
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let repo_state_stdout = Arc::clone(&repo_state);
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut collected = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push_str(&line);
            collected.push('\n');
            let mut state = repo_state_stdout.lock().unwrap();
            state.log.push(line);
        }
        collected
    });

    let repo_state_stderr = Arc::clone(&repo_state);
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut collected = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push_str(&line);
            collected.push('\n');
            let mut state = repo_state_stderr.lock().unwrap();
            state.log.push(line);
        }
        collected
    });

    let status = child.wait().await?;
    let exit_success = status.success();

    let stdout_output = stdout_task.await.unwrap_or_default();
    let stderr_output = stderr_task.await.unwrap_or_default();
    let combined = format!("{stdout_output}{stderr_output}");

    let outcome = classify_pull_output(&combined, exit_success);

    let elapsed = started.elapsed();
    match outcome {
        PullOutcome::AlreadyUpToDate => {
            let mut state = repo_state.lock().unwrap();
            state.elapsed = Some(elapsed);
            state.status = RepoStatus::UpToDate;
        }
        PullOutcome::Updated => {
            // Append diff stat to log
            let stat = diff_stat(&path).await.unwrap_or_default();
            if !stat.is_empty() {
                let mut state = repo_state.lock().unwrap();
                for line in stat.lines() {
                    state.log.push(line.to_string());
                }
            }
            let mut state = repo_state.lock().unwrap();
            state.elapsed = Some(started.elapsed());
            state.status = RepoStatus::Updated;
        }
        PullOutcome::Failed => {
            let mut state = repo_state.lock().unwrap();
            state.elapsed = Some(elapsed);
            state.status = RepoStatus::Failed;
        }
    }

    Ok(())
}

/// Discover worktrees and update app_state when done.
pub async fn run_worktree_discovery(app_state: Arc<Mutex<AppState>>, cwd: std::path::PathBuf) {
    let entries = discover_worktrees(&cwd).await.unwrap_or_default();

    let worktrees: Vec<WorktreeEntry> = entries
        .into_iter()
        .map(|(repo, branch)| WorktreeEntry { repo, branch })
        .collect();

    let mut state = app_state.lock().unwrap();
    state.worktrees = worktrees;
    state.worktrees_done = true;
}

/// Run all pulls concurrently (up to `max_jobs` at a time).
pub async fn run_all_pulls(
    repos: Vec<SharedRepoState>,
    max_jobs: usize,
    timeout_secs: u64,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let mut handles = Vec::new();

    for repo_state in repos {
        let semaphore = Arc::clone(&semaphore);
        let handle = tokio::spawn(pull_repo(repo_state, semaphore, timeout_secs));
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// Fetch each repo's `origin` remote URL concurrently and store it on the repo state,
/// so the help modal can offer clickable links. Best-effort: failures leave `remote_url` None.
pub async fn run_remote_url_discovery(repos: Vec<SharedRepoState>, max_jobs: usize) {
    let semaphore = Arc::new(Semaphore::new(max_jobs.max(1)));
    let mut handles = Vec::new();

    for repo_state in repos {
        let semaphore = Arc::clone(&semaphore);
        let handle = tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok();
            let path = { repo_state.lock().unwrap().path.clone() };
            if let Some(url) = get_remote_url(&path).await {
                repo_state.lock().unwrap().remote_url = Some(url);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }
}

/// Fetch the info-panel details for one repo (last commit, ahead/behind, dirty/stash counts)
/// and store them. The caller sets `details_loading` before spawning; this clears it.
pub async fn run_repo_details(repo: SharedRepoState) {
    let path = { repo.lock().unwrap().path.clone() };
    let details = get_repo_details(&path).await;
    let mut state = repo.lock().unwrap();
    state.details = Some(details);
    state.details_loading = false;
}

/// Fetch the diff for one repo (working-tree changes if dirty, else the last pull's diff)
/// and store it in the transient diff buffer for the Diff view.
pub async fn run_repo_diff(repo: SharedRepoState) {
    let path = { repo.lock().unwrap().path.clone() };
    let dirty = is_dirty(&path).await.unwrap_or(false);
    let diff = get_diff(&path, dirty).await;
    repo.lock().unwrap().diff = Some(diff);
}

/// Populate the dedicated repo page: show branches/worktrees immediately, then `git fetch`
/// and refresh ahead/behind. Caller sets `page_loading`; this clears it.
pub async fn run_repo_page(repo: SharedRepoState) {
    let path = { repo.lock().unwrap().path.clone() };

    let branches = list_local_branches(&path).await;
    let worktrees = list_worktrees(&path).await;
    {
        let mut state = repo.lock().unwrap();
        state.page = Some(RepoPageData {
            branches,
            worktrees: worktrees.clone(),
            fetched: false,
            fetch_error: None,
        });
    }

    let fetch = fetch_remote(&path).await;
    let branches = list_local_branches(&path).await;
    let mut state = repo.lock().unwrap();
    state.page = Some(RepoPageData {
        branches,
        worktrees,
        fetched: true,
        fetch_error: fetch.err(),
    });
    state.page_loading = false;
}

/// Check out a branch in a repo's main worktree, set a result banner, and reload its page.
pub async fn run_checkout(app_state: Arc<Mutex<AppState>>, repo_idx: usize, branch: String) {
    let path = { app_state.lock().unwrap().repos[repo_idx].lock().unwrap().path.clone() };
    let result = checkout_branch(&path, &branch).await;
    let mut app = app_state.lock().unwrap();
    app.repo_page_message = Some(match result {
        Ok(()) => format!("Checked out {branch}"),
        Err(err) => format!("checkout failed: {err}"),
    });
    app.repos[repo_idx].lock().unwrap().page = None;
}

/// Delete a branch (safe `-d`), set a result banner, and reload the repo's page.
pub async fn run_delete(app_state: Arc<Mutex<AppState>>, repo_idx: usize, branch: String) {
    let path = { app_state.lock().unwrap().repos[repo_idx].lock().unwrap().path.clone() };
    let result = delete_branch(&path, &branch).await;
    let mut app = app_state.lock().unwrap();
    app.repo_page_message = Some(match result {
        Ok(()) => format!("Deleted {branch}"),
        Err(err) => format!("delete failed: {err}"),
    });
    app.repos[repo_idx].lock().unwrap().page = None;
}

/// Fast-forward the selected repo-page row (a single branch or worktree), set a result
/// banner, and reload the page so ahead/behind refresh.
pub async fn run_pull_branch(app_state: Arc<Mutex<AppState>>, repo_idx: usize, row: PageRow) {
    let (path, worktrees) = {
        let app = app_state.lock().unwrap();
        let repo = app.repos[repo_idx].lock().unwrap();
        let worktrees = repo
            .page
            .as_ref()
            .map(|page| page.worktrees.clone())
            .unwrap_or_default();
        (repo.path.clone(), worktrees)
    };

    let result = match row.kind {
        PageRowKind::Worktree => pull_ff_only(&row.path).await,
        PageRowKind::Branch => {
            if row.is_head {
                pull_ff_only(&path).await
            } else if let Some(worktree) = worktrees.iter().find(|wt| wt.branch == row.branch) {
                pull_ff_only(&worktree.path).await
            } else if let Some(upstream) = row.upstream.as_deref() {
                fetch_ff_branch(&path, upstream, &row.branch).await
            } else {
                Err(format!("'{}' has no upstream", row.branch))
            }
        }
    };

    let mut app = app_state.lock().unwrap();
    app.repo_page_message = Some(match result {
        Ok(PullOutcome::Updated) => format!("Pulled {}", row.branch),
        Ok(_) => format!("{} already up to date", row.branch),
        Err(err) => format!("pull failed: {err}"),
    });
    app.repos[repo_idx].lock().unwrap().page = None;
}

/// Fast-forward every fast-forwardable local branch of the repo, set a summary banner,
/// and reload the page.
pub async fn run_pull_all_branches(app_state: Arc<Mutex<AppState>>, repo_idx: usize) {
    let Some((path, branches, worktrees)) = ({
        let app = app_state.lock().unwrap();
        let repo = app.repos[repo_idx].lock().unwrap();
        repo.page.as_ref().map(|page| {
            (repo.path.clone(), page.branches.clone(), page.worktrees.clone())
        })
    }) else {
        return;
    };

    let summary = pull_all_branches(&path, &branches, &worktrees).await;
    let failed = if summary.failed > 0 {
        format!(", {} failed", summary.failed)
    } else {
        String::new()
    };

    let mut app = app_state.lock().unwrap();
    app.repo_page_message = Some(format!(
        "Pulled: {} updated, {} up-to-date, {} skipped{failed}",
        summary.updated, summary.up_to_date, summary.skipped
    ));
    app.repos[repo_idx].lock().unwrap().page = None;
}

/// Fetch info-panel details for all repos that don't have them yet (background column fill).
pub async fn run_all_details(repos: Vec<SharedRepoState>, max_jobs: usize) {
    let semaphore = Arc::new(Semaphore::new(max_jobs.max(1)));
    let mut handles = Vec::new();
    for repo in repos {
        let semaphore = Arc::clone(&semaphore);
        handles.push(tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok();
            if repo.lock().unwrap().details.is_some() {
                return;
            }
            let path = { repo.lock().unwrap().path.clone() };
            let details = get_repo_details(&path).await;
            repo.lock().unwrap().details = Some(details);
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

