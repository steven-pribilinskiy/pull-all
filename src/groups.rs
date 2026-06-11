use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::app::AppState;

/// Groups with more members than this get a collapsible (selectable) header by default.
pub const DEFAULT_COLLAPSE_THRESHOLD: usize = 5;
/// Default freshness window for dynamic (command/url) membership, in minutes.
pub const DEFAULT_CACHE_TTL_MINUTES: u64 = 1440;
/// Per-source resolution timeout.
const RESOLVE_TIMEOUT_SECS: u64 = 10;

/// User-edited group definitions at `~/.config/pull-all/groups.json`. Optional — when the file
/// is missing, grouping is inert. Never written by the app (unlike `state.json`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GroupsConfig {
    /// Groups with more members than this get collapsible headers; 0 = use the default.
    pub collapse_threshold: usize,
    /// Dynamic-source cache freshness in minutes; 0 = use the default.
    pub cache_ttl_minutes: u64,
    pub groups: Vec<GroupDef>,
}

impl GroupsConfig {
    pub fn collapse_threshold(&self) -> usize {
        if self.collapse_threshold == 0 {
            DEFAULT_COLLAPSE_THRESHOLD
        } else {
            self.collapse_threshold
        }
    }

    pub fn cache_ttl_minutes(&self) -> u64 {
        if self.cache_ttl_minutes == 0 {
            DEFAULT_CACHE_TTL_MINUTES
        } else {
            self.cache_ttl_minutes
        }
    }
}

/// One group definition: a name plus exactly one membership source
/// (`pattern` / `repos` / `command` / `url`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GroupDef {
    pub name: String,
    /// `*`-wildcard match on repo names, e.g. `"mfe-*"`.
    pub pattern: Option<String>,
    /// Explicit repo-name list.
    pub repos: Option<Vec<String>>,
    /// Shell command whose stdout lines are repo names.
    pub command: Option<String>,
    /// URL of a JSON document; member names extracted per `extract`.
    pub url: Option<String>,
    /// How to pull names out of the JSON at `url` (default: keys of the document root).
    pub extract: Option<ExtractSpec>,
}

impl GroupDef {
    /// The group's membership source, enforcing exactly-one-of pattern/repos/command/url.
    pub fn source(&self) -> Result<GroupSource, String> {
        if self.name.trim().is_empty() {
            return Err("group with an empty name".to_string());
        }
        let mut sources: Vec<GroupSource> = Vec::new();
        if let Some(pattern) = &self.pattern {
            sources.push(GroupSource::Pattern(pattern.clone()));
        }
        if let Some(repos) = &self.repos {
            sources.push(GroupSource::Repos(repos.clone()));
        }
        if let Some(command) = &self.command {
            sources.push(GroupSource::Command(command.clone()));
        }
        if let Some(url) = &self.url {
            sources.push(GroupSource::Url {
                url: url.clone(),
                extract: self.extract.clone().unwrap_or_default(),
            });
        }
        match sources.len() {
            1 => Ok(sources.remove(0)),
            0 => Err(format!("group '{}': needs one of pattern/repos/command/url", self.name)),
            _ => Err(format!("group '{}': exactly one of pattern/repos/command/url", self.name)),
        }
    }
}

/// A validated membership source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupSource {
    Pattern(String),
    Repos(Vec<String>),
    Command(String),
    Url { url: String, extract: ExtractSpec },
}

impl GroupSource {
    /// Whether membership comes from an external source resolved asynchronously.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, GroupSource::Command(_) | GroupSource::Url { .. })
    }

    /// Cache-invalidation key: a cached entry whose fingerprint no longer matches the
    /// (possibly edited) config is ignored. Empty for static sources (never cached).
    pub fn fingerprint(&self) -> String {
        match self {
            GroupSource::Command(command) => format!("command:{command}"),
            GroupSource::Url { url, extract } => {
                format!("url:{url}:{}:{:?}", extract.pointer, extract.kind)
            }
            _ => String::new(),
        }
    }

    /// Short source-kind label for the group preview.
    pub fn kind_label(&self) -> &'static str {
        match self {
            GroupSource::Pattern(_) => "pattern",
            GroupSource::Repos(_) => "static list",
            GroupSource::Command(_) => "command",
            GroupSource::Url { .. } => "url",
        }
    }

    /// The source's defining string for the group preview.
    pub fn detail(&self) -> String {
        match self {
            GroupSource::Pattern(pattern) => pattern.clone(),
            GroupSource::Repos(repos) => format!("{} repos", repos.len()),
            GroupSource::Command(command) => command.clone(),
            GroupSource::Url { url, .. } => url.clone(),
        }
    }
}

