use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

use crate::app::{BranchInfo, BranchStats, DiffFile, RepoDetails, StashInfo, WorktreeInfo};

/// Branches excluded from the feature-branch count.
const EXCLUDED_BRANCHES: [&str; 2] = ["main", "dev"];

/// Result of parsing git pull output to determine status.
#[derive(Debug, PartialEq, Eq)]
pub enum PullOutcome {
    AlreadyUpToDate,
    Updated,
    /// The current branch has no upstream configured — not an error, nothing to pull.
    NoUpstream,
    /// The remote throttled us (HTTP 429 / rate limit / SSH connection throttling). Distinct
    /// from a plain failure so the UI can back off concurrency and retry, not just mark failed.
    Throttled,
    Failed,
}

/// Whether combined git output looks like remote throttling (rate limiting / connection
/// throttling) rather than a genuine failure. Checked on a lowercased copy.
fn looks_throttled(lower: &str) -> bool {
    const MARKERS: &[&str] = &[
        "too many requests",
        "rate limit",          // "rate limit exceeded", "API rate limit", "rate limited"
        "returned error: 429", // git/curl HTTP form
        "http/2 429",
        "http 429",
        "error: 429",
        "kex_exchange_identification", // ssh: server dropped the connection (often throttling)
        "connection reset by peer",
        "connection closed by remote host",
    ];
    // Avoid false positives like "429 objects" in progress output by requiring a real marker.
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Parse combined stdout+stderr from `git pull` to determine outcome.
/// `exit_success` — did the process exit with code 0?
pub fn classify_pull_output(output: &str, exit_success: bool) -> PullOutcome {
    if !exit_success {
        // A branch with no upstream isn't a failure — surface it as its own state. The
        // "no such ref was fetched" case is the same in spirit: the branch tracks a remote
        // ref that no longer exists (its PR was merged and the branch deleted), so there's
        // nothing to pull — gentler than a red failure on the Errors page.
        if output.contains("no tracking information")
            || output.contains("no upstream")
            || output.contains("There is no tracking information")
            || output.contains("no such ref was fetched")
        {
            return PullOutcome::NoUpstream;
        }
        if looks_throttled(&output.to_lowercase()) {
            return PullOutcome::Throttled;
        }
        return PullOutcome::Failed;
    }
    if output.contains("Already up to date") {
        PullOutcome::AlreadyUpToDate
    } else {
        PullOutcome::Updated
    }
}

/// Get the current branch for a repo directory.
pub async fn get_branch(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", dir.to_str().unwrap_or("."), "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if branch.is_empty() { "?".to_string() } else { branch })
}

/// Check if repo has uncommitted changes. Returns true if dirty.
pub async fn is_dirty(dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", dir.to_str().unwrap_or("."), "status", "--porcelain"])
        .output()
        .await?;
    Ok(!output.stdout.is_empty())
}

/// Count uncommitted changes (`git status --porcelain` lines). 0 when clean or on error.
pub async fn dirty_count(dir: &Path) -> u32 {
    match Command::new("git")
        .args(["-C", dir.to_str().unwrap_or("."), "status", "--porcelain"])
        .output()
        .await
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u32,
        Err(_) => 0,
    }
}

/// Get `git diff --stat --color=always HEAD@{1} HEAD` output.
pub async fn diff_stat(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-C",
            dir.to_str().unwrap_or("."),
            "diff",
            "--stat",
            "--color=always",
            "HEAD@{1}",
            "HEAD",
        ])
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Discover worktree entries from `<cwd>/<repo>.worktrees/*/.git`.
/// Returns Vec of (parent_repo_name, branch).
pub async fn discover_worktrees(cwd: &Path) -> Result<Vec<(String, String)>> {
    let mut results = Vec::new();

    let mut dir_iter = tokio::fs::read_dir(cwd).await?;
    let mut entries = Vec::new();
    while let Some(entry) = dir_iter.next_entry().await? {
        entries.push(entry);
    }

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.contains(".worktrees") {
            continue;
        }
        let wt_root = entry.path();
        if !wt_root.is_dir() {
            continue;
        }
        // Enumerate branches inside <repo>.worktrees/
        let mut wt_iter = match tokio::fs::read_dir(&wt_root).await {
            Ok(iter) => iter,
            Err(_) => continue,
        };
        while let Some(branch_entry) = wt_iter.next_entry().await? {
            let branch_dir = branch_entry.path();
            let git_dir = branch_dir.join(".git");
            if !git_dir.exists() {
                continue;
            }
            // repo name = everything before .worktrees in the directory name
            let repo_name = name
                .split(".worktrees")
                .next()
                .unwrap_or(&name)
                .to_string();
            let branch = get_branch(&branch_dir).await.unwrap_or_else(|_| "?".to_string());
            results.push((repo_name, branch));
        }
    }

    results.sort_by(|first, second| first.0.cmp(&second.0).then(first.1.cmp(&second.1)));
    Ok(results)
}

/// Directory names never descended into during the recursive scan (besides hidden dirs and
/// `*.worktrees`): heavy dependency/build dirs that never hold sibling repos worth pulling.
pub const PRUNE_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    ".venv",
    "venv",
    "bower_components",
    ".terraform",
];

/// Whether the recursive scan should descend into a child directory named `name`.
/// Hidden dirs (`.`-prefixed, including `.git`), `*.worktrees`, and `PRUNE_DIRS` are skipped.
pub fn should_descend(name: &str) -> bool {
    if name.starts_with('.') || name.contains(".worktrees") {
        return false;
    }
    !PRUNE_DIRS.contains(&name)
}

