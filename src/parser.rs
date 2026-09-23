use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::LogEntry;

// Conservative limits to prevent DoS from malformed files
// These can be overridden via environment variables if needed
// Validated ranges: file_size [1MB, 2GB], line_size [1KB, 100MB], entries [100, 1M]

// Large session JSONLs (~hundreds of MB) appear in normal heavy use, so the
// default must accommodate them or token totals silently drop the entries from
// over-cap files. Peak memory tracks the longest single line, not the file
// size — but `max_line_size` does not bound it: `BufRead::lines` has already
// allocated the whole line by the time the cap rejects it.
const DEFAULT_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB
const MIN_MAX_FILE_SIZE: u64 = 1024 * 1024; // 1MB
const MAX_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB

const DEFAULT_MAX_LINE_SIZE: usize = 50 * 1024 * 1024; // 50MB
const MIN_MAX_LINE_SIZE: usize = 1024; // 1KB
const MAX_MAX_LINE_SIZE: usize = 100 * 1024 * 1024; // 100MB

// A truncated parse drops the NEWEST entries (the loop breaks on its way
// forward), so an over-cap session reports stale totals AND a stale
// last-activity time. Matched to `MAX_MAX_ENTRIES` for the same reason
// `DEFAULT_MAX_FILE_SIZE` is 2GB: real heavy-use transcripts reach these
// sizes, and dropping their tail is worse than the memory it costs.
const DEFAULT_MAX_ENTRIES: usize = 1_000_000;
const MIN_MAX_ENTRIES: usize = 100;
const MAX_MAX_ENTRIES: usize = 1_000_000;

fn max_file_size() -> u64 {
    std::env::var("CCSIGHT_MAX_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_FILE_SIZE, |v: u64| {
            v.clamp(MIN_MAX_FILE_SIZE, MAX_MAX_FILE_SIZE)
        })
}

fn clamp_line_size(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_LINE_SIZE, |v: usize| {
            v.clamp(MIN_MAX_LINE_SIZE, MAX_MAX_LINE_SIZE)
        })
}

fn clamp_entries(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_ENTRIES, |v: usize| {
            v.clamp(MIN_MAX_ENTRIES, MAX_MAX_ENTRIES)
        })
}

/// Caps carried as a value rather than read inside the parse loop: the guards
/// they drive silently drop data, and a test that has to set a process-wide
/// env var to reach them races every other test in the binary.
#[derive(Clone, Copy)]
struct Limits {
    line_size: usize,
    entries: usize,
}

impl Limits {
    fn from_env() -> Self {
        Self {
            line_size: clamp_line_size(std::env::var("CCSIGHT_MAX_LINE_SIZE").ok().as_deref()),
            entries: clamp_entries(std::env::var("CCSIGHT_MAX_ENTRIES").ok().as_deref()),
        }
    }
}

/// Files whose entry cap stopped the walk this process. Counted only on the
/// file path: the byte seam is also reached from property tests with tiny caps,
/// and those are not user data. Read by the UI so an incomplete total is never
/// presented as a complete one. A set, not a counter: a file is re-parsed on
/// every reload and by the index, the conversation view and title generation.
static TRUNCATED_FILES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::LazyLock::new(Default::default);

/// Number of distinct files truncated by the entry cap so far.
pub fn truncated_file_count() -> usize {
    TRUNCATED_FILES.lock().map_or(0, |set| set.len())
}

pub struct JsonlParser;

impl JsonlParser {
    pub fn parse_file(path: &Path) -> Result<Vec<LogEntry>> {
        Self::parse_file_with(path, Limits::from_env())
    }

    fn parse_file_with(path: &Path, limits: Limits) -> Result<Vec<LogEntry>> {
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

        let (entries, truncated) = Self::parse_lines_reporting(BufReader::new(file), limits);
        if truncated && let Ok(mut set) = TRUNCATED_FILES.lock() {
            set.insert(path.to_path_buf());
        }
        Ok(entries)
    }

    /// Parse raw JSONL bytes into entries — the pure byte seam. Per-line
    /// errors are skipped, never propagated, so it can't fail (returns
    /// whatever parsed). Its invariants are pinned by the `SEAM_INPUTS` table
    /// and the property tests in this file's `mod tests`.
    pub fn parse_entries(bytes: &[u8]) -> Vec<LogEntry> {
        Self::parse_lines(BufReader::new(bytes), Limits::from_env())
    }

    fn parse_lines<R: BufRead>(reader: R, limits: Limits) -> Vec<LogEntry> {
        Self::parse_lines_reporting(reader, limits).0
    }

