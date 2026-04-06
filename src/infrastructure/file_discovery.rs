use std::path::PathBuf;

use anyhow::Result;
use glob::glob;
use serde::Deserialize;

pub struct FileDiscovery;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClaudeSettings {
    #[serde(default)]
    cleanup_period_days: Option<u32>,
}

pub struct RetentionWarning {
    pub days: u32,
    pub is_default: bool,
}

pub fn check_cleanup_period() -> Option<RetentionWarning> {
    let home = dirs::home_dir()?;
    let settings_path = home.join(".claude/settings.json");

    if !settings_path.exists() {
        return Some(RetentionWarning {
            days: 30,
            is_default: true,
        });
    }

    let content = std::fs::read_to_string(&settings_path).ok()?;
    let settings: ClaudeSettings = serde_json::from_str(&content).unwrap_or_default();

    match settings.cleanup_period_days {
        Some(days) if days <= 30 => Some(RetentionWarning {
            days,
            is_default: false,
        }),
        None => Some(RetentionWarning {
            days: 30,
            is_default: true,
        }),
        _ => None,
    }
}

impl FileDiscovery {
    pub fn find_jsonl_files_with_limit(limit: usize) -> Result<Vec<PathBuf>> {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

        let claude_projects = home.join(".claude/projects");

        if !claude_projects.exists() {
            return Ok(Vec::new());
        }

        let pattern = claude_projects.join("*/*.jsonl");
        let pattern_str = pattern.to_string_lossy();

        let mut files: Vec<PathBuf> = glob(&pattern_str)?.filter_map(std::result::Result::ok).collect();

        files.sort_by(|a, b| {
            let a_modified = a.metadata().and_then(|m| m.modified()).ok();
            let b_modified = b.metadata().and_then(|m| m.modified()).ok();
            b_modified.cmp(&a_modified)
        });

        if limit > 0 && files.len() > limit {
            files.truncate(limit);
        }

        Ok(files)
    }
}

mod dirs {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}