/// `path` rendered relative to `root` with `/` separators (e.g. "personal/pull-all").
/// Falls back to the full path when `path` isn't under `root`.
pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Recursively scan `root` for git repos (directories containing `.git`), streaming each found
/// path over the returned channel as soon as it's discovered. Pruned per `should_descend`; never
/// descends into a found repo (no nested-repo scan) and never treats `root` itself as a repo.
/// `max_depth` caps the descent: 1 = immediate subdirs only (the legacy single-level behavior).
/// The channel closes when the walk completes.
pub fn spawn_repo_walker(root: PathBuf, max_depth: usize) -> mpsc::UnboundedReceiver<PathBuf> {
    let (tx, rx) = mpsc::unbounded_channel();
    let semaphore = Arc::new(Semaphore::new(64));
    tokio::spawn(async move {
        walk_dir(root, 0, max_depth, tx, semaphore).await;
    });
    rx
}

/// One node of the recursive walk: emit `dir` if it's a repo (depth ≥ 1), else fan out into its
/// descendable children up to `max_depth`. Boxed because it recurses through an async fn.
fn walk_dir(
    dir: PathBuf,
    depth: usize,
    max_depth: usize,
    tx: mpsc::UnboundedSender<PathBuf>,
    semaphore: Arc<Semaphore>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        // The root (depth 0) is never itself a pull target — only its descendants are.
        if depth >= 1 && dir.join(".git").exists() {
            let _ = tx.send(dir);
            return;
        }
        if depth >= max_depth {
            return;
        }
        let entries = {
            let _permit = semaphore.acquire().await;
            let mut entries = Vec::new();
            if let Ok(mut iter) = tokio::fs::read_dir(&dir).await {
                while let Ok(Some(entry)) = iter.next_entry().await {
                    entries.push(entry);
                }
            }
            entries
        };
        let mut children = Vec::new();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if !should_descend(&name) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                children.push(walk_dir(
                    path,
                    depth + 1,
                    max_depth,
                    tx.clone(),
                    Arc::clone(&semaphore),
                ));
            }
        }
        futures::future::join_all(children).await;
    })
}

/// Collect every git repo under `root` (up to `max_depth`), sorted by path. The blocking
/// (collect-all) counterpart to `spawn_repo_walker`, used by the plain (`--no-tui`) path.
pub async fn discover_repos_recursive(root: &Path, max_depth: usize) -> Result<Vec<PathBuf>> {
    let mut rx = spawn_repo_walker(root.to_path_buf(), max_depth);
    let mut repos = Vec::new();
    while let Some(path) = rx.recv().await {
        repos.push(path);
    }
    repos.sort();
    Ok(repos)
}

/// Get the `origin` remote URL for a repo, normalized to a browsable https URL.
/// Returns None when there's no origin or the URL isn't a recognized git host form.
pub async fn get_remote_url(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", dir.to_str().unwrap_or("."), "remote", "get-url", "origin"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    normalize_remote_url(&raw)
}

/// Convert a git remote URL (scp-like, ssh, or http(s)) into a browsable https URL.
/// `git@github.com:org/repo.git` and `ssh://git@github.com/org/repo.git` both become
/// `https://github.com/org/repo`. Returns None for local paths or unknown forms.
pub fn normalize_remote_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let https = if let Some(rest) = raw.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        format!("https://{host}/{path}")
    } else if let Some(rest) = raw.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        format!("https://{rest}")
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        return None;
    };
    Some(https.strip_suffix(".git").unwrap_or(&https).to_string())
}

/// Parse the US (0x1f)-separated `git log -1 --format=%h%x1f%s%x1f%an%x1f%cr%x1f%ct` line
/// into (hash, subject, author, relative-date, committer-timestamp).
pub fn parse_commit_line(line: &str) -> (String, String, String, String, i64) {
    let line = line.trim_end_matches(['\n', '\r']);
    let mut parts = line.split('\u{1f}');
    (
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("").to_string(),
        parts.next().and_then(|value| value.trim().parse().ok()).unwrap_or(0),
    )
}

/// Parse `git rev-list --left-right --count @{u}...HEAD` output ("behind\tahead")
/// into (behind, ahead). Empty/garbage input yields (None, None).
pub fn parse_ahead_behind(text: &str) -> (Option<u32>, Option<u32>) {
    let mut nums = text.split_whitespace();
    let behind = nums.next().and_then(|value| value.parse().ok());
    let ahead = nums.next().and_then(|value| value.parse().ok());
    (behind, ahead)
}

/// Fetch the lazy info-panel details for one repo: last commit, ahead/behind vs
/// upstream, dirty file count, and stash count. Best-effort — failures leave defaults.
pub async fn get_repo_details(dir: &Path) -> RepoDetails {
    let dir_str = dir.to_str().unwrap_or(".");
    let mut details = RepoDetails::default();

    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "log", "-1", "--format=%h%x1f%s%x1f%an%x1f%cr%x1f%ct"])
        .output()
        .await
    {
        if output.status.success() {
            let line = String::from_utf8_lossy(&output.stdout);
            let (hash, subject, author, rel_date, timestamp) = parse_commit_line(&line);
            details.commit_hash = hash;
            details.commit_subject = subject;
            details.commit_author = author;
            details.commit_rel_date = rel_date;
            details.commit_timestamp = timestamp;
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "rev-list", "--left-right", "--count", "@{u}...HEAD"])
        .output()
        .await
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let (behind, ahead) = parse_ahead_behind(&text);
            details.behind = behind;
            details.ahead = ahead;
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "status", "--porcelain"])
        .output()
        .await
    {
        details.dirty_count = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u32;
    }

    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "stash", "list"])
        .output()
        .await
    {
        details.stash_count = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u32;
    }

    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output()
        .await
    {
        if output.status.success() {
            details.branch_count = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty() && !EXCLUDED_BRANCHES.contains(name))
                .count() as u32;
        }
    }

    details
}

