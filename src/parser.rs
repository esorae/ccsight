use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
// Bounds the lines held at once to a batch, not the whole file. Big enough
// that a transcript of multi-MB lines still fills every core.
const BATCH_BYTES: usize = 64 * 1024 * 1024;
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
    /// Bytes of lines deserialized in parallel at once; never changes the result.
    batch_bytes: usize,
}

impl Limits {
    fn from_env() -> Self {
        Self {
            line_size: clamp_line_size(std::env::var("CCSIGHT_MAX_LINE_SIZE").ok().as_deref()),
            entries: clamp_entries(std::env::var("CCSIGHT_MAX_ENTRIES").ok().as_deref()),
            batch_bytes: BATCH_BYTES,
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

    /// `parse_file`, adding every byte read off disk to `read` as it goes, so
    /// a caller on another thread can show how far a long parse has got.
    pub fn parse_file_tracked(path: &Path, read: &AtomicU64) -> Result<Vec<LogEntry>> {
        Self::parse_file_counted(path, Limits::from_env(), Some(read))
    }

    fn parse_file_with(path: &Path, limits: Limits) -> Result<Vec<LogEntry>> {
        Self::parse_file_counted(path, limits, None)
    }

    fn parse_file_counted(
        path: &Path,
        limits: Limits,
        read: Option<&AtomicU64>,
    ) -> Result<Vec<LogEntry>> {
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

        let reader = BufReader::new(CountingReader { inner: file, read });
        let (entries, truncated) = Self::parse_lines_reporting(reader, limits)?;
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
        // An in-memory reader fails only on invalid UTF-8, which the walk skips.
        Self::parse_lines_reporting(reader, limits)
            .map(|(entries, _)| entries)
            .unwrap_or_default()
    }

    /// `true` = the entry cap stopped the walk, so the file's tail is missing.
    /// Any read error but invalid UTF-8 fails the walk, as a partial result
    /// would be cached as the whole file. Batches parse in parallel and fold
    /// in file order, so dedup and the cap see what a serial walk would.
    /// Private: the public byte seam stays `-> Vec<LogEntry>`.
    fn parse_lines_reporting<R: BufRead>(
        reader: R,
        limits: Limits,
    ) -> std::io::Result<(Vec<LogEntry>, bool)> {
        let mut fold = Fold {
            entries: Vec::new(),
            hash_to_index: HashMap::new(),
            limit: limits.entries,
            truncated: false,
        };
        let mut batch: Vec<String> = Vec::new();
        let mut batch_bytes = 0;
        for line_result in reader.lines() {
            let line = match line_result {
                Ok(line) => line,
                // Invalid UTF-8 has consumed its line; any other error may
                // repeat on every call and never let the walk end.
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(e) => return Err(e),
            };
            batch_bytes += line.len();
            batch.push(line);
            if batch_bytes >= limits.batch_bytes {
                if !fold.extend(&mut batch, limits.line_size) {
                    return Ok((fold.entries, true));
                }
                batch_bytes = 0;
            }
        }
        fold.extend(&mut batch, limits.line_size);

        Ok((fold.entries, fold.truncated))
    }

    fn parse_line(line: &str, line_size_limit: usize) -> Option<LogEntry> {
        if line.len() > line_size_limit || line.trim().is_empty() {
            return None;
        }
        let mut entry = serde_json::from_str::<LogEntry>(line).ok()?;

        // Cowork `audit.jsonl` puts the wall-clock time in `_audit_timestamp`
        // and leaves the standard `timestamp` field null. Fill the gap so
        // downstream date-grouping / hourly aggregation works without
        // every consumer needing to know about the alternate field.
        if entry.timestamp.is_none()
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(ts_str) = value.get("_audit_timestamp").and_then(|v| v.as_str())
            && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str)
        {
            entry.timestamp = Some(ts.with_timezone(&chrono::Utc));
        }
        Some(entry)
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

/// Adds each read's byte count to `read`; a pass-through when `None`.
struct CountingReader<'a, R> {
    inner: R,
    read: Option<&'a AtomicU64>,
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(read) = self.read {
            read.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

/// The in-order half of a parse: dedup by message + request id, and the
/// entry cap. `push` takes one slot per input line, parsed or not, and
/// returns `false` once the cap has stopped the walk.
struct Fold {
    entries: Vec<LogEntry>,
    hash_to_index: HashMap<String, usize>,
    limit: usize,
    truncated: bool,
}

impl Fold {
    /// Parses `batch` in parallel and folds it in order, leaving it empty.
    fn extend(&mut self, batch: &mut Vec<String>, line_size_limit: usize) -> bool {
        use rayon::prelude::*;
        let parsed: Vec<Option<LogEntry>> = batch
            .par_iter()
            .map(|line| JsonlParser::parse_line(line, line_size_limit))
            .collect();
        batch.clear();
        parsed.into_iter().all(|entry| self.push(entry))
    }

    fn push(&mut self, entry: Option<LogEntry>) -> bool {
        if self.entries.len() >= self.limit {
            self.truncated = true;
            return false;
        }
        let Some(entry) = entry else {
            return true;
        };
        if let Some(hash) = JsonlParser::create_dedup_hash(&entry) {
            if let Some(&existing_idx) = self.hash_to_index.get(&hash) {
                self.entries[existing_idx] = entry;
            } else {
                self.hash_to_index.insert(hash, self.entries.len());
                self.entries.push(entry);
            }
        } else {
            self.entries.push(entry);
        }
        true
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
            batch_bytes: BATCH_BYTES,
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
        // A fallback's per-attempt record: each attempt bills, so an entry
        // dropped over its shape takes every attempt's tokens with it.
        (
            "usage_iterations",
            b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[],\"usage\":{\"output_tokens\":1,\"iterations\":[{\"type\":\"message\",\"model\":\"m\",\"output_tokens\":2,\"extra\":{}},{\"type\":\"fallback_message\",\"output_tokens\":1}]}}}\n",
            1,
        ),
        // Whatever shape `iterations` takes, its entry survives with its
        // top-level tokens; only the malformed attempts are lost.
        (
            "usage_iterations_null",
            b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[],\"usage\":{\"output_tokens\":1,\"iterations\":null}}}\n",
            1,
        ),
        (
            "usage_iterations_mistyped",
            b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[],\"usage\":{\"output_tokens\":1,\"iterations\":[7,\"x\",[1],{\"model\":3,\"output_tokens\":null,\"cache_creation\":[1]}]}}}\n",
            1,
        ),
        (
            "usage_iterations_object",
            b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[],\"usage\":{\"output_tokens\":1,\"iterations\":{\"a\":[{}]}}}}\n",
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
            batch_bytes: BATCH_BYTES,
        };
        let (entries, truncated) =
            JsonlParser::parse_lines_reporting(BufReader::new(body.as_bytes()), limits).unwrap();
        assert_eq!(entries.len(), 2, "the cap must stop the walk, not the file");
        // The dropped entries are the NEWEST ones, so a silent cap understates
        // both the totals and the last-activity time.
        assert!(truncated, "hitting the cap has to be reportable");

        let under = Limits {
            line_size: DEFAULT_MAX_LINE_SIZE,
            entries: 99,
            batch_bytes: BATCH_BYTES,
        };
        let (all, hit) =
            JsonlParser::parse_lines_reporting(BufReader::new(body.as_bytes()), under).unwrap();
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
            batch_bytes: BATCH_BYTES,
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
    /// Any JSON value, its object keys drawn partly from the usage field
    /// names so a mistyped known field is generated, not only unknown ones.
    fn arb_json() -> impl Strategy<Value = serde_json::Value> {
        use serde_json::Value;
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            any::<f64>().prop_map(Value::from),
            ".{0,8}".prop_map(Value::String),
        ];
        let key = prop_oneof![
            Just("model".to_string()),
            Just("output_tokens".to_string()),
            Just("input_tokens".to_string()),
            Just("cache_creation".to_string()),
            Just("ephemeral_1h_input_tokens".to_string()),
            "[a-z_]{1,8}",
        ];
        leaf.prop_recursive(4, 48, 6, move |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                proptest::collection::btree_map(key.clone(), inner, 0..6)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

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

    // Lines drawn from a few message / request ids so retries collide and
    // dedup replaces a kept entry, mixed with garbage and blank lines.
    fn arb_dedup_doc() -> impl Strategy<Value = String> {
        let line = (0u8..3, 0u8..2, any::<u16>()).prop_map(|(m, r, n)| {
            serde_json::json!({
                "type": "assistant",
                "uuid": format!("u{n}"),
                "requestId": format!("r{r}"),
                "message": {"role": "assistant", "content": "x", "id": format!("m{m}")},
            })
            .to_string()
        });
        proptest::collection::vec(
            prop_oneof![
                4 => line,
                1 => Just("{\"type\":\"user\"}".to_string()),
                1 => Just("not json".to_string()),
                1 => Just(String::new()),
            ],
            0..16,
        )
        .prop_map(|lines| lines.join("\n"))
    }

    // A read error past the first line (a failing disk or mount) fails the
    // walk: returning the lines before it would be cached as the whole file.
    #[test]
    fn a_read_error_fails_the_parse_instead_of_returning_part_of_it() {
        struct FailsAfter(&'static [u8]);
        impl std::io::Read for FailsAfter {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Err(std::io::Error::other("device error"));
                }
                let n = buf.len().min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let limits = Limits {
            line_size: DEFAULT_MAX_LINE_SIZE,
            entries: DEFAULT_MAX_ENTRIES,
            batch_bytes: BATCH_BYTES,
        };
        let good = b"{\"type\":\"user\"}\n";
        let result = JsonlParser::parse_lines_reporting(BufReader::new(FailsAfter(good)), limits);
        assert!(result.is_err(), "a partial walk must not pass as complete");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        // Batching is invisible: one line per batch and the whole doc in one
        // batch agree on every entry, on dedup, and on where the cap stops.
        #[test]
        fn the_batch_size_never_changes_the_parse(doc in arb_dedup_doc(), cap in 1usize..10) {
            let run = |batch_bytes| {
                let limits = Limits { line_size: DEFAULT_MAX_LINE_SIZE, entries: cap, batch_bytes };
                let (entries, truncated) =
                    JsonlParser::parse_lines_reporting(BufReader::new(doc.as_bytes()), limits)
                        .unwrap();
                let json: Vec<String> =
                    entries.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
                (json, truncated)
            };
            prop_assert_eq!(run(1), run(usize::MAX));
        }

        // Whatever JSON `usage.iterations` holds, its entry survives with the
        // top-level counts it carries.
        #[test]
        fn a_malformed_iterations_never_costs_its_entry_or_tokens(
            iterations in arb_json(), output in 0u64..1_000_000,
        ) {
            let line = serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": [],
                    "usage": {"output_tokens": output, "iterations": iterations}},
            });
            let entries = JsonlParser::parse_entries(line.to_string().as_bytes());
            prop_assert_eq!(entries.len(), 1);
            let usage = entries[0].message.as_ref().and_then(|m| m.usage.as_ref());
            prop_assert_eq!(usage.map(|u| u.output_tokens), Some(output));
        }

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
