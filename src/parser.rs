use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::LogEntry;

// Conservative limits to prevent DoS from malformed files
// These can be overridden via environment variables if needed
// Validated ranges: file_size [1MB, 2GB], line_size [1KB, 100MB], entries [100, 1M]

const DEFAULT_MAX_FILE_SIZE: u64 = 500 * 1024 * 1024; // 500MB
const MIN_MAX_FILE_SIZE: u64 = 1024 * 1024; // 1MB
const MAX_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB

const DEFAULT_MAX_LINE_SIZE: usize = 50 * 1024 * 1024; // 50MB
const MIN_MAX_LINE_SIZE: usize = 1024; // 1KB
const MAX_MAX_LINE_SIZE: usize = 100 * 1024 * 1024; // 100MB

const DEFAULT_MAX_ENTRIES: usize = 100_000;
const MIN_MAX_ENTRIES: usize = 100;
const MAX_MAX_ENTRIES: usize = 1_000_000;

fn max_file_size() -> u64 {
    std::env::var("CCSIGHT_MAX_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_FILE_SIZE, |v: u64| v.clamp(MIN_MAX_FILE_SIZE, MAX_MAX_FILE_SIZE))
}

fn max_line_size() -> usize {
    std::env::var("CCSIGHT_MAX_LINE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_LINE_SIZE, |v: usize| v.clamp(MIN_MAX_LINE_SIZE, MAX_MAX_LINE_SIZE))
}

fn max_entries() -> usize {
    std::env::var("CCSIGHT_MAX_ENTRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_ENTRIES, |v: usize| v.clamp(MIN_MAX_ENTRIES, MAX_MAX_ENTRIES))
}

pub struct JsonlParser;

impl JsonlParser {
    pub fn parse_file(path: &Path) -> Result<Vec<LogEntry>> {
        let file =
            File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

        let metadata = file.metadata()?;
        let file_size_limit = max_file_size();
        if metadata.len() > file_size_limit {
            anyhow::bail!(
                "File too large: {} bytes (max {} bytes)",
                metadata.len(),
                file_size_limit
            );
        }

        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut hash_to_index: HashMap<String, usize> = HashMap::new();
        let entry_limit = max_entries();
        let line_size_limit = max_line_size();

        for line_result in reader.lines() {
            if entries.len() >= entry_limit {
                break;
            }

            let Ok(line) = line_result else {
                continue;
            };

            if line.len() > line_size_limit {
                continue;
            }

            if line.trim().is_empty() {
                continue;
            }

            if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
                if let Some(hash) = Self::create_dedup_hash(&entry) {
                    if let Some(&existing_idx) = hash_to_index.get(&hash) {
                        entries[existing_idx] = entry;
                    } else {
                        hash_to_index.insert(hash, entries.len());
                        entries.push(entry);
                    }
                } else {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }

    fn create_dedup_hash(entry: &LogEntry) -> Option<String> {
        let request_id = entry.request_id.as_ref()?;
        let message_id = entry.message.as_ref()?.id.as_ref()?;
        Some(format!("{message_id}:{request_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_line() {
        let json = r#"{"uuid":"123","timestamp":"2025-01-01T00:00:00Z","type":"user","message":{"role":"user","content":"hello"}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.uuid, Some("123".to_string()));
    }

    #[test]
    fn test_parse_summary_entry() {
        let json = r#"{"type":"summary","summary":"Test summary","leafUuid":"abc"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.summary, Some("Test summary".to_string()));
        assert!(entry.message.is_none());
    }

    #[test]
    fn test_parse_file_history_snapshot() {
        let json = r#"{"type":"file-history-snapshot","messageId":"123","snapshot":{}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(
            entry.entry_type,
            crate::domain::EntryType::FileHistorySnapshot
        );
    }

    #[test]
    fn test_parse_unknown_type() {
        let json = r#"{"type":"some-unknown-type","data":"test"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, crate::domain::EntryType::Unknown);
    }

    #[test]
    fn test_parse_actual_files() {
        let Some(home) = std::env::var_os("HOME") else {
            return; // Skip test if HOME is not set
        };
        let projects_dir = format!("{}/.claude/projects", home.to_string_lossy());

        if std::path::Path::new(&projects_dir).exists() {
            let pattern = format!("{}/*/*.jsonl", projects_dir);
            let files: Vec<_> = glob::glob(&pattern)
                .unwrap()
                .filter_map(|r| r.ok())
                .take(5)
                .collect();

            let mut total_entries = 0;
            let mut total_errors = 0;

            for file in &files {
                match JsonlParser::parse_file(file) {
                    Ok(entries) => {
                        total_entries += entries.len();
                    }
                    Err(e) => {
                        total_errors += 1;
                        // JSONL parse failure is non-fatal
                    }
                }
            }

            println!(
                "Parsed {} entries from {} files ({} errors)",
                total_entries,
                files.len(),
                total_errors
            );
            assert!(total_entries > 0, "Should parse at least some entries");
        }
    }
}