/// Fetch a colored diff for the info panel: working-tree changes when `dirty`,
/// otherwise the most recent pull's diff (`HEAD@{1}..HEAD`). Returns its lines.
pub async fn get_diff(dir: &Path, dirty: bool) -> Vec<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let args: Vec<&str> = if dirty {
        vec!["-C", dir_str, "diff", "--color=always"]
    } else {
        vec!["-C", dir_str, "diff", "--color=always", "HEAD@{1}", "HEAD"]
    };
    let output = match Command::new("git").args(&args).output().await {
        Ok(output) => output,
        Err(_) => return vec!["(diff unavailable)".to_string()],
    };
    let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect();
    if lines.is_empty() {
        vec!["(no changes)".to_string()]
    } else {
        lines
    }
}

/// Run a git command and return its stdout as diff lines, with friendly placeholders for
/// empty output or failure.
async fn run_diff(args: &[&str]) -> Vec<String> {
    let output = match Command::new("git").args(args).output().await {
        Ok(output) => output,
        Err(_) => return vec!["(diff unavailable)".to_string()],
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return vec![if err.is_empty() {
            "(diff unavailable)".to_string()
        } else {
            format!("(diff failed: {err})")
        }];
    }
    let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect();
    if lines.is_empty() {
        vec!["(no changes)".to_string()]
    } else {
        lines
    }
}

/// List stash entries (`git stash list`), newest (`stash@{0}`) first.
pub async fn list_stashes(dir: &Path) -> Vec<StashInfo> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = match Command::new("git")
        .args(["-C", dir_str, "stash", "list", "--format=%gs"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, label)| StashInfo {
            index,
            label: label.to_string(),
        })
        .collect()
}

/// Resolve the repo's base branch ref: the remote's default branch (`origin/HEAD`) if set,
/// otherwise the first of origin/{main,master,dev} or local {main,master,dev} that exists.
pub async fn default_base_branch(dir: &Path) -> Option<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    if let Ok(output) = Command::new("git")
        .args(["-C", dir_str, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .await
    {
        if output.status.success() {
            let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !head.is_empty() {
                return Some(head);
            }
        }
    }
    for candidate in [
        "origin/main",
        "origin/master",
        "origin/dev",
        "main",
        "master",
        "dev",
    ] {
        let ok = Command::new("git")
            .args(["-C", dir_str, "rev-parse", "--verify", "--quiet", candidate])
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false);
        if ok {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Resolve the merge-base between the repo's default base branch and HEAD (the point a feature
/// branch forked from). None if no base branch is found.
pub async fn base_merge_base(dir: &Path) -> Option<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let base = default_base_branch(dir).await?;
    let output = Command::new("git")
        .args(["-C", dir_str, "merge-base", &base, "HEAD"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return Some(base);
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Merge-base of `branch` and the repo's default base branch (so a branch diff shows only what
/// the branch added since it diverged). Falls back to the base branch ref itself.
pub async fn branch_merge_base(dir: &Path, branch: &str) -> Option<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let base = default_base_branch(dir).await?;
    let output = Command::new("git")
        .args(["-C", dir_str, "merge-base", &base, branch])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return Some(base);
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Files a branch changed vs its base branch (`git diff --name-status <merge-base> <branch>`).
/// Works on any local branch without checking it out.
pub async fn branch_file_list(dir: &Path, branch: &str) -> Vec<DiffFile> {
    let dir_str = dir.to_str().unwrap_or(".");
    let Some(merge_base) = branch_merge_base(dir, branch).await else {
        return Vec::new();
    };
    let output = match Command::new("git")
        .args(["-C", dir_str, "diff", "--name-status", &merge_base, branch])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    parse_name_status(&String::from_utf8_lossy(&output.stdout))
}

/// Colored diff of a single file a branch changed vs its base branch.
pub async fn branch_file_diff(dir: &Path, branch: &str, path: &str) -> Vec<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let Some(merge_base) = branch_merge_base(dir, branch).await else {
        return vec!["(diff unavailable)".to_string()];
    };
    run_diff(&["-C", dir_str, "diff", "--color=always", &merge_base, branch, "--", path]).await
}

/// Count `--name-status` lines into (added, modified, deleted): A and untracked `?` are added,
/// D is deleted, and M/T plus renames/copies (R*/C*) count as modified. Pure for unit tests.
pub fn count_name_status(stdout: &str) -> (u32, u32, u32) {
    let (mut added, mut modified, mut deleted) = (0u32, 0u32, 0u32);
    for line in stdout.lines() {
        let Some(status) = line.split('\t').next().and_then(|field| field.chars().next()) else {
            continue;
        };
        match status.to_ascii_uppercase() {
            'A' | '?' => added += 1,
            'D' => deleted += 1,
            'M' | 'T' | 'R' | 'C' => modified += 1,
            _ => {}
        }
    }
    (added, modified, deleted)
}

/// Per-branch change stats vs the merge-base with `base` (one `git diff --name-status`). `None`
/// when the diff can't be produced. `merge_base` is resolved once per page and passed in.
pub async fn branch_diff_stats(dir: &Path, merge_base: &str, branch: &str) -> Option<BranchStats> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "diff", "--name-status", merge_base, branch])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let (added, modified, deleted) = count_name_status(&String::from_utf8_lossy(&output.stdout));
    Some(BranchStats { added, modified, deleted })
}

/// Merge-base sha of `branch` and an already-resolved base branch ref. Falls back to the ref.
pub async fn merge_base_with(dir: &Path, base: &str, branch: &str) -> Option<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "merge-base", base, branch])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return Some(base.to_string());
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse `--name-status` output (`STATUS\tPATH`, or `R100\tOLD\tNEW` for renames) into files.
fn parse_name_status(stdout: &str) -> Vec<DiffFile> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let status = fields.next()?.chars().next()?.to_string();
            // For renames/copies the new path is the last tab-separated field.
            let path = fields.next_back()?.to_string();
            if path.is_empty() {
                return None;
            }
            Some(DiffFile { status, path, untracked: false })
        })
        .collect()
}

