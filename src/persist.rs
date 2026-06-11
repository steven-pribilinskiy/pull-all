use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{
    Background, ColumnFlags, Contrast, HelpTab, IconStyle, RepoPageColumns, SortColumn, SortDir,
    Theme,
};

/// UI preferences persisted between runs at `~/.config/pull-all/state.json`.
/// `#[serde(default)]` keeps older state files (missing newer fields) loadable.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub columns: ColumnFlags,
    /// The info block (`i`) was shown on last exit.
    pub info_pinned: bool,
    /// Left/right splitter position.
    pub split_ratio: f64,
    /// 1-cell padding inside every bordered panel/modal.
    pub panel_padding: bool,
    /// Glyph set (Unicode vs emoji).
    pub icon_style: IconStyle,
    /// Color theme (auto / dark / light).
    pub theme: Theme,
    /// Contrast level (normal / soft) — text + accent saturation.
    pub contrast: Contrast,
    /// Background tone (normal / soft) — surface only. `None` in pre-split state files;
    /// `resolve_background` derives it from `contrast` for backward compatibility.
    pub background: Option<Background>,
    /// Repo-list sort column. Tolerant: the removed `"discovery"` value (and anything
    /// unknown) loads as the default (Name) without discarding the rest of the file.
    #[serde(default, deserialize_with = "sort_column_tolerant")]
    pub sort_column: SortColumn,
    /// Repo-list sort direction.
    pub sort_dir: SortDir,
    /// Last-active help-modal tab.
    pub help_tab: HelpTab,
    /// Grouped list view was on at last exit.
    pub grouping_enabled: bool,
    /// Names (or `folder::name` keys) of collapsed groups.
    pub collapsed_groups: Vec<String>,
    /// Directory-tree view was on at last exit.
    pub tree_enabled: bool,
    /// Relative paths of collapsed folders.
    pub collapsed_folders: Vec<String>,
    /// Repo-page branch columns (all on by default).
    pub repo_page_columns: RepoPageColumns,
    /// Repo-page bottom info panel shown (default on).
    #[serde(default = "default_true")]
    pub repo_page_info: bool,
}

fn default_true() -> bool {
    true
}

/// Deserialize `sort_column` tolerantly: the removed `"discovery"` value and any unknown
/// string fall back to the default (`Name`) instead of failing the whole-file parse.
fn sort_column_tolerant<'de, D>(deserializer: D) -> Result<SortColumn, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(match raw.as_str() {
        "branch" => SortColumn::Branch,
        "status" => SortColumn::Status,
        "ahead-behind" => SortColumn::AheadBehind,
        "dirty" => SortColumn::Dirty,
        "last-commit" => SortColumn::LastCommit,
        "worktrees" => SortColumn::Worktrees,
        "branches" => SortColumn::Branches,
        "stashes" => SortColumn::Stashes,
        // "name", removed "discovery", and anything unknown → default.
        _ => SortColumn::Name,
    })
}

/// Resolve the background tone for a loaded state file. Pre-split files carry only `contrast`,
/// where `Soft` meant a soft everything — so a missing background inherits the contrast level.
pub fn resolve_background(background: Option<Background>, contrast: Contrast) -> Background {
    background.unwrap_or(match contrast {
        Contrast::Soft => Background::Soft,
        Contrast::Normal => Background::Normal,
    })
}

fn state_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("pull-all").join("state.json"))
}

/// Load persisted UI state. A missing/corrupt file deserializes from `{}` so every field's
/// serde default applies (notably the `default = "default_true"` ones), unlike the derived
/// `Default` which would zero booleans like `repo_page_info`.
pub fn load() -> PersistedState {
    let contents = state_path().and_then(|path| std::fs::read_to_string(&path).ok());
    let raw = contents.as_deref().unwrap_or("{}");
    serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str("{}"))
        .expect("empty object deserializes with serde defaults")
}

/// Persist UI state, best-effort (errors are ignored).
/// (Unused in the test profile — `AppState::save_state` is stubbed out there.)
#[cfg_attr(test, allow(dead_code))]
pub fn save(state: &PersistedState) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(contents) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, contents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_inherits_soft_contrast_when_absent() {
        assert_eq!(resolve_background(None, Contrast::Soft), Background::Soft);
        assert_eq!(resolve_background(None, Contrast::Normal), Background::Normal);
    }

    #[test]
    fn explicit_background_wins_over_contrast() {
        assert_eq!(resolve_background(Some(Background::Normal), Contrast::Soft), Background::Normal);
        assert_eq!(resolve_background(Some(Background::Soft), Contrast::Normal), Background::Soft);
    }

    #[test]
    fn old_state_without_background_loads() {
        // A pre-split state file has no `background` key; serde(default) → None.
        let json = r#"{"contrast":"soft","theme":"dark"}"#;
        let state: PersistedState = serde_json::from_str(json).unwrap();
        assert_eq!(state.background, None);
        assert_eq!(resolve_background(state.background, state.contrast), Background::Soft);
    }

    #[test]
    fn removed_discovery_sort_loads_as_name_without_losing_other_fields() {
        // An old file with the removed "discovery" sort must not reset the whole file.
        let json = r#"{"sort_column":"discovery","panel_padding":true,"grouping_enabled":true}"#;
        let state: PersistedState = serde_json::from_str(json).unwrap();
        assert_eq!(state.sort_column, SortColumn::Name);
        assert!(state.panel_padding);
        assert!(state.grouping_enabled);
    }

    #[test]
    fn sort_column_tolerant_maps_known_and_unknown() {
        let cases = [
            (r#"{"sort_column":"branch"}"#, SortColumn::Branch),
            (r#"{"sort_column":"ahead-behind"}"#, SortColumn::AheadBehind),
            (r#"{"sort_column":"stashes"}"#, SortColumn::Stashes),
            (r#"{"sort_column":"garbage"}"#, SortColumn::Name),
        ];
        for (json, expected) in cases {
            let state: PersistedState = serde_json::from_str(json).unwrap();
            assert_eq!(state.sort_column, expected, "for {json}");
        }
    }
}
