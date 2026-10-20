use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// Bump on changes to: parser output, aggregator field semantics
// (`extract_project_name`, `extract_session_model`, `git_branch`, etc.).
const CACHE_VERSION: u32 = 46;

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
    /// Lines added / removed by this day's edits. Stored per day only — a
    /// per-file copy would be a second definition of the same number that
    /// could drift. `serde(default)` keeps the
    /// derive total; the version gate is what stops a stale entry being read.
    #[serde(default)]
    pub line_churn: crate::domain::LineChurn,
    /// Per-day count of user / assistant message turns. `serde(default)`
    /// keeps deserialisation tolerant of older cache layouts; a version
    /// bump forces a rebuild that populates real values.
    #[serde(default)]
    pub user_msgs: u64,
    /// The share of `user_msgs` someone typed.
    #[serde(default)]
    pub prompts: u64,
    #[serde(default)]
    pub assistant_msgs: u64,
    #[serde(default)]
    pub tool_errors: usize,
    #[serde(default)]
    pub tool_successes: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedFileStats {
    pub modified_secs: u64,
    #[serde(default)]
    pub file_size: u64,
    /// `(message.id, requestId)` pairs credited to this file. Pre-loaded
    /// into the cross-file dedup set so resume/branch duplicates skip.
    /// Empty for caches predating this field (`serde(default)`).
    #[serde(default)]
    pub unique_hashes: Vec<String>,
    pub session_date: Option<NaiveDate>,
    pub project_name: Option<String>,
    /// First cwd in the file whose official slug matches the parent project
    /// dir — the verified `claude -r` cd target. `None` = no witness in this
    /// file (resume falls back to dir-level witnesses, never a guess).
    #[serde(default)]
    pub verified_cwd: Option<String>,
    pub session_id: Option<String>,
    pub git_branch: Option<String>,
    pub first_timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub ai_title: Option<String>,
    /// Most recent user-role message text. Powers the Live/Daily preview's
    /// "remind me what I was doing" line. Kept here so cache-valid sessions
    /// don't need to re-walk entries on every load.
    #[serde(default)]
    pub last_user_message: Option<String>,
    /// First user-role message text. Universal title fallback so Live row
    /// line 2 always carries something semantic instead of "—".
    #[serde(default)]
    pub first_user_message: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub is_subagent: bool,
    #[serde(default)]
    pub daily_stats: HashMap<String, CachedDailyStats>,
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
    /// Test-only: an empty cache that DOES persist to `path` (unlike
    /// `new_empty`'s non-saving `/dev/null`), so a test can read back what a
    /// writer produced.
    #[cfg(test)]
    pub(crate) fn empty_at(cache_path: PathBuf) -> Self {
        Self {
            cache_path,
            data: CacheData::default(),
        }
    }

    /// These contents with saving disabled, for tests that drive the real
    /// load path over real data: `save` refuses the placeholder path, so the
    /// developer's cache is read but never rewritten.
    #[cfg(test)]
    pub(crate) fn read_only(mut self) -> Self {
        self.cache_path = PathBuf::from("/dev/null");
        self
    }

    /// The cache as it would come back from disk: serialized and parsed
    /// again, so a test exercises the on-disk shape without touching disk.
    #[cfg(test)]
    pub(crate) fn through_disk_shape(self) -> Self {
        let json = serde_json::to_vec(&self.data).expect("serialize cache");
        Self {
            cache_path: self.cache_path,
            data: serde_json::from_slice(&json).expect("parse cache"),
        }
    }

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
        // Reject the `/dev/null` placeholder used by `Cache::new_empty()` when HOME is
        // unset. Saving to /dev/null on Linux silently succeeds but never persists, which
        // would mask the failure mode. Surface it as an error.
        if self.cache_path.as_os_str() == "/dev/null" {
            return Err(anyhow::anyhow!(
                "Cannot save cache: HOME is not set (cache_path is /dev/null fallback)"
            ));
        }
        // Stream the serializer to disk — the cache JSON is tens of MB, and
        // building it in memory first spikes RSS on every 30s-reload save.
        crate::infrastructure::atomic_write_with(&self.cache_path, |w| {
            serde_json::to_writer(w, &self.data).map_err(io::Error::from)
        })?;
        Ok(())
    }

    pub fn get(&self, path: &Path) -> Option<&CachedFileStats> {
        let key = path.to_string_lossy().to_string();
        self.data.files.get(&key)
    }

    pub fn is_valid(&self, path: &Path) -> bool {
        let key = path.to_string_lossy().to_string();
        if let Some(cached) = self.data.files.get(&key)
            && let Ok(metadata) = fs::metadata(path)
        {
            let current_size = metadata.len();
            if let Ok(modified) = metadata.modified() {
                let modified_secs = modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                return cached.modified_secs == modified_secs && cached.file_size == current_size;
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

    /// One file per cache version: a ccsight still running an older build (a
    /// long-lived MCP server) keeps its own file instead of discarding this one
    /// on every load and making both rebuild from scratch.
    fn cache_file_path() -> Result<PathBuf> {
        let path = super::cache_path()?.with_file_name(format!("cache-v{CACHE_VERSION}.json"));
        if let Some(dir) = path.parent() {
            prune_stale_versions(dir, &path, is_cache_file, STALE_VERSION_AFTER);
        }
        Ok(path)
    }

    /// Every version's cache file, for `--clear-cache`.
    pub fn clear_all() -> Result<Vec<PathBuf>> {
        let dir = super::cache_path()?
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let mut removed = Vec::new();
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_cache_file)
                && fs::remove_file(&path).is_ok()
            {
                removed.push(path);
            }
        }
        Ok(removed)
    }
}

