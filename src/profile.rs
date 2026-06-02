use std::time::Duration;

/// One repo's timing entry for the profile report.
pub struct ProfileRow {
    pub name: String,
    pub branch: String,
    pub status: &'static str,
    pub elapsed: Duration,
    /// Last non-empty captured git log line (the failure reason for stragglers).
    pub last_log_line: String,
}

/// True when profiling is enabled, honoring the `--profile` flag and the
/// `PULL_PROFILE` env var (any non-empty value enables it).
pub fn profile_enabled(flag: bool) -> bool {
    if flag {
        return true;
    }
    std::env::var("PULL_PROFILE")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

/// Format the profile report, sorted by elapsed descending (slowest first).
pub fn format_report(mut rows: Vec<ProfileRow>) -> String {
    rows.sort_by(|first, second| second.elapsed.cmp(&first.elapsed));

    let name_pad = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);

    let mut out = format!(
        "pull-all-tui profile — {} repos, slowest first\n",
        rows.len()
    );
    for row in &rows {
        let elapsed = format!("{:.2}s", row.elapsed.as_secs_f64());
        let last = truncate(row.last_log_line.trim(), 100);
        out.push_str(&format!(
            "  {elapsed:>8}  {status:<10}  {name:<name_pad$}  ({branch})  {last}\n",
            status = row.status,
            name = row.name,
            branch = row.branch,
        ));
    }
    out
}

fn truncate(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        line.to_string()
    } else {
        line.chars().take(max_chars).collect()
    }
}