/// The empty-tree object — diff against it to render an untracked/added file whole.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Files in a stash entry (`git stash show --include-untracked --name-status`), with the
/// untracked ones (stored in the stash's `^3` tree) flagged so their diff uses the right command.
pub async fn stash_file_list(dir: &Path, index: usize) -> Vec<DiffFile> {
    let dir_str = dir.to_str().unwrap_or(".");
    let stash_ref = format!("stash@{{{index}}}");
    let output = match Command::new("git")
        .args(["-C", dir_str, "stash", "show", "--include-untracked", "--name-status", &stash_ref])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let mut files = parse_name_status(&String::from_utf8_lossy(&output.stdout));

    // Untracked files captured by `git stash -u` live in the stash's third parent (`^3`).
    let untracked_ref = format!("stash@{{{index}}}^3");
    if let Ok(tree) = Command::new("git")
        .args(["-C", dir_str, "ls-tree", "-r", "--name-only", &untracked_ref])
        .output()
        .await
    {
        if tree.status.success() {
            let untracked: Vec<String> = String::from_utf8_lossy(&tree.stdout)
                .lines()
                .map(|line| line.to_string())
                .collect();
            for file in &mut files {
                if untracked.contains(&file.path) {
                    file.untracked = true;
                }
            }
        }
    }
    files
}

/// All uncommitted + untracked files (`git status --porcelain`), for the uncommitted diff view.
pub async fn uncommitted_file_list(dir: &Path) -> Vec<DiffFile> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = match Command::new("git")
        .args(["-C", dir_str, "status", "--porcelain", "--untracked-files=all"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let code = &line[..2];
            let untracked = code == "??";
            // Status char: the meaningful side of XY (index then worktree), or ? for untracked.
            let status = if untracked {
                "?".to_string()
            } else {
                code.trim().chars().next().unwrap_or('M').to_string()
            };
            let path = line[3..]
                .split_once(" -> ")
                .map(|(_, new)| new)
                .unwrap_or(&line[3..])
                .to_string();
            Some(DiffFile { status, path, untracked })
        })
        .collect()
}

/// Files changed since the branch forked from its base (`git diff --name-status <merge-base>`).
pub async fn base_file_list(dir: &Path) -> Vec<DiffFile> {
    let dir_str = dir.to_str().unwrap_or(".");
    let Some(merge_base) = base_merge_base(dir).await else {
        return Vec::new();
    };
    let output = match Command::new("git")
        .args(["-C", dir_str, "diff", "--name-status", &merge_base])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    parse_name_status(&String::from_utf8_lossy(&output.stdout))
}

/// Colored diff of a single file within a stash entry. Tracked files diff the stash against its
/// first parent (the state when stashed); untracked files diff the `^3` tree against the empty
/// tree (`git stash show -p` itself rejects a pathspec, so we go through `git diff`).
pub async fn stash_file_diff(dir: &Path, index: usize, path: &str, untracked: bool) -> Vec<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    if untracked {
        let untracked_tree = format!("stash@{{{index}}}^3");
        run_diff(&["-C", dir_str, "diff", "--color=always", EMPTY_TREE, &untracked_tree, "--", path])
            .await
    } else {
        let stash_ref = format!("stash@{{{index}}}");
        let parent = format!("stash@{{{index}}}^1");
        run_diff(&["-C", dir_str, "diff", "--color=always", &parent, &stash_ref, "--", path]).await
    }
}

/// Colored diff of a single file against `base` (a ref/sha), or against HEAD when `base` is None.
/// Untracked files (not in HEAD/base) are shown whole via `git diff --no-index`.
pub async fn file_diff_vs(dir: &Path, base: Option<&str>, path: &str, untracked: bool) -> Vec<String> {
    let dir_str = dir.to_str().unwrap_or(".");
    if untracked {
        let abs = dir.join(path);
        let abs_str = abs.to_str().unwrap_or(path);
        // --no-index exits 1 when files differ (always, here), so don't treat that as failure.
        let output = match Command::new("git")
            .args(["-C", dir_str, "diff", "--no-index", "--color=always", "/dev/null", abs_str])
            .output()
            .await
        {
            Ok(output) => output,
            Err(_) => return vec!["(diff unavailable)".to_string()],
        };
        let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.to_string())
            .collect();
        return if lines.is_empty() {
            vec!["(new file)".to_string()]
        } else {
            lines
        };
    }
    let reference = base.unwrap_or("HEAD");
    run_diff(&["-C", dir_str, "diff", "--color=always", reference, "--", path]).await
}