    /// `true` = the entry cap stopped the walk, so the tail of the file is
    /// missing. Private: the public byte seam stays `-> Vec<LogEntry>`.
    fn parse_lines_reporting<R: BufRead>(reader: R, limits: Limits) -> (Vec<LogEntry>, bool) {
        let mut truncated = false;
        let mut entries = Vec::new();
        let mut hash_to_index: HashMap<String, usize> = HashMap::new();
        let entry_limit = limits.entries;
        let line_size_limit = limits.line_size;

        for line_result in reader.lines() {
            if entries.len() >= entry_limit {
                truncated = true;
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

            let Ok(mut entry) = serde_json::from_str::<LogEntry>(&line) else {
                continue;
            };

            // Cowork `audit.jsonl` puts the wall-clock time in `_audit_timestamp`
            // and leaves the standard `timestamp` field null. Fill the gap so
            // downstream date-grouping / hourly aggregation works without
            // every consumer needing to know about the alternate field.
            if entry.timestamp.is_none()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && let Some(ts_str) = value.get("_audit_timestamp").and_then(|v| v.as_str())
                && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str)
            {
                entry.timestamp = Some(ts.with_timezone(&chrono::Utc));
            }

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

        (entries, truncated)
    }

    pub fn entry_hash(entry: &LogEntry) -> Option<String> {
        Self::create_dedup_hash(entry)
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
    fn a_file_reparsed_after_truncation_is_counted_once() {
        let path = std::env::temp_dir().join(format!("ccsight-trunc-{}.jsonl", std::process::id()));
        let line = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        std::fs::write(&path, line.repeat(5)).unwrap();
        let limits = || Limits {
            line_size: 1 << 20,
            entries: 2,
        };
        let before = truncated_file_count();
        // Every reload re-parses the same file; the warning counts files.
        for _ in 0..3 {
            JsonlParser::parse_file_with(&path, limits()).unwrap();
        }
        assert_eq!(truncated_file_count() - before, 1);
        let _ = std::fs::remove_file(&path);
    }

    /// Byte blobs the parse seam must survive — a JSONL read mid-write, a line
    /// of terminal control bytes — each with the entry count it must yield.
    /// Without that count a parser that drops everything still passes, since
    /// most of these blobs produce none. Inline rather than files on disk so a
    /// review diff shows the bytes.
    const SEAM_INPUTS: &[(&str, &[u8], usize)] = &[
        ("empty", b"", 0),
        (
            "valid_line",
            b"{\"type\":\"assistant\",\"uuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n",
            1,
        ),
        ("truncated_json", b"{\"type\":\"assist", 0),
        (
            "blank_lines_mixed",
            b"{\"type\":\"user\"}\n\n\n{\"type\":\"summary\",\"summary\":\"x\"}\n",
            2,
        ),
        (
            "control_and_invalid_utf8",
            b"\x00\x01\xff not json \x1b[31mANSI\x07",
            0,
        ),
        // A tool result is foreign JSON reaching the seam. Reading churn out
        // of it must not cost the entry: a nested third-party payload takes
        // the entry's tokens down with it if the reader has a depth ceiling.
        (
            "tool_result_edit",
            b"{\"type\":\"user\",\"toolUseResult\":{\"structuredPatch\":[{\"lines\":[\" c\",\"-x\",\"+y\"]}]}}\n",
            1,
        ),
        (
            "tool_result_deeply_nested",
            b"{\"type\":\"user\",\"toolUseResult\":[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]}\n",
            1,
        ),
        (
            "tool_result_truncated_mid_write",
            b"{\"type\":\"user\",\"toolUseResult\":{\"structuredPatch\":[{\"lines\":[\"+a\"",
            0,
        ),
        // The compaction recap is a plain `user` row plus a flag. An entry that
        // dropped over the unknown flag would take the recap — the only
        // whole-session signal the title prompt has — with it.
        (
            "compact_summary_flag",
            b"{\"type\":\"user\",\"isCompactSummary\":true,\"message\":{\"role\":\"user\",\"content\":\"recap text\"}}\n",
            1,
        ),
        (
            "compact_summary_flag_wrong_type",
            b"{\"type\":\"user\",\"isCompactSummary\":\"yes\",\"message\":{\"role\":\"user\",\"content\":\"recap\"}}\n",
            0,
        ),
        // An Agent / Task result carries `content` as an array of blocks; a
        // value of an unexpected type must not cost the entry after it.
        (
            "tool_result_array_content",
            b"{\"type\":\"user\",\"toolUseResult\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]},\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            1,
        ),
        (
            "tool_result_patch_is_an_object",
            b"{\"type\":\"user\",\"toolUseResult\":{\"structuredPatch\":{\"x\":1},\"filePath\":\"/a\"},\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            1,
        ),
    ];

    // Non-panic alone is a weak oracle: an entry the parser accepted but that
    // cannot be re-read would corrupt the cache silently, so assert the
    // round-trip too.
    #[test]
    fn seam_inputs_yield_their_expected_entries_and_round_trip() {
        for (name, bytes, expected) in SEAM_INPUTS {
            let entries = JsonlParser::parse_entries(bytes);
            assert_eq!(entries.len(), *expected, "{name}: wrong entry count");
            for entry in entries {
                let json = serde_json::to_string(&entry)
                    .unwrap_or_else(|e| panic!("{name}: accepted entry does not serialize: {e}"));
                serde_json::from_str::<LogEntry>(&json)
                    .unwrap_or_else(|e| panic!("{name}: accepted entry does not round-trip: {e}"));
            }
        }
    }

    /// One `(message.id, requestId)` pair repeated across lines — a retried
    /// request. The later line carries the settled usage, so it must win.
    #[test]
    fn dedup_keeps_the_last_line_for_a_repeated_message_and_request_id() {
        let bytes = concat!(
            r#"{"type":"assistant","uuid":"u1","requestId":"r1","message":{"role":"assistant","id":"m1","content":"stale"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"u2","requestId":"r1","message":{"role":"assistant","id":"m1","content":"settled"}}"#,
            "\n",
        )
        .as_bytes();
        let entries = JsonlParser::parse_entries(bytes);
        assert_eq!(entries.len(), 1, "the pair must collapse to one entry");
        assert_eq!(
            entries[0].uuid.as_deref(),
            Some("u2"),
            "first-wins would keep the stale usage of a retried request"
        );
    }

    #[test]
    fn entry_cap_stops_the_walk_before_reading_the_rest() {
        let line = |i: usize| format!(r#"{{"type":"user","uuid":"u{i}"}}"#);
        let body = (0..5).map(line).collect::<Vec<_>>().join("\n");
        let limits = Limits {
            line_size: DEFAULT_MAX_LINE_SIZE,
            entries: 2,
        };
        let (entries, truncated) =
            JsonlParser::parse_lines_reporting(BufReader::new(body.as_bytes()), limits);
        assert_eq!(entries.len(), 2, "the cap must stop the walk, not the file");
        // The dropped entries are the NEWEST ones, so a silent cap understates
        // both the totals and the last-activity time.
        assert!(truncated, "hitting the cap has to be reportable");

        let under = Limits {
            line_size: DEFAULT_MAX_LINE_SIZE,
            entries: 99,
        };
        let (all, hit) = JsonlParser::parse_lines_reporting(BufReader::new(body.as_bytes()), under);
        assert_eq!(all.len(), 5);
        assert!(!hit, "a file inside the cap is not truncated");
    }

    // The cap defaults to its own ceiling for the same reason
    // `DEFAULT_MAX_FILE_SIZE` is 2GB: real transcripts reach these sizes, and
    // the walk breaks going forward, so a lower default eats the tail.
    #[test]
    fn the_entry_cap_default_does_not_sit_below_its_ceiling() {
        assert_eq!(DEFAULT_MAX_ENTRIES, MAX_MAX_ENTRIES);
    }

    #[test]
    fn line_cap_skips_the_oversized_line_and_keeps_the_rest() {
        let long = format!(r#"{{"type":"user","uuid":"{}"}}"#, "x".repeat(200));
        let body = format!("{long}\n{{\"type\":\"user\",\"uuid\":\"short\"}}\n");
        let limits = Limits {
            line_size: 64,
            entries: DEFAULT_MAX_ENTRIES,
        };
        let entries = JsonlParser::parse_lines(BufReader::new(body.as_bytes()), limits);
        assert_eq!(entries.len(), 1, "only the over-cap line should drop");
        assert_eq!(entries[0].uuid.as_deref(), Some("short"));
    }

    #[test]
    fn env_caps_fall_back_to_the_default_and_clamp_out_of_range_values() {
        assert_eq!(clamp_entries(None), DEFAULT_MAX_ENTRIES);
        assert_eq!(clamp_entries(Some("not a number")), DEFAULT_MAX_ENTRIES);
        assert_eq!(clamp_entries(Some("1")), MIN_MAX_ENTRIES);
        assert_eq!(clamp_entries(Some("999999999")), MAX_MAX_ENTRIES);
        assert_eq!(clamp_line_size(None), DEFAULT_MAX_LINE_SIZE);
        assert_eq!(clamp_line_size(Some("")), DEFAULT_MAX_LINE_SIZE);
        assert_eq!(clamp_line_size(Some("1")), MIN_MAX_LINE_SIZE);
        assert_eq!(clamp_line_size(Some("999999999999")), MAX_MAX_LINE_SIZE);
    }

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
    fn test_audit_timestamp_fallback() {
        // Cowork audit.jsonl emits `timestamp: null` and carries the wall-clock
        // time in `_audit_timestamp` — the parser should backfill so date
        // bucketing works on these files just like Claude Code JSONL.
        let dir = std::env::temp_dir().join(format!("ccsight-parser-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let line = r#"{"type":"assistant","uuid":"u1","session_id":"s1","timestamp":null,"_audit_timestamp":"2026-04-27T01:23:45.000Z","message":{"role":"assistant","content":"hi"}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let entries = JsonlParser::parse_file(&path).unwrap();
        assert_eq!(entries.len(), 1);
        let ts = entries[0]
            .timestamp
            .expect("timestamp filled from _audit_timestamp");
        assert_eq!(ts.to_rfc3339(), "2026-04-27T01:23:45+00:00");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_unknown_type() {
        let json = r#"{"type":"some-unknown-type","data":"test"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, crate::domain::EntryType::Unknown);
    }

    /// Integration test against the developer's local `~/.claude/projects` directory.
    /// Marked `#[ignore]` because results depend on the runner's environment (CI may have
    /// no logs at all). Run explicitly with `cargo test -- --ignored test_parse_actual_files`.
    #[test]
    #[ignore]
    fn test_parse_actual_files() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let projects_dir = format!("{}/.claude/projects", home.to_string_lossy());

        if std::path::Path::new(&projects_dir).exists() {
            let pattern = format!("{projects_dir}/*/*.jsonl");
            let files: Vec<_> = glob::glob(&pattern)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .take(5)
                .collect();

            let mut total_entries = 0;
            let mut total_errors = 0;

            for file in &files {
                match JsonlParser::parse_file(file) {
                    Ok(entries) => {
                        total_entries += entries.len();
                    }
                    Err(_) => {
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

    // ── Property tests: the `parse_entries(&[u8])` byte seam ───────────────────
    // Driven on adversarial bytes to pin JSONL robustness. Cheap enough for
    // the per-commit gate, which is where a seam regression must surface.
    // `release-verify.yml` re-runs them with a far larger `PROPTEST_CASES`.
    use proptest::prelude::*;

    // A valid Claude Code JSONL line with generated string fields — `json!` keeps
    // it well-formed so the dedup / audit-fallback path is actually exercised.
    fn valid_line() -> impl Strategy<Value = String> {
        (
            any::<String>(),
            prop_oneof![
                Just("user"),
                Just("assistant"),
                Just("summary"),
                Just("system")
            ],
            any::<String>(),
            proptest::option::of(any::<String>()),
        )
            .prop_map(|(uuid, ty, content, req)| {
                let mut v = serde_json::json!({
                    "uuid": uuid,
                    "type": ty,
                    "timestamp": "2026-01-01T00:00:00Z",
                    "message": {"role": "user", "content": content, "id": "m1"},
                });
                if let Some(r) = req {
                    v["requestId"] = serde_json::Value::String(r);
                }
                v.to_string()
            })
    }

    // A document = mix of valid lines and arbitrary garbage, newline-joined.
    fn arb_doc() -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(
            prop_oneof![
                valid_line(),
                any::<String>(),
                Just("\u{0}\u{1b}[m".to_string())
            ],
            0..8,
        )
        .prop_map(|lines| lines.join("\n").into_bytes())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        // Arbitrary bytes through the parse seam never panic.
        #[test]
        fn parse_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = JsonlParser::parse_entries(&bytes);
        }

        // A doc mixing valid + garbage lines parses without panic; the result
        // is bounded by the non-empty line count (dedup only shrinks).
        #[test]
        fn parse_entry_count_bounded_by_lines(doc in arb_doc()) {
            let entries = JsonlParser::parse_entries(&doc);
            let non_empty = String::from_utf8_lossy(&doc)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            prop_assert!(entries.len() <= non_empty);
        }

        // Every accepted entry re-serializes and re-parses (the parser never
        // admits a value it can't round-trip).
        #[test]
        fn accepted_entries_roundtrip(doc in arb_doc()) {
            let entries = JsonlParser::parse_entries(&doc);
            for e in &entries {
                let s = serde_json::to_string(e).expect("entry serializes");
                prop_assert!(
                    serde_json::from_str::<LogEntry>(&s).is_ok(),
                    "accepted entry failed to round-trip: {s}"
                );
            }
        }
    }
}