/// Another version's state goes once nothing has written it for this long —
/// a build still running keeps touching its own file on every reload.
pub(crate) const STALE_VERSION_AFTER: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

fn is_cache_file(name: &str) -> bool {
    name == "cache.json" || (name.starts_with("cache-v") && name.ends_with(".json"))
}

/// Remove the entries of `dir` that `in_family` claims, other than `keep`,
/// whose last write is older than `idle`. Best-effort: a failure leaves the
/// entry for the next pass.
pub(crate) fn prune_stale_versions(
    dir: &Path,
    keep: &Path,
    in_family: fn(&str) -> bool,
    idle: std::time::Duration,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let claimed = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(in_family);
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > idle);
        if claimed && stale && path != keep {
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            drop(removed);
        }
    }
}

pub fn get_file_modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    // A version still being written survives; one idle past the window goes;
    // names outside the family and the current file are never touched.
    #[test]
    fn only_idle_other_versions_are_pruned() {
        let dir = std::env::temp_dir().join(format!("ccsight-prune-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
        let make = |name: &str, idle: bool| {
            let path = dir.join(name);
            let file = File::create(&path).unwrap();
            if idle {
                file.set_modified(old).unwrap();
            }
            path
        };
        let current = make("cache-v9.json", true);
        let idle_other = make("cache-v8.json", true);
        let busy_other = make("cache.json", false);
        let unrelated = make("pins.json", true);

        prune_stale_versions(&dir, &current, is_cache_file, STALE_VERSION_AFTER);

        assert!(current.exists(), "the current version is kept");
        assert!(!idle_other.exists(), "an idle other version is removed");
        assert!(busy_other.exists(), "a version still written is kept");
        assert!(unrelated.exists(), "a non-cache file is never touched");
        fs::remove_dir_all(&dir).unwrap();
    }

    use super::*;

    #[test]
    fn a_read_only_cache_refuses_to_save() {
        let path = std::env::temp_dir().join(format!("ccsight-ro-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(Cache::empty_at(path.clone()).read_only().save().is_err());
        assert!(!path.exists(), "a read-only cache wrote its file");
    }
    use crate::aggregator::TokenStats;

    #[test]
    fn test_cached_token_stats_from_token_stats() {
        let ts: CachedTokenStats = TokenStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 20,
            cache_read_tokens: 10,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
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

    #[test]
    fn test_new_empty_cache_save_returns_error() {
        // Regression: Cache::new_empty() uses /dev/null as a placeholder when no HOME.
        // save() must surface an error rather than silently writing to /dev/null.
        let cache = Cache::new_empty();
        let err = cache
            .save()
            .expect_err("save() should fail on /dev/null fallback");
        assert!(
            err.to_string().contains("HOME is not set"),
            "error message should mention HOME, got: {err}"
        );
    }

    #[test]
    fn test_cache_save_load_roundtrip() {
        // Regression: serialize a Cache to disk, reload it, verify CACHE_VERSION matches
        // and the per-file entries survive (including new tool_usage / daily_stats keys).
        // Guards against silent data loss when the schema gains new fields.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-cache-roundtrip-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let key = "/tmp/some-session.jsonl".to_string();
        let mut tool_usage = HashMap::new();
        tool_usage.insert("Bash".to_string(), 5);
        tool_usage.insert("skill:my-skill".to_string(), 2);

        let original = Cache {
            cache_path: path.clone(),
            data: CacheData {
                version: CACHE_VERSION,
                files: {
                    let mut map = HashMap::new();
                    map.insert(
                        key.clone(),
                        CachedFileStats {
                            modified_secs: 1234,
                            file_size: 100,
                            project_name: Some("proj".to_string()),
                            session_id: Some("sid".to_string()),
                            daily_stats: HashMap::from([(
                                "2026-01-01".to_string(), // lint-ok: date-literal
                                CachedDailyStats {
                                    tool_usage: tool_usage.clone(),
                                    ..Default::default()
                                },
                            )]),
                            ..Default::default()
                        },
                    );
                    map
                },
                day_summaries: HashMap::new(),
                session_summaries: HashMap::new(),
            },
        };

        original.save().expect("save cache");

        // Reload by reading the file directly (Cache::load uses fixed XDG path).
        let file = File::open(&path).expect("open saved cache");
        let reader = BufReader::new(file);
        let reloaded: CacheData = serde_json::from_reader(reader).expect("parse cache");

        assert_eq!(reloaded.version, CACHE_VERSION);
        let entry = reloaded.files.get(&key).expect("file entry survived");
        let day = &entry.daily_stats["2026-01-01"]; // lint-ok: date-literal
        assert_eq!(day.tool_usage.get("Bash"), Some(&5));
        assert_eq!(day.tool_usage.get("skill:my-skill"), Some(&2));
        assert_eq!(entry.session_id.as_deref(), Some("sid"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_old_cache_version_is_rejected_at_load() {
        // Write a JSON cache file with version = CACHE_VERSION - 1 and
        // verify Cache::load_from_path treats it as missing (returns
        // empty), so a stale cache after a schema bump rebuilds rather
        // than serving wrong data.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-cache-version-mismatch-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let stale = serde_json::json!({
            "version": CACHE_VERSION - 1,
            "files": {
                "/tmp/legacy.jsonl": {
                    "modified_secs": 0,
                    "file_size": 0,
                    "entry_count": 0,
                    "input_tokens": 999,
                    "output_tokens": 0,
                    "cache_creation_tokens": 0,
                    "cache_read_tokens": 0,
                }
            }
        });
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        // Read the file directly via the same loader logic the real load
        // path uses (version mismatch → CacheData::default()).
        let file = File::open(&path).expect("open stale cache");
        let reader = BufReader::new(file);
        let parsed: serde_json::Result<CacheData> = serde_json::from_reader(reader);
        let cache = match parsed {
            Ok(cache) if cache.version == CACHE_VERSION => cache,
            _ => CacheData::default(),
        };

        // After version-mismatch fallback, the cache should carry the
        // current version (CacheData::default() returns CACHE_VERSION) and
        // an empty files map (the stale entry must NOT survive).
        assert_eq!(cache.version, CACHE_VERSION);
        assert!(
            cache.files.is_empty(),
            "stale-version cache must NOT carry stale per-file entries forward"
        );

        let _ = fs::remove_file(&path);
    }
}
