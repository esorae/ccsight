use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

const CACHE_VERSION: u32 = 22;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheData {
    pub version: u32,
    pub files: HashMap<String, CachedFileStats>,
    #[serde(default)]
    pub day_summaries: HashMap<String, String>,
    #[serde(default)]
    pub session_summaries: HashMap<String, String>,
}

pub type CachedTokenStats = crate::aggregator::TokenStats;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedDailyStats {
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_by_model: HashMap<String, CachedTokenStats>,
    #[serde(default)]
    pub hourly_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub hourly_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub tool_usage: HashMap<String, usize>,
    #[serde(default)]
    pub language_usage: HashMap<String, usize>,
    #[serde(default)]
    pub extension_usage: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFileStats {
    pub modified_secs: u64,
    #[serde(default)]
    pub file_size: u64,
    pub entry_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub tool_usage: HashMap<String, usize>,
    pub model_usage: HashMap<String, usize>,
    #[serde(default)]
    pub model_tokens: HashMap<String, CachedTokenStats>,
    pub session_date: Option<NaiveDate>,
    pub project_name: Option<String>,
    pub session_id: Option<String>,
    pub git_branch: Option<String>,
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub is_subagent: bool,
    #[serde(default)]
    pub daily_stats: HashMap<String, CachedDailyStats>,
    #[serde(default)]
    pub hourly_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub hourly_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub weekday_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub weekday_work_activity: HashMap<u8, u64>,
    #[serde(default)]
    pub tool_error_count: usize,
    #[serde(default)]
    pub tool_success_count: usize,
    #[serde(default)]
    pub session_duration_mins: Option<i64>,
    #[serde(default)]
    pub language_usage: HashMap<String, usize>,
    #[serde(default)]
    pub extension_usage: HashMap<String, usize>,
}

impl Default for CacheData {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            files: HashMap::new(),
            day_summaries: HashMap::new(),
            session_summaries: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Cache {
    cache_path: PathBuf,
    data: CacheData,
}

impl Cache {
    pub fn new_empty() -> Self {
        Self {
            cache_path: PathBuf::from("/dev/null"), // Fallback path, won't be saved
            data: CacheData::default(),
        }
    }

    pub fn load() -> Result<Self> {
        let cache_path = Self::cache_file_path()?;
        let data = if cache_path.exists() {
            let file = File::open(&cache_path)?;
            let reader = BufReader::new(file);
            match serde_json::from_reader::<_, CacheData>(reader) {
                Ok(cache) if cache.version == CACHE_VERSION => cache,
                _ => CacheData::default(),
            }
        } else {
            CacheData::default()
        };

        Ok(Self { cache_path, data })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write: write to temp file, then rename
        let temp_path = self.cache_path.with_extension("json.tmp");
        let file = match File::create(&temp_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Stale file with wrong ownership — try to remove and recreate
                let _ = fs::remove_file(&temp_path);
                let _ = fs::remove_file(&self.cache_path);
                File::create(&temp_path)?
            }
            Err(e) => return Err(e.into()),
        };

        // Set restrictive permissions (0600) on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&temp_path, permissions)?;
        }

        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &self.data)?;
        writer.flush()?;
        writer.into_inner()?.sync_all()?;

        // Atomic rename (POSIX guarantees this is atomic on same filesystem)
        if let Err(e) = fs::rename(&temp_path, &self.cache_path) {
            // Clean up temp file on failure
            let _ = fs::remove_file(&temp_path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn get(&self, path: &Path) -> Option<&CachedFileStats> {
        let key = path.to_string_lossy().to_string();
        self.data.files.get(&key)
    }

    pub fn is_valid(&self, path: &Path) -> bool {
        let key = path.to_string_lossy().to_string();
        if let Some(cached) = self.data.files.get(&key)
            && let Ok(metadata) = fs::metadata(path) {
                let current_size = metadata.len();
                if let Ok(modified) = metadata.modified() {
                    let modified_secs = modified
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return cached.modified_secs == modified_secs
                        && cached.file_size == current_size;
                }
            }
        false
    }

    pub fn insert(&mut self, path: &Path, stats: CachedFileStats) {
        let key = path.to_string_lossy().to_string();
        self.data.files.insert(key, stats);
    }

    pub fn get_day_summary(&self, date: &NaiveDate) -> Option<&String> {
        let key = date.format("%Y-%m-%d").to_string();
        self.data.day_summaries.get(&key)
    }

    pub fn set_day_summary(&mut self, date: &NaiveDate, summary: String) {
        let key = date.format("%Y-%m-%d").to_string();
        self.data.day_summaries.insert(key, summary);
    }

    pub fn get_session_summary(&self, path: &Path) -> Option<&String> {
        let key = path.to_string_lossy().to_string();
        self.data.session_summaries.get(&key)
    }

    pub fn set_session_summary(&mut self, path: &Path, summary: String) {
        let key = path.to_string_lossy().to_string();
        self.data.session_summaries.insert(key, summary);
    }

    fn cache_file_path() -> Result<PathBuf> {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".cache/ccsight/cache.json"))
    }
}

pub fn get_file_modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

pub fn get_file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::TokenStats;

    #[test]
    fn test_cached_token_stats_from_token_stats() {
        let ts: CachedTokenStats = TokenStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 20,
            cache_read_tokens: 10,
        };

        assert_eq!(ts.input_tokens, 100);
        assert_eq!(ts.output_tokens, 50);
        assert_eq!(ts.cache_creation_tokens, 20);
        assert_eq!(ts.cache_read_tokens, 10);
    }

    #[test]
    fn test_cached_token_stats_from_zero_token_stats() {
        let ts: CachedTokenStats = TokenStats::default();

        assert_eq!(ts.input_tokens, 0);
        assert_eq!(ts.output_tokens, 0);
        assert_eq!(ts.cache_creation_tokens, 0);
        assert_eq!(ts.cache_read_tokens, 0);
    }
}