/// What to extract from the JSON document fetched from a `url` source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ExtractSpec {
    /// RFC 6901 JSON pointer to the node holding the names; `""` = document root.
    pub pointer: String,
    pub kind: ExtractKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractKind {
    /// Member names = keys of the object at the pointer.
    #[default]
    Keys,
    /// Member names = string entries of the array (or string values of the object) at the pointer.
    Values,
}

/// Case-insensitive `*`-wildcard match (`*` matches any run, anywhere, any number of times).
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    let mut p = 0usize;
    let mut n = 0usize;
    let mut star: Option<usize> = None;
    let mut star_n = 0usize;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_n = n;
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some(star_p) = star {
            p = star_p + 1;
            star_n += 1;
            n = star_n;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Pull member names out of a fetched JSON document per the extract spec.
pub fn extract_members(
    json: &serde_json::Value,
    spec: &ExtractSpec,
) -> Result<Vec<String>, String> {
    let node = json
        .pointer(&spec.pointer)
        .ok_or_else(|| format!("JSON pointer '{}' not found", spec.pointer))?;
    match spec.kind {
        ExtractKind::Keys => match node {
            serde_json::Value::Object(map) => Ok(map.keys().cloned().collect()),
            _ => Err(format!("node at '{}' is not an object (kind=keys)", spec.pointer)),
        },
        ExtractKind::Values => match node {
            serde_json::Value::Array(entries) => Ok(entries
                .iter()
                .filter_map(|entry| entry.as_str().map(String::from))
                .collect()),
            serde_json::Value::Object(map) => Ok(map
                .values()
                .filter_map(|value| value.as_str().map(String::from))
                .collect()),
            _ => Err(format!("node at '{}' is not an array or object (kind=values)", spec.pointer)),
        },
    }
}

/// Resolved dynamic-source membership cached at `~/.config/pull-all/groups-cache.json`
/// (auto-written, like `state.json`) so grouped startups don't wait on commands/fetches.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupsCache {
    pub entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Unix seconds when the source was last resolved.
    pub resolved_at: u64,
    /// `GroupSource::fingerprint()` at resolve time — a mismatch means the config changed.
    pub fingerprint: String,
    pub members: Vec<String>,
}

/// Whether a membership resolved at `resolved_at` (unix seconds) is still within the TTL.
pub fn resolved_fresh(resolved_at: u64, now: u64, ttl_minutes: u64) -> bool {
    now.saturating_sub(resolved_at) < ttl_minutes * 60
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("pull-all").join("groups.json"))
}

fn cache_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("pull-all").join("groups-cache.json"))
}

/// Load the group config. A missing file is normal (no groups); a malformed file degrades to
/// no groups with the parse error returned for a toast.
pub fn load_config() -> (GroupsConfig, Option<String>) {
    let Some(path) = config_path() else {
        return (GroupsConfig::default(), None);
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(config) => (config, None),
            Err(err) => (GroupsConfig::default(), Some(format!("groups.json: {err}"))),
        },
        Err(_) => (GroupsConfig::default(), None),
    }
}

/// Load the dynamic-membership cache, falling back to empty on any error.
pub fn load_cache() -> GroupsCache {
    let Some(path) = cache_path() else {
        return GroupsCache::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => GroupsCache::default(),
    }
}

/// Persist the dynamic-membership cache, best-effort (errors are ignored).
pub fn save_cache(cache: &GroupsCache) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(contents) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(&path, contents);
    }
}

