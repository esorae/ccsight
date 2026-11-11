//! Pure session-state detection from on-disk signals: read a session JSONL
//! tail, classify the turn position, and derive whether the session needs
//! human attention. Shared by the MCP `live_sessions` tool and `--wait` so
//! both surfaces classify identically. Independent of the TUI and of any
//! running Claude session — it only reads files.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::domain::{ContentBlock, EntryType, LogEntry, MessageContent, Role};

/// What the last meaningful conversation entry implies about the turn position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailKind {
    /// Assistant text with no pending tool call — the turn ended, human's move.
    AssistantEnd,
    /// Assistant emitted a `tool_use` not yet executed — awaiting run/approval.
    ToolPending,
    /// User text with no assistant reply yet — Claude owes a response.
    UserMessage,
    /// A `tool_result` was delivered — Claude owes a continuation.
    ToolResult,
    /// A hard error is the tail: a `type:system, level:error` entry (api_error,
    /// hook failure). NOT a per-tool `is_error` (those are recoverable noise).
    Error,
    /// Nothing classifiable (empty / unparsable / metadata-only tail).
    Empty,
}

/// Intervention state of a session derived purely from on-disk signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Busy, or alive-but-not-idle-long-enough, or cleanly exited — no action.
    Active,
    /// Alive and idle: Claude finished / awaits approval, left unattended.
    AwaitingInput,
    /// Alive and idle: Claude should be producing output but isn't.
    Stalled,
    /// Recent unrecovered error in the conversation.
    Error,
    /// Process gone while the last entry was mid-task.
    ExitedUnfinished,
}

/// Classify a `User` / `Assistant` entry's tail position. A failed tool result
/// (`is_error: true`) is deliberately NOT an error — Claude Code emits those
/// constantly for recoverable failures (Edit retry, non-zero Bash); true errors
/// are `type:system, level:error`, handled in `read_tail_kind`.
pub fn classify_entry(e: &LogEntry) -> TailKind {
    let Some(msg) = &e.message else {
        return TailKind::Empty;
    };
    let blocks: &[ContentBlock] = match &msg.content {
        MessageContent::Blocks(b) => b,
        _ => &[],
    };
    let has_tool_use = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let has_tool_result = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
    match msg.role {
        Role::Assistant => {
            if has_tool_use {
                TailKind::ToolPending
            } else {
                TailKind::AssistantEnd
            }
        }
        Role::User => {
            if has_tool_result {
                TailKind::ToolResult
            } else {
                TailKind::UserMessage
            }
        }
        Role::Unknown => TailKind::Empty,
    }
}

/// Read the tail of a session JSONL and classify its last meaningful entry.
/// Only the final chunk is read (sessions can be huge), and `summary` /
/// `system` / title rows are skipped so resume-title appends don't mask the
/// real turn position. The window grows until it holds a complete line so a
/// single entry larger than it (e.g. a big `tool_result`) can't blank the tail.
pub fn read_tail_kind(path: &Path) -> TailKind {
    let Some(text) = read_tail(path, 64 * 1024, |lines| !lines.is_empty()) else {
        return TailKind::Empty;
    };
    let lines: Vec<&str> = text.lines().collect();
    for line in lines.iter().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<LogEntry>(line) else {
            continue;
        };
        match entry.entry_type {
            // A `system` error (api_error, hook failure, …) is the only reliable
            // hard-error signal; other system levels (suggestion/info/warning)
            // are noise and skipped.
            EntryType::System if entry.level.as_deref() == Some("error") => {
                return TailKind::Error;
            }
            EntryType::User | EntryType::Assistant => return classify_entry(&entry),
            // Metadata / non-error system / summary / attachment rows: skip so a
            // resume-title append or a hook note can't mask the real turn tail.
            _ => continue,
        }
    }
    TailKind::Empty
}

/// The whole lines at the end of `path`: a window grown from `initial` until
/// `enough` accepts them, the whole file is read, or the cap is hit. A seek
/// past zero lands mid-line, so that partial first line is dropped.
fn read_tail(path: &Path, initial: u64, enough: impl Fn(&[&str]) -> bool) -> Option<String> {
    const MAX_TAIL: u64 = 8 * 1024 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().map_or(0, |m| m.len());
    let mut window = initial;
    loop {
        let start = len.saturating_sub(window);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).ok()?;
        let buf = String::from_utf8_lossy(&bytes).into_owned();
        let body = if start > 0 {
            buf.split_once('\n').map_or("", |(_, rest)| rest)
        } else {
            buf.as_str()
        };
        let lines: Vec<&str> = body.lines().collect();
        if enough(&lines) || start == 0 || window >= MAX_TAIL {
            return Some(body.to_string());
        }
        window = window.saturating_mul(2);
    }
}