/// Run `git fetch --all` to refresh remote-tracking refs. Best-effort.
pub async fn fetch_remote(dir: &Path) -> Result<(), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "fetch", "--all", "--quiet"])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Parse a `%(upstream:track,nobracket)` value into (ahead, behind).
/// No upstream or `gone` → (None, None); present-but-current → (Some(0), Some(0)).
pub fn parse_track(upstream: &str, track: &str) -> (Option<u32>, Option<u32>) {
    if upstream.trim().is_empty() {
        return (None, None);
    }
    let track = track.trim();
    if track == "gone" {
        return (None, None);
    }
    if track.is_empty() {
        return (Some(0), Some(0));
    }
    let tokens: Vec<&str> = track
        .split([',', ' '])
        .filter(|token| !token.is_empty())
        .collect();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "ahead" => {
                ahead = tokens.get(index + 1).and_then(|value| value.parse().ok()).unwrap_or(0);
                index += 2;
            }
            "behind" => {
                behind = tokens.get(index + 1).and_then(|value| value.parse().ok()).unwrap_or(0);
                index += 2;
            }
            _ => index += 1,
        }
    }
    (Some(ahead), Some(behind))
}

/// Parse one US (0x1f)-separated `for-each-ref` line into a BranchInfo. Fields 6 (short sha)
/// and 7 (author) are tolerated as absent for forward/backward compatibility.
fn parse_branch_line(line: &str) -> Option<BranchInfo> {
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    if fields.len() < 6 || fields[1].is_empty() {
        return None;
    }
    let upstream = if fields[2].is_empty() {
        None
    } else {
        Some(fields[2].to_string())
    };
    let (ahead, behind) = parse_track(fields[2], fields[3]);
    Some(BranchInfo {
        is_head: fields[0] == "*",
        name: fields[1].to_string(),
        upstream,
        ahead,
        behind,
        last_commit_rel: fields[4].to_string(),
        subject: fields[5].to_string(),
        commit_sha: fields.get(6).map(|sha| sha.to_string()).unwrap_or_default(),
        author: fields.get(7).map(|author| author.to_string()).unwrap_or_default(),
        stats: None,
        merge_base_short: None,
    })
}

/// List local branches (most-recent first) with upstream, ahead/behind, last-commit date,
/// subject, short sha, and author.
pub async fn list_local_branches(dir: &Path) -> Vec<BranchInfo> {
    let dir_str = dir.to_str().unwrap_or(".");
    let format = "%(HEAD)%1f%(refname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(committerdate:relative)%1f%(contents:subject)%1f%(objectname:short)%1f%(authorname)";
    let output = match Command::new("git")
        .args([
            "-C",
            dir_str,
            "for-each-ref",
            "--sort=-committerdate",
            "--format",
            format,
            "refs/heads",
        ])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_branch_line)
        .collect()
}

/// Parse `git worktree list --porcelain` output into worktrees, skipping the main checkout
/// (path == `main_dir`) and detached/bare entries (no branch).
pub fn parse_worktree_porcelain(output: &str, main_dir: &Path) -> Vec<WorktreeInfo> {
    fn flush(
        path: &mut Option<PathBuf>,
        branch: &mut Option<String>,
        main_dir: &Path,
        out: &mut Vec<WorktreeInfo>,
    ) {
        if let (Some(found_path), Some(found_branch)) = (path.take(), branch.take()) {
            if found_path.as_path() != main_dir {
                out.push(WorktreeInfo {
                    branch: found_branch,
                    path: found_path,
                });
            }
        }
    }

    let mut result = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch, main_dir, &mut result);
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
        }
    }
    flush(&mut path, &mut branch, main_dir, &mut result);
    result
}

/// List worktrees for a repo (excluding the main checkout).
pub async fn list_worktrees(dir: &Path) -> Vec<WorktreeInfo> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = match Command::new("git")
        .args(["-C", dir_str, "worktree", "list", "--porcelain"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    parse_worktree_porcelain(&String::from_utf8_lossy(&output.stdout), dir)
}

/// Check out `branch` in the main worktree. Refuses if the tree is dirty.
pub async fn checkout_branch(dir: &Path, branch: &str) -> Result<(), String> {
    if is_dirty(dir).await.unwrap_or(false) {
        return Err("working tree has uncommitted changes".to_string());
    }
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "checkout", branch])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Delete `branch`: `git branch -d` (safe, refuses unmerged) or `-D` (force) when `force`.
pub async fn delete_branch(dir: &Path, branch: &str, force: bool) -> Result<(), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let flag = if force { "-D" } else { "-d" };
    let output = Command::new("git")
        .args(["-C", dir_str, "branch", flag, branch])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// The files contained in a stash entry (`git stash show --name-only stash@{index}`), relative
/// to the repo root. Used to show what a drop would throw away.
pub async fn stash_files(dir: &Path, index: usize) -> Result<Vec<String>, String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let stash_ref = format!("stash@{{{index}}}");
    let output = Command::new("git")
        .args([
            "-C",
            dir_str,
            "stash",
            "show",
            "--include-untracked",
            "--name-only",
            &stash_ref,
        ])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect())
}

/// Drop a stash entry (`git stash drop stash@{index}`).
pub async fn drop_stash(dir: &Path, index: usize) -> Result<(), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let stash_ref = format!("stash@{{{index}}}");
    let output = Command::new("git")
        .args(["-C", dir_str, "stash", "drop", &stash_ref])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Remove a worktree (`git worktree remove [--force] <path>`). Without `force`, git refuses