/// Resolve a dynamic source to member names (static sources resolve to nothing here).
async fn resolve_source(source: &GroupSource) -> Result<Vec<String>, String> {
    match source {
        GroupSource::Command(command) => {
            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .output()
                .await
                .map_err(|err| format!("command failed to start: {err}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let first_line = stderr.trim().lines().next().unwrap_or("").to_string();
                return Err(format!("command exited with {}: {first_line}", output.status));
            }
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from)
                .collect())
        }
        GroupSource::Url { url, extract } => {
            let response = reqwest::get(url)
                .await
                .map_err(|err| format!("fetch failed: {err}"))?
                .error_for_status()
                .map_err(|err| format!("fetch failed: {err}"))?;
            let text = response
                .text()
                .await
                .map_err(|err| format!("fetch failed: {err}"))?;
            let json: serde_json::Value =
                serde_json::from_str(&text).map_err(|err| format!("invalid JSON: {err}"))?;
            extract_members(&json, extract)
        }
        _ => Ok(Vec::new()),
    }
}

/// Resolve every dynamic group that is unresolved or past its TTL (or all of them when
/// `force`), write results back into `AppState`, and persist the cache. A failed resolve
/// keeps the previous (cached) membership and surfaces the error on the group.
pub async fn run_group_resolution(app_state: Arc<Mutex<AppState>>, force: bool) {
    let now = now_unix();
    let targets: Vec<(usize, GroupSource)> = {
        let mut app = app_state.lock().unwrap();
        let ttl_minutes = app.group_cache_ttl_minutes;
        let mut targets = Vec::new();
        for (group_idx, group) in app.groups.iter_mut().enumerate() {
            if !group.source.is_dynamic() {
                continue;
            }
            let stale = group.members.is_none()
                || group
                    .resolved_at
                    .is_none_or(|resolved_at| !resolved_fresh(resolved_at, now, ttl_minutes));
            if force || stale {
                group.resolving = true;
                targets.push((group_idx, group.source.clone()));
            } else {
                // `Z` may have optimistically flagged everything; clear fresh groups.
                group.resolving = false;
            }
        }
        targets
    };
    if targets.is_empty() {
        return;
    }

    let results = futures::future::join_all(targets.iter().map(|(_, source)| async move {
        match tokio::time::timeout(
            Duration::from_secs(RESOLVE_TIMEOUT_SECS),
            resolve_source(source),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(format!("timed out after {RESOLVE_TIMEOUT_SECS}s")),
        }
    }))
    .await;

    let cache = {
        let mut app = app_state.lock().unwrap();
        let prev = app.selected_repo_index();
        let mut first_error: Option<String> = None;
        for ((group_idx, _), result) in targets.iter().zip(results) {
            let group = &mut app.groups[*group_idx];
            group.resolving = false;
            match result {
                Ok(members) => {
                    group.members =
                        Some(members.into_iter().map(|name| name.to_lowercase()).collect());
                    group.error = None;
                    group.resolved_at = Some(now);
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(format!("group '{}': {err}", group.name));
                    }
                    group.error = Some(err);
                }
            }
        }
        if let Some(error) = first_error {
            app.show_toast(error);
        }
        app.recompute_group_assignments();
        app.reselect_repo(prev);
        GroupsCache {
            entries: app
                .groups
                .iter()
                .filter(|group| group.source.is_dynamic())
                .filter_map(|group| {
                    Some((
                        group.name.clone(),
                        CacheEntry {
                            resolved_at: group.resolved_at?,
                            fingerprint: group.source.fingerprint(),
                            members: group.members.clone()?,
                        },
                    ))
                })
                .collect(),
        }
    };
    save_cache(&cache);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_literals_exactly() {
        assert!(wildcard_match("pull-all", "pull-all"));
        assert!(!wildcard_match("pull-all", "pull-all-extra"));
        assert!(!wildcard_match("pull-all-extra", "pull-all"));
    }

    #[test]
    fn wildcard_matches_prefix_suffix_and_infix() {
        assert!(wildcard_match("mfe-*", "mfe-calendar"));
        assert!(!wildcard_match("mfe-*", "core-mfe"));
        assert!(wildcard_match("*-service", "auth-service"));
        assert!(!wildcard_match("*-service", "service-auth"));
        assert!(wildcard_match("a*b*c", "axxbyyc"));
        assert!(wildcard_match("a*b*c", "abc"));
        assert!(!wildcard_match("a*b*c", "acb"));
    }

    #[test]
    fn wildcard_backtracks_across_multiple_stars() {
        assert!(wildcard_match("*ab*ab*", "xxabxabxabyy"));
        assert!(!wildcard_match("*ab*ab*", "xxabyy"));
    }

    #[test]
    fn wildcard_is_case_insensitive() {
        assert!(wildcard_match("MFE-*", "mfe-calendar"));
        assert!(wildcard_match("mfe-*", "MFE-Calendar"));
    }

    #[test]
    fn wildcard_handles_bare_star_and_empties() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("", ""));
        assert!(!wildcard_match("", "x"));
    }

    #[test]
    fn group_def_requires_exactly_one_source() {
        let none = GroupDef { name: "g".to_string(), ..GroupDef::default() };
        assert!(none.source().unwrap_err().contains("g"));

        let two = GroupDef {
            name: "g".to_string(),
            pattern: Some("a*".to_string()),
            command: Some("true".to_string()),
            ..GroupDef::default()
        };
        assert!(two.source().unwrap_err().contains("exactly one"));

        let one = GroupDef {
            name: "g".to_string(),
            pattern: Some("a*".to_string()),
            ..GroupDef::default()
        };
        assert_eq!(one.source().unwrap(), GroupSource::Pattern("a*".to_string()));

        let unnamed = GroupDef { pattern: Some("a*".to_string()), ..GroupDef::default() };
        assert!(unnamed.source().is_err());
    }

    #[test]
    fn extract_keys_at_root_by_default() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"repo-a": {"x": 1}, "repo-b": {}}"#).unwrap();
        let members = extract_members(&json, &ExtractSpec::default()).unwrap();
        assert_eq!(members, vec!["repo-a", "repo-b"]);
    }

    #[test]
    fn extract_keys_at_nested_pointer() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"entries": {"repo-a": 1, "repo-b": 2}}"#).unwrap();
        let spec = ExtractSpec { pointer: "/entries".to_string(), kind: ExtractKind::Keys };
        assert_eq!(extract_members(&json, &spec).unwrap(), vec!["repo-a", "repo-b"]);
    }

    #[test]
    fn extract_values_from_array_skips_non_strings() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"list": ["repo-a", 42, "repo-b", null]}"#).unwrap();
        let spec = ExtractSpec { pointer: "/list".to_string(), kind: ExtractKind::Values };
        assert_eq!(extract_members(&json, &spec).unwrap(), vec!["repo-a", "repo-b"]);
    }

    #[test]
    fn extract_values_from_object_takes_string_values() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"a": "repo-a", "b": "repo-b", "c": 3}"#).unwrap();
        let spec = ExtractSpec { pointer: String::new(), kind: ExtractKind::Values };
        assert_eq!(extract_members(&json, &spec).unwrap(), vec!["repo-a", "repo-b"]);
    }

    #[test]
    fn extract_errors_on_bad_pointer_or_wrong_shape() {
        let json: serde_json::Value = serde_json::from_str(r#"{"a": [1]}"#).unwrap();
        let missing = ExtractSpec { pointer: "/nope".to_string(), kind: ExtractKind::Keys };
        assert!(extract_members(&json, &missing).is_err());
        let not_object = ExtractSpec { pointer: "/a".to_string(), kind: ExtractKind::Keys };
        assert!(extract_members(&json, &not_object).is_err());
    }

    #[test]
    fn resolved_fresh_respects_ttl_boundary() {
        // ttl 1 minute = 60s window: fresh strictly inside, stale at the boundary.
        assert!(resolved_fresh(1_000, 1_059, 1));
        assert!(!resolved_fresh(1_000, 1_060, 1));
        // Clock skew (resolved in the "future") never underflows.
        assert!(resolved_fresh(1_000, 500, 1));
    }

    #[test]
    fn config_defaults_kick_in_for_zero_values() {
        let config = GroupsConfig::default();
        assert_eq!(config.collapse_threshold(), DEFAULT_COLLAPSE_THRESHOLD);
        assert_eq!(config.cache_ttl_minutes(), DEFAULT_CACHE_TTL_MINUTES);
        let custom = GroupsConfig { collapse_threshold: 9, cache_ttl_minutes: 5, groups: vec![] };
        assert_eq!(custom.collapse_threshold(), 9);
        assert_eq!(custom.cache_ttl_minutes(), 5);
    }
}