/// The prompt cache a session's last main-chain request left behind. Once the
/// TTL lapses, the next request rewrites all of `tokens` at the write rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTail {
    pub at: chrono::DateTime<chrono::Utc>,
    /// Prefix cached after the request: what it read plus what it wrote.
    pub tokens: u64,
    /// From the newest cache write in the tail; `None` when the tail has none.
    pub ttl_secs: Option<i64>,
    pub model: Option<String>,
}

/// Subagent (sidechain) requests keep their own cache, so only the main
/// chain's last request says whether resuming finds a warm prefix.
pub fn read_cache_tail(path: &Path) -> Option<CacheTail> {
    let is_request = |l: &str| l.contains("\"assistant\"") && l.contains("\"usage\"");
    let text = read_tail(path, 512 * 1024, |lines| {
        lines.iter().any(|l| is_request(l))
    })?;
    let mut last: Option<CacheTail> = None;
    for line in text.lines().rev().filter(|l| is_request(l)) {
        let Ok(e) = serde_json::from_str::<LogEntry>(line) else {
            continue;
        };
        if e.entry_type != EntryType::Assistant || e.is_sidechain {
            continue;
        }
        let Some(msg) = e.message.as_ref() else {
            continue;
        };
        let (Some(u), Some(at)) = (msg.usage.as_ref(), e.timestamp) else {
            continue;
        };
        if msg.model.as_deref() == Some("<synthetic>") {
            continue;
        }
        let tail = last.get_or_insert_with(|| CacheTail {
            at,
            tokens: u.cache_read_input_tokens + u.cache_creation_input_tokens,
            ttl_secs: None,
            model: msg.model.clone(),
        });
        let ttl = match u.cache_creation.as_ref() {
            Some(b) if b.ephemeral_1h_input_tokens > 0 => Some(3600),
            Some(b) if b.ephemeral_5m_input_tokens > 0 => Some(300),
            // A write without the per-TTL breakdown is the API's default TTL.
            None if u.cache_creation_input_tokens > 0 => Some(300),
            _ => None,
        };
        if ttl.is_some() {
            tail.ttl_secs = ttl;
            break;
        }
    }
    last
}

/// What resuming would find: the cache still warm, or gone with the cost of
/// rewriting it (`None` when the model has no known price).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CacheState {
    Warm { secs_left: i64 },
    Cold { rebuild_usd: Option<f64> },
    Unknown,
}

pub fn cache_state(tail: &CacheTail, now: chrono::DateTime<chrono::Utc>) -> CacheState {
    let Some(ttl) = tail.ttl_secs else {
        return CacheState::Unknown;
    };
    let secs_left = ttl - (now - tail.at).num_seconds();
    if secs_left > 0 {
        return CacheState::Warm { secs_left };
    }
    let rebuild_usd = crate::aggregator::CostCalculator::global()
        .get_pricing(tail.model.as_deref())
        .map(|p| {
            let write = if ttl >= 3600 {
                p.cache_write_1h_cost_per_mtok
            } else {
                p.cache_write_5m_cost_per_mtok
            };
            tail.tokens as f64 / 1e6 * (write - p.cache_read_cost_per_mtok)
        });
    CacheState::Cold { rebuild_usd }
}

/// Decide a session's intervention state from on-disk signals. Pure: the
/// caller supplies `alive` (PID verified), `busy` (Claude's `status` field),
/// `idle_secs` (now − jsonl mtime), the `idle_threshold`, and the `tail`.
pub fn classify_state(
    alive: bool,
    busy: bool,
    idle_secs: u64,
    idle_threshold: u64,
    tail: TailKind,
) -> SessionState {
    if !alive {
        // A process that's gone only matters if it stopped mid-task; a clean
        // turn-end is a normal close, not an alert.
        return match tail {
            TailKind::ToolPending | TailKind::UserMessage | TailKind::Error => {
                SessionState::ExitedUnfinished
            }
            _ => SessionState::Active,
        };
    }
    // Busy (responding) or not-yet-idle means Claude is still working — even
    // through a `system` error it may be auto-retrying — so don't alert yet.
    if busy || idle_secs < idle_threshold {
        return SessionState::Active;
    }
    match tail {
        TailKind::AssistantEnd | TailKind::ToolPending => SessionState::AwaitingInput,
        TailKind::UserMessage | TailKind::ToolResult => SessionState::Stalled,
        // A hard error that's still the tail after the idle window = unrecovered.
        TailKind::Error => SessionState::Error,
        TailKind::Empty => SessionState::Active,
    }
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Active => "active",
            SessionState::AwaitingInput => "awaiting_input",
            SessionState::Stalled => "stalled",
            SessionState::Error => "error",
            SessionState::ExitedUnfinished => "exited",
        }
    }
}