/// when the worktree has uncommitted/untracked changes or is locked.
pub async fn remove_worktree(dir: &Path, path: &Path, force: bool) -> Result<(), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let path_str = path.to_str().unwrap_or_default();
    let mut args = vec!["-C", dir_str, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path_str);
    let output = Command::new("git")
        .args(&args)
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// The working-tree changes a discard would touch: `restore` lists tracked files that
/// `reset --hard` would revert, `delete` lists untracked files that `clean -fd` would remove.
/// Both are paths relative to the repo root, parsed from `git status --porcelain`.
pub async fn discard_status(dir: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "status", "--porcelain", "--untracked-files=all"])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let mut restore = Vec::new();
    let mut delete = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let status = &line[..2];
        // Porcelain renames render as "R  old -> new"; the new path is what's on disk.
        let path = line[3..]
            .split_once(" -> ")
            .map(|(_, new)| new)
            .unwrap_or(&line[3..])
            .to_string();
        if status == "??" {
            delete.push(path);
        } else {
            restore.push(path);
        }
    }
    Ok((restore, delete))
}

/// Discard every uncommitted change in `dir`: `reset --hard` reverts tracked files and
/// `clean -fd` removes untracked files/dirs (ignored files are left in place).
pub async fn discard_changes(dir: &Path) -> Result<(), String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let reset = Command::new("git")
        .args(["-C", dir_str, "reset", "--hard"])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !reset.status.success() {
        return Err(String::from_utf8_lossy(&reset.stderr).trim().to_string());
    }
    let clean = Command::new("git")
        .args(["-C", dir_str, "clean", "-fd"])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if clean.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&clean.stderr).trim().to_string())
    }
}

/// Fast-forward the currently checked-out branch of `dir` to its upstream
/// (`git merge --ff-only @{u}`). Used for the repo HEAD and worktree-checked-out branches.
pub async fn pull_ff_only(dir: &Path) -> Result<PullOutcome, String> {
    let dir_str = dir.to_str().unwrap_or(".");
    let output = Command::new("git")
        .args(["-C", dir_str, "merge", "--ff-only", "@{u}"])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match classify_pull_output(&combined, output.status.success()) {
        PullOutcome::Failed => Err(combined.trim().to_string()),
        outcome => Ok(outcome),
    }
}

/// Fast-forward a non-checked-out local branch by fetching its upstream into it
/// (`git fetch <remote> <ref>:<local>`). The refspec only advances on fast-forward and
/// is rejected otherwise, so this can never clobber local commits. `upstream` is the
/// `origin/main`-style short upstream name.
pub async fn fetch_ff_branch(repo: &Path, upstream: &str, local: &str) -> Result<PullOutcome, String> {
    let Some((remote, remote_ref)) = upstream.split_once('/') else {
        return Err(format!("malformed upstream '{upstream}'"));
    };
    let dir_str = repo.to_str().unwrap_or(".");
    let refspec = format!("{remote_ref}:{local}");
    let output = Command::new("git")
        .args(["-C", dir_str, "fetch", remote, &refspec])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    // A no-op fetch prints nothing; an advancing one reports the ref update on stderr.
    let progress = String::from_utf8_lossy(&output.stderr);
    if progress.trim().is_empty() {
        Ok(PullOutcome::AlreadyUpToDate)
    } else {
        Ok(PullOutcome::Updated)
    }
}

/// Tally of a `pull_all_branches` pass over one repo.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PullAllSummary {
    pub updated: u32,
    pub up_to_date: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// Fast-forward every local branch of `repo` that can be advanced cleanly:
/// the HEAD and any worktree-checked-out branch via `merge --ff-only`, all other
/// branches via a fetch refspec. Branches with no upstream, already up to date, or
/// ahead/diverged (would not fast-forward) are left untouched.
pub async fn pull_all_branches(
    repo: &Path,
    branches: &[BranchInfo],
    worktrees: &[WorktreeInfo],
) -> PullAllSummary {
    let mut summary = PullAllSummary::default();
    for branch in branches {
        let Some(upstream) = branch.upstream.as_deref() else {
            summary.skipped += 1;
            continue;
        };
        let ahead = branch.ahead.unwrap_or(0);
        let behind = branch.behind.unwrap_or(0);
        if ahead > 0 {
            // Diverged or ahead-only — a fast-forward can't apply.
            summary.skipped += 1;
            continue;
        }
        if behind == 0 {
            summary.up_to_date += 1;
            continue;
        }
        let result = if branch.is_head {
            pull_ff_only(repo).await
        } else if let Some(worktree) = worktrees.iter().find(|wt| wt.branch == branch.name) {
            pull_ff_only(&worktree.path).await
        } else {
            fetch_ff_branch(repo, upstream, &branch.name).await
        };
        match result {
            Ok(PullOutcome::Updated) => summary.updated += 1,
            Ok(_) => summary.up_to_date += 1,
            Err(_) => summary.failed += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_already_up_to_date() {
        let output = "From github.com:org/repo\nAlready up to date.\n";
        assert_eq!(
            classify_pull_output(output, true),
            PullOutcome::AlreadyUpToDate
        );
    }

    #[test]
    fn test_classify_updated() {
        let output = "Updating abc1234..def5678\nFast-forward\n src/foo.ts | 12 +++\n";
        assert_eq!(classify_pull_output(output, true), PullOutcome::Updated);
    }

    #[test]
    fn test_classify_failed_nonzero_exit() {
        let output = "Already up to date.\n";
        // Even if the text says "up to date", non-zero exit means failed
        assert_eq!(
            classify_pull_output(output, false),
            PullOutcome::Failed
        );
    }

    #[test]
    fn test_classify_failed_exit_error_output() {
        let output = "error: Your local changes would be overwritten by merge.\n";
        assert_eq!(classify_pull_output(output, false), PullOutcome::Failed);
    }

    #[test]
    fn test_classify_no_upstream_is_not_failure() {
        let output = "There is no tracking information for the current branch.\n\
            Please specify which branch you want to merge with.\n";
        assert_eq!(classify_pull_output(output, false), PullOutcome::NoUpstream);
    }

    #[test]
    fn test_classify_deleted_upstream_ref_is_no_upstream() {
        // The tracked remote branch was deleted (e.g. PR merged) — not a hard failure.
        let output = "Your configuration specifies to merge with the ref \
            'refs/heads/chore/FEP-61-remove-actions-doc-workflow'\n\
            from the remote, but no such ref was fetched.\n";
        assert_eq!(classify_pull_output(output, false), PullOutcome::NoUpstream);
    }

    #[test]
    fn test_classify_updated_no_already_up_to_date_text() {
        let output = "From github.com:org/repo\n   abc1234..def5678  dev -> origin/dev\n";
        assert_eq!(classify_pull_output(output, true), PullOutcome::Updated);
    }

    #[test]
    fn test_classify_already_up_to_date_case_sensitive() {
        // The bash script does `grep -q "Already up to date"`
        let output = "already up to date.\n";
        // lowercase → classified as Updated (no exact match)
        assert_eq!(classify_pull_output(output, true), PullOutcome::Updated);
    }

    #[test]
    fn test_classify_table_data() {
        let cases: &[(&str, bool, PullOutcome)] = &[
            ("Already up to date.\n", true, PullOutcome::AlreadyUpToDate),
            ("Already up to date.\n", false, PullOutcome::Failed),
            ("Updating abc..def\nFast-forward\n", true, PullOutcome::Updated),
            ("error: cannot lock ref\n", false, PullOutcome::Failed),
            ("", false, PullOutcome::Failed),
            ("", true, PullOutcome::Updated),
        ];

        for (output, exit_success, expected) in cases {
            assert_eq!(
                classify_pull_output(output, *exit_success),
                *expected,
                "classify_pull_output({output:?}, {exit_success}) should be {expected:?}"
            );
        }
    }

    #[test]
    fn normalize_remote_url_handles_all_forms() {
        assert_eq!(
            normalize_remote_url("git@github.com:org/repo.git").as_deref(),
            Some("https://github.com/org/repo")
        );
        assert_eq!(
            normalize_remote_url("https://github.com/org/repo.git").as_deref(),
            Some("https://github.com/org/repo")
        );
        assert_eq!(
            normalize_remote_url("https://github.com/org/repo").as_deref(),
            Some("https://github.com/org/repo")
        );
        assert_eq!(
            normalize_remote_url("ssh://git@github.com/org/repo.git").as_deref(),
            Some("https://github.com/org/repo")
        );
        assert_eq!(normalize_remote_url(""), None);
        assert_eq!(normalize_remote_url("/local/path/repo"), None);
    }

    #[test]
    fn parse_commit_line_splits_us_fields() {
        let line =
            "a1b2c3d\u{1f}fix: handle empty input\u{1f}Ada Byron\u{1f}2 hours ago\u{1f}1700000000\n";
        let (hash, subject, author, rel, timestamp) = parse_commit_line(line);
        assert_eq!(hash, "a1b2c3d");
        assert_eq!(subject, "fix: handle empty input");
        assert_eq!(author, "Ada Byron");
        assert_eq!(rel, "2 hours ago");
        assert_eq!(timestamp, 1_700_000_000);
    }

    #[test]
    fn parse_commit_line_tolerates_missing_fields() {
        let (hash, subject, author, rel, timestamp) = parse_commit_line("deadbee");
        assert_eq!(hash, "deadbee");
        assert_eq!(subject, "");
        assert_eq!(author, "");
        assert_eq!(rel, "");
        assert_eq!(timestamp, 0);
    }

    #[test]
    fn parse_ahead_behind_reads_behind_then_ahead() {
        assert_eq!(parse_ahead_behind("3\t5\n"), (Some(3), Some(5)));
        assert_eq!(parse_ahead_behind("0\t0\n"), (Some(0), Some(0)));
        assert_eq!(parse_ahead_behind(""), (None, None));
    }

    #[test]
    fn parse_track_covers_upstream_states() {
        // No upstream → unknown.
        assert_eq!(parse_track("", ""), (None, None));
        // Upstream present, in sync.
        assert_eq!(parse_track("origin/main", ""), (Some(0), Some(0)));
        // Deleted upstream.
        assert_eq!(parse_track("origin/gone", "gone"), (None, None));
        // One-sided and two-sided.
        assert_eq!(parse_track("origin/main", "ahead 2"), (Some(2), Some(0)));
        assert_eq!(parse_track("origin/main", "behind 3"), (Some(0), Some(3)));
        assert_eq!(parse_track("origin/main", "ahead 1, behind 4"), (Some(1), Some(4)));
    }

    #[test]
    fn parse_branch_line_splits_us_fields() {
        let line = "*\u{1f}main\u{1f}origin/main\u{1f}ahead 1\u{1f}3 days ago\u{1f}init repo\u{1f}abc1234\u{1f}Ada";
        let branch = parse_branch_line(line).expect("parses");
        assert!(branch.is_head);
        assert_eq!(branch.name, "main");
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((branch.ahead, branch.behind), (Some(1), Some(0)));
        assert_eq!(branch.last_commit_rel, "3 days ago");
        assert_eq!(branch.subject, "init repo");
        assert_eq!(branch.commit_sha, "abc1234");
        assert_eq!(branch.author, "Ada");
        assert_eq!(branch.stats, None);
    }

    #[test]
    fn parse_branch_line_tolerates_missing_sha_author() {
        // Old six-field format (no sha/author) still parses, with empty extras.
        let line = "*\u{1f}main\u{1f}origin/main\u{1f}ahead 1\u{1f}3 days ago\u{1f}init repo";
        let branch = parse_branch_line(line).expect("parses");
        assert_eq!(branch.commit_sha, "");
        assert_eq!(branch.author, "");
    }

    #[test]
    fn count_name_status_buckets_letters() {
        let stdout = "M\tsrc/a.rs\nA\tsrc/b.rs\nD\tsrc/c.rs\nR100\told.rs\tnew.rs\n?\tuntracked.rs\nC75\tx.rs\ty.rs\n";
        // M + R + C = 3 modified; A + ? = 2 added; D = 1 deleted.
        assert_eq!(count_name_status(stdout), (2, 3, 1));
    }

    #[test]
    fn classify_detects_throttling_distinct_from_failure() {
        for output in [
            "fatal: unable to access '...': The requested URL returned error: 429\n",
            "remote: You have exceeded a secondary rate limit.\nfatal: rate limit\n",
            "error: RPC failed; HTTP 429 curl 22\n",
            "kex_exchange_identification: Connection closed by remote host\n",
            "fatal: Could not read from remote repository\nConnection reset by peer\n",
        ] {
            assert_eq!(
                classify_pull_output(output, false),
                PullOutcome::Throttled,
                "should be Throttled: {output:?}"
            );
        }
    }

    #[test]
    fn classify_does_not_mistake_progress_or_plain_failure_for_throttling() {
        // "429 objects" in progress is not throttling.
        assert_eq!(
            classify_pull_output("Receiving objects: 100% (429/429)\n", false),
            PullOutcome::Failed
        );
        assert_eq!(
            classify_pull_output("error: Your local changes would be overwritten\n", false),
            PullOutcome::Failed
        );
        // A throttle marker on a SUCCESSFUL pull is still Updated (exit 0 wins).
        assert_eq!(classify_pull_output("rate limit note\nFast-forward\n", true), PullOutcome::Updated);
    }

    #[test]
    fn should_descend_skips_hidden_pruned_and_worktrees() {
        assert!(should_descend("projects"));
        assert!(should_descend("my-repo"));
        assert!(!should_descend(".git"));
        assert!(!should_descend(".cache"));
        assert!(!should_descend("node_modules"));
        assert!(!should_descend("target"));
        assert!(!should_descend("vendor"));
        assert!(!should_descend("my-repo.worktrees"));
    }

    #[test]
    fn relative_path_renders_under_root() {
        let root = std::path::Path::new("/home/me/projects");
        assert_eq!(relative_path(root, std::path::Path::new("/home/me/projects/a")), "a");
        assert_eq!(
            relative_path(root, std::path::Path::new("/home/me/projects/personal/pull-all")),
            "personal/pull-all"
        );
        // Not under root → full path is kept.
        assert_eq!(relative_path(root, std::path::Path::new("/elsewhere/x")), "/elsewhere/x");
    }

    /// Build a throwaway directory tree under a unique temp dir; returns its root for cleanup.
    fn build_tree(dirs_with_git: &[&str], plain_dirs: &[&str]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("pull-all-walk-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in plain_dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for dir in dirs_with_git {
            std::fs::create_dir_all(root.join(dir).join(".git")).unwrap();
        }
        root
    }

    #[tokio::test]
    async fn walker_finds_nested_repos_and_prunes() {
        let root = build_tree(
            &[
                "a",                 // depth 1 repo
                "b/c",               // depth 2 repo
                "d/e/f",             // depth 3 repo
                "a/nested",          // inside repo a — must NOT be found (no descent into repos)
                "node_modules/dep",  // pruned dir — must NOT be found
                ".hidden/repo",      // hidden dir — must NOT be found
                "g.worktrees/feat",  // worktree dir — must NOT be found
            ],
            &[],
        );
        let mut found = discover_repos_recursive(&root, 16).await.unwrap();
        found.sort();
        let rels: Vec<String> = found.iter().map(|path| relative_path(&root, path)).collect();
        assert_eq!(rels, vec!["a", "b/c", "d/e/f"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn walker_depth_cap_limits_descent() {
        let root = build_tree(&["a", "b/c", "d/e/f"], &[]);
        let found = discover_repos_recursive(&root, 1).await.unwrap();
        let rels: Vec<String> = found.iter().map(|path| relative_path(&root, path)).collect();
        // depth 1 only: just the immediate-child repo.
        assert_eq!(rels, vec!["a"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_worktree_porcelain_skips_main_and_detached() {
        let output = "\
worktree /repo
HEAD aaaa
branch refs/heads/main

worktree /repo.worktrees/feature
HEAD bbbb
branch refs/heads/feature

worktree /repo.worktrees/detached
HEAD cccc
detached
";
        let worktrees = parse_worktree_porcelain(output, std::path::Path::new("/repo"));
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch, "feature");
        assert_eq!(worktrees[0].path, std::path::PathBuf::from("/repo.worktrees/feature"));
    }
}