/// Read a session's JSONL tail and classify its intervention state. The single
/// entry point shared by `--wait` and the MCP `live_sessions` tool so both
/// derive the same state from the same signals.
pub fn live_state(
    alive: bool,
    busy: bool,
    idle_secs: u64,
    idle_threshold: u64,
    jsonl_path: Option<&Path>,
) -> (SessionState, TailKind) {
    let tail = jsonl_path.map_or(TailKind::Empty, read_tail_kind);
    (
        classify_state(alive, busy, idle_secs, idle_threshold, tail),
        tail,
    )
}

/// Human-readable reason for an alert state.
pub fn reason_for(state: SessionState, tail: TailKind, idle_secs: u64) -> String {
    let mins = idle_secs / 60;
    match state {
        SessionState::AwaitingInput => match tail {
            TailKind::ToolPending => format!("tool call awaiting approval; idle {mins}m"),
            _ => format!("assistant turn finished; no input for {mins}m"),
        },
        SessionState::Stalled => format!("Claude owes output but is idle {mins}m"),
        SessionState::Error => "unrecovered error in the conversation".to_string(),
        SessionState::ExitedUnfinished => "process exited mid-task".to_string(),
        SessionState::Active => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(json: &str) -> LogEntry {
        serde_json::from_str(json).unwrap()
    }

    fn request(ts: &str, sidechain: bool, read: u64, w1h: u64, w5m: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","isSidechain":{sidechain},"message":{{"role":"assistant","model":"claude-opus-5-5","content":[],"usage":{{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":{read},"cache_creation_input_tokens":{w},"cache_creation":{{"ephemeral_1h_input_tokens":{w1h},"ephemeral_5m_input_tokens":{w5m}}}}}}}}}"#,
            w = w1h + w5m
        )
    }

    fn tail_of(lines: &[String]) -> Option<CacheTail> {
        let path = std::env::temp_dir().join(format!(
            "ccsight-cache-tail-{}-{}.jsonl",
            std::process::id(),
            lines.len()
        ));
        std::fs::write(&path, lines.join("\n")).unwrap();
        let tail = read_cache_tail(&path);
        let _ = std::fs::remove_file(&path);
        tail
    }

    #[test]
    fn the_cache_tail_is_the_last_main_request_with_the_newest_writes_ttl() {
        let tail = tail_of(&[
            request("2026-01-01T00:00:00Z", false, 0, 1000, 0),
            // A read-only request refreshes the cache without naming a TTL.
            request("2026-01-01T00:10:00Z", false, 5000, 0, 0),
            // A subagent's request keeps its own cache.
            request("2026-01-01T00:20:00Z", true, 9, 0, 9),
        ])
        .unwrap();
        assert_eq!(tail.at.to_rfc3339(), "2026-01-01T00:10:00+00:00");
        assert_eq!(tail.tokens, 5000);
        assert_eq!(tail.ttl_secs, Some(3600));

        let five = tail_of(&[request("2026-01-01T00:00:00Z", false, 10, 0, 20)]).unwrap();
        assert_eq!(five.ttl_secs, Some(300));
        let unknown = tail_of(&[request("2026-01-01T00:00:00Z", false, 10, 0, 0)]).unwrap();
        assert_eq!(unknown.ttl_secs, None);
        assert_eq!(tail_of(&[r#"{"type":"user"}"#.to_string()]), None);
    }

    #[test]
    fn cache_state_is_warm_until_the_ttl_then_prices_the_rewrite() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let tail = CacheTail {
            at,
            tokens: 1_000_000,
            ttl_secs: Some(3600),
            model: Some("claude-opus-5-5".into()),
        };
        let after = |secs| at + chrono::Duration::seconds(secs);
        assert_eq!(
            cache_state(&tail, after(600)),
            CacheState::Warm { secs_left: 3000 }
        );
        let p = crate::aggregator::CostCalculator::global()
            .get_pricing(Some("claude-opus-5-5"))
            .unwrap();
        let want = p.cache_write_1h_cost_per_mtok - p.cache_read_cost_per_mtok;
        assert_eq!(
            cache_state(&tail, after(3600)),
            CacheState::Cold {
                rebuild_usd: Some(want)
            }
        );
        let unpriced = CacheTail {
            model: Some("no-such-model".into()),
            ..tail.clone()
        };
        assert_eq!(
            cache_state(&unpriced, after(3600)),
            CacheState::Cold { rebuild_usd: None }
        );
        let no_ttl = CacheTail {
            ttl_secs: None,
            ..tail
        };
        assert_eq!(cache_state(&no_ttl, after(1)), CacheState::Unknown);
    }

    #[test]
    fn classify_entry_covers_each_turn_position() {
        let assistant_text = entry(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        );
        assert_eq!(classify_entry(&assistant_text), TailKind::AssistantEnd);

        let assistant_tool = entry(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
        );
        assert_eq!(classify_entry(&assistant_tool), TailKind::ToolPending);

        let user_result = entry(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok","is_error":false}]}}"#,
        );
        assert_eq!(classify_entry(&user_result), TailKind::ToolResult);

        let user_text = entry(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
        );
        assert_eq!(classify_entry(&user_text), TailKind::UserMessage);

        // A failed tool result is recoverable noise, NOT an error — it reads as
        // a normal tool_result (Claude owes a continuation).
        let failed_tool = entry(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"Error editing file","is_error":true}]}}"#,
        );
        assert_eq!(classify_entry(&failed_tool), TailKind::ToolResult);
    }

    #[test]
    fn read_tail_system_error_is_error_but_recoverable_failure_is_not() {
        let dir = std::env::temp_dir().join(format!("ccsight-state-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A `system` level=error tail (api_error) → Error.
        let sys_err = dir.join("a.jsonl");
        std::fs::write(
            &sys_err,
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
                "\n",
                r#"{"type":"system","level":"error","subtype":"api_error"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(read_tail_kind(&sys_err), TailKind::Error);

        // A failed Edit (is_error) followed by a recovery tool_use → the tail is
        // the recovery, never an Error.
        let recovered = dir.join("b.jsonl");
        std::fs::write(
            &recovered,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"Error editing file","is_error":true}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(read_tail_kind(&recovered), TailKind::ToolPending);

        // A non-error system tail (hook info) is skipped → picks the prior turn.
        let sys_info = dir.join("c.jsonl");
        std::fs::write(
            &sys_info,
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
                "\n",
                r#"{"type":"system","level":"info","subtype":"informational"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(read_tail_kind(&sys_info), TailKind::AssistantEnd);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_tail_classifies_a_last_entry_larger_than_the_initial_window() {
        // A final entry bigger than the initial tail window must not blank the
        // classification — an empty read would classify as Active and suppress
        // the alert. The window grows until a whole line survives.
        let dir = std::env::temp_dir().join(format!("ccsight-state-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.jsonl");
        let huge = "x".repeat(200 * 1024); // > 64KB single line
        let big_line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"{huge}"}}]}}}}"#
        );
        let content = format!(
            "{}\n{big_line}\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{}}]}}"#,
        );
        std::fs::write(&path, content).unwrap();
        // Last entry is a >64KB tool_result → ToolResult, not Empty.
        assert_eq!(read_tail_kind(&path), TailKind::ToolResult);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_state_decision_table() {
        let t = 600;
        // Busy with an error tail → Active: Claude may be auto-retrying (this
        // is the recoverable-failure false-positive the gate fixes).
        assert_eq!(
            classify_state(true, true, 0, t, TailKind::Error),
            SessionState::Active
        );
        // Alive, error tail, idle below threshold → still Active (may recover).
        assert_eq!(
            classify_state(true, false, t - 1, t, TailKind::Error),
            SessionState::Active
        );
        // Alive, error tail, idle past threshold → unrecovered Error.
        assert_eq!(
            classify_state(true, false, t, t, TailKind::Error),
            SessionState::Error
        );
        // Busy → Active even when idle counter is high (mtime lag).
        assert_eq!(
            classify_state(true, true, 9999, t, TailKind::AssistantEnd),
            SessionState::Active
        );
        // Alive, idle below threshold → Active.
        assert_eq!(
            classify_state(true, false, t - 1, t, TailKind::AssistantEnd),
            SessionState::Active
        );
        // Alive, idle past threshold, turn ended → AwaitingInput.
        assert_eq!(
            classify_state(true, false, t, t, TailKind::AssistantEnd),
            SessionState::AwaitingInput
        );
        // Alive, idle, tool awaiting approval → AwaitingInput.
        assert_eq!(
            classify_state(true, false, t + 1, t, TailKind::ToolPending),
            SessionState::AwaitingInput
        );
        // Alive, idle, Claude owes output → Stalled.
        assert_eq!(
            classify_state(true, false, t + 1, t, TailKind::UserMessage),
            SessionState::Stalled
        );
        assert_eq!(
            classify_state(true, false, t + 1, t, TailKind::ToolResult),
            SessionState::Stalled
        );
        // Dead process mid-task → ExitedUnfinished; clean end → Active.
        assert_eq!(
            classify_state(false, false, 10, t, TailKind::ToolPending),
            SessionState::ExitedUnfinished
        );
        assert_eq!(
            classify_state(false, false, 10, t, TailKind::UserMessage),
            SessionState::ExitedUnfinished
        );
        assert_eq!(
            classify_state(false, false, 10, t, TailKind::AssistantEnd),
            SessionState::Active
        );
    }

    #[test]
    fn read_tail_kind_skips_summary_and_picks_last_meaningful() {
        let dir = std::env::temp_dir().join(format!("ccsight-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        // Last meaningful entry is the assistant text; the trailing summary
        // (resume-title append) must be ignored.
        let contents = concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"all done"}]}}"#,
            "\n",
            r#"{"type":"summary","summary":"My Session Title","leafUuid":"x"}"#,
            "\n",
        );
        std::fs::write(&path, contents).unwrap();
        assert_eq!(read_tail_kind(&path), TailKind::AssistantEnd);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_tail_kind_missing_file_is_empty() {
        assert_eq!(
            read_tail_kind(Path::new("/nonexistent/ccsight/state/x.jsonl")),
            TailKind::Empty
        );
    }

    #[test]
    fn session_state_as_str_round_trip() {
        assert_eq!(SessionState::Active.as_str(), "active");
        assert_eq!(SessionState::AwaitingInput.as_str(), "awaiting_input");
        assert_eq!(SessionState::ExitedUnfinished.as_str(), "exited");
    }

    #[test]
    fn live_state_reads_tail_and_classifies() {
        let dir = std::env::temp_dir().join(format!("ccsight-state-ls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        // Alive, not busy, idle past threshold, assistant turn ended → awaiting.
        let (state, tail) = live_state(true, false, 700, 600, Some(&path));
        assert_eq!(state, SessionState::AwaitingInput);
        assert_eq!(tail, TailKind::AssistantEnd);
        // No path → Empty tail → Active.
        let (state2, tail2) = live_state(true, false, 700, 600, None);
        assert_eq!(tail2, TailKind::Empty);
        assert_eq!(state2, SessionState::Active);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Soak / leak guard (off per-commit gate) ────────────────────────────
    // `read_tail_kind` is the file-handle-heavy IO shared by the MCP
    // `live_sessions` tool and `--wait` polling; a leaked handle would grow
    // the fd count linearly across thousands of polls. fd is read from
    // /proc/self/fd (Linux CI) or /dev/fd (macOS dev).
    fn fd_count() -> Option<usize> {
        for dir in ["/proc/self/fd", "/dev/fd"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                return Some(rd.count());
            }
        }
        None
    }

    #[test]
    #[ignore = "soak: off the per-commit gate (cargo test -- --ignored)"]
    fn tail_read_does_not_leak_fds() {
        let dir = std::env::temp_dir().join(format!("ccsight-soak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
            ),
        )
        .unwrap();

        // Warm up so lazy first-touch opens settle before the baseline.
        for _ in 0..50 {
            let _ = read_tail_kind(&path);
        }
        let fd_start = fd_count();
        for _ in 0..3000 {
            let _ = read_tail_kind(&path);
        }
        if let (Some(a), Some(b)) = (fd_start, fd_count()) {
            assert!(
                b <= a + 8,
                "open fds grew {a} -> {b} across the soak — handle leak in the tail read"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Property test: state classification ────────────────────────────────
    use proptest::prelude::*;

    fn arb_tail() -> impl Strategy<Value = TailKind> {
        prop_oneof![
            Just(TailKind::AssistantEnd),
            Just(TailKind::ToolPending),
            Just(TailKind::UserMessage),
            Just(TailKind::ToolResult),
            Just(TailKind::Error),
            Just(TailKind::Empty),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // classify_state totality + busy/fresh→Active invariant.
        #[test]
        fn classify_state_invariants(
            alive in any::<bool>(),
            busy in any::<bool>(),
            idle in any::<u64>(),
            thr in any::<u64>(),
            tail in arb_tail(),
        ) {
            let s = classify_state(alive, busy, idle, thr, tail);
            if alive && (busy || idle < thr) {
                prop_assert_eq!(s, SessionState::Active);
            }
        }
    }
}
