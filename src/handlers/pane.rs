//! Conversation-pane helpers: load conversation messages off-thread, look up
//! the file path / count of the currently visible session, open/preview a
//! conversation in a pane, plus text-selection extraction shared by the mouse
//! Up handler and the Session Detail popup.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use crate::aggregator::DailyGroup;
use crate::state::{ConvListMode, ConversationPane, MAX_PANES};
use crate::{AppState, ConversationMessage, ui};

type CachedMessages = Arc<Vec<ConversationMessage>>;

/// Parsed-conversation cache keyed by (path, mtime): reopening the same
/// unchanged session skips the full JSONL re-parse. The buffer is shared
/// by `Arc`, so a cache hit is a refcount bump, not a deep copy.
/// Module-global (like `CostCalculator::global()`) so no call site has to
/// thread it through.
static CONV_CACHE: OnceLock<Mutex<Vec<(PathBuf, u64, CachedMessages)>>> = OnceLock::new();
const CONV_CACHE_CAP: usize = 8;

fn conv_cache() -> &'static Mutex<Vec<(PathBuf, u64, CachedMessages)>> {
    CONV_CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn conv_cache_get(path: &std::path::Path, mtime: u64) -> Option<CachedMessages> {
    let cache = conv_cache().lock().ok()?;
    cache
        .iter()
        .find(|(p, m, _)| p == path && *m == mtime)
        .map(|(_, _, msgs)| Arc::clone(msgs))
}

fn conv_cache_put(path: PathBuf, mtime: u64, messages: CachedMessages) {
    let Ok(mut cache) = conv_cache().lock() else {
        return;
    };
    cache.retain(|(p, _, _)| p != &path);
    cache.push((path, mtime, messages));
    if cache.len() > CONV_CACHE_CAP {
        cache.remove(0);
    }
}

pub(crate) fn spawn_load_conversation(
    file_path: &std::path::Path,
) -> mpsc::Receiver<CachedMessages> {
    let fp = file_path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    // mtime captured BEFORE the parse: a file that grows mid-parse lands in
    // the cache under the old stamp, so the next open re-parses correctly.
    let mtime = crate::infrastructure::get_file_modified_secs(&fp);
    if mtime != 0
        && let Some(hit) = conv_cache_get(&fp, mtime)
    {
        let _ = tx.send(hit);
        return rx;
    }
    std::thread::spawn(move || {
        let messages = Arc::new(ui::load_conversation(&fp).unwrap_or_default());
        if mtime != 0 {
            conv_cache_put(fp, mtime, Arc::clone(&messages));
        }
        let _ = tx.send(messages);
    });
    rx
}

/// Owned clone of the user-session currently highlighted in the Daily tab —
/// the value most "summary" / "regen" key bindings need. Returns `None` if
/// the selection points outside the visible (subagent-filtered) list.
pub(crate) fn current_selected_session(state: &AppState) -> Option<crate::aggregator::SessionInfo> {
    state
        .daily_groups
        .get(state.selected_day)
        .and_then(|g| g.user_sessions().nth(state.selected_session))
        .cloned()
}

/// Locate a session by its on-disk path, returning `(day, sess, raw_idx)` where
/// `sess` is the user-session (subagent-filtered) index and `raw_idx` is the
/// subagent-inclusive slot — the shape `start_jsonl_regen` needs to splice a
/// regenerated title back. Lets the Summary popup's resume-title write find
/// its target without relying on the current selection still pointing at it.
pub(crate) fn find_session_indices_by_path(
    state: &AppState,
    path: &std::path::Path,
) -> Option<(usize, usize, usize)> {
    for (day, group) in state.daily_groups.iter().enumerate() {
        let mut sess = 0;
        for (raw_idx, s) in group.sessions.iter().enumerate() {
            if s.is_subagent {
                continue;
            }
            if s.file_path == path {
                return Some((day, sess, raw_idx));
            }
            sess += 1;
        }
    }
    None
}

pub(crate) fn get_conv_session_file(state: &AppState, idx: usize) -> Option<std::path::PathBuf> {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .and_then(|g| g.user_sessions().nth(idx))
            .map(|s| s.file_path.clone()),
        ConvListMode::Pinned => state.pins.entries().get(idx).map(|e| e.path.clone()),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(DailyGroup::user_sessions)
            .nth(idx)
            .map(|s| s.file_path.clone()),
        ConvListMode::Live => {
            // Same active+paused order as the Live tab's list.
            if idx < state.live_active.len() {
                state.live_active[idx].jsonl_path.clone()
            } else {
                state
                    .live_paused
                    .get(idx - state.live_active.len())
                    .and_then(|s| s.jsonl_path.clone())
            }
        }
    }
}

pub(crate) fn get_conv_session_count(state: &AppState) -> usize {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .map_or(0, |g| g.user_sessions().count()),
        ConvListMode::Pinned => state.pins.entries().len(),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(DailyGroup::user_sessions)
            .count(),
        ConvListMode::Live => crate::live_visible_count(state),
    }
}

pub(crate) fn preview_conversation_in_pane(state: &mut AppState) {
    let saved = state.active_pane_index;
    open_conversation_in_pane(state);
    state.active_pane_index = saved;
}

pub(crate) fn open_conversation_in_pane(state: &mut AppState) {
    let no_loading = state.panes.iter().all(|p| !p.loading);
    if !no_loading || state.panes.len() >= MAX_PANES {
        return;
    }

    let Some(file_path) = get_conv_session_file(state, state.selected_session) else {
        return;
    };

    let target_idx = state
        .panes
        .iter()
        .position(|p| p.file_path.is_none())
        .unwrap_or_else(|| state.active_pane_index.unwrap_or(0));

    let new_pane = ConversationPane::load_from(&file_path);
    if target_idx < state.panes.len() {
        state.panes[target_idx] = new_pane;
    } else {
        state.panes.push(new_pane);
    }

    state.active_pane_index = Some(target_idx);
    state.show_conversation = true;
}

/// Persist pins, surfacing a save failure as a toast. Pins are the only
/// user-curated state ccsight writes, so a silent save failure would lose
/// them without the user knowing (see MEMORY pins-safety).
pub(crate) fn persist_pins(state: &mut AppState) {
    if let Err(e) = state.pins.save() {
        state.toast(format!("Pin save failed: {e}"));
    }
}

/// Toggle a session's pinned state, persist, and request a redraw.
pub(crate) fn toggle_pin(state: &mut AppState, path: &std::path::Path) {
    state.pins.toggle(path);
    persist_pins(state);
    state.needs_draw = true;
}

/// Resolve a Live session's JSONL path to its indexed `SessionInfo`. Looks in
/// `original_daily_groups` because Live sessions can sit outside the active
/// filter. `None` means the aggregator hasn't picked the file up yet.
pub(crate) fn find_indexed_session_by_path(
    state: &AppState,
    jsonl: &std::path::Path,
) -> Option<crate::aggregator::SessionInfo> {
    state.original_daily_groups.iter().find_map(|g| {
        g.sessions
            .iter()
            .find(|s| s.file_path == jsonl && !s.is_subagent)
            .cloned()
    })
}

/// Open the Session Detail popup for the selected Live session. Live sessions
/// can sit outside the active filter, so it looks up in `original_daily_groups`
/// and stashes via `session_detail_override`. Returns false (and toasts) when
/// the session isn't indexed yet. Shared by the `i` key and the `[i]` mouse
/// zone so both behave identically.
pub(crate) fn open_live_session_detail(state: &mut AppState) -> bool {
    let live_info = crate::live_selected_session(state).map(|live| {
        let started = live
            .started_at
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        (
            live.jsonl_path.clone(),
            live.pid,
            live.status.clone().unwrap_or_else(|| "—".to_string()),
            started,
        )
    });
    let Some((Some(jsonl), pid, status, started)) = live_info else {
        return false;
    };
    if let Some(session) = find_indexed_session_by_path(state, &jsonl) {
        state.session_detail_override = Some(session);
        state.session_detail_live_extra = Some((pid, status, started));
        state.active_popup = crate::ActivePopup::Detail;
        state.session_detail_scroll = 0;
        true
    } else {
        state.toast("Session not yet indexed; ccsight reloads every 30s");
        false
    }
}

/// Extract text from the rendered terminal buffer for a rectangular mouse
/// selection. When `conv_area` is `Some`, the selection is clamped to that
/// rect (Conversation pane / popup); when `wrap_flags` is also `Some`, the
/// extraction joins wrapped-continuation rows back into single logical lines.
pub(crate) fn extract_selected_text_from_buffer(
    sel: &(u16, u16, u16, u16),
    buffer: &ratatui::buffer::Buffer,
    conv_area: Option<ratatui::layout::Rect>,
    wrap_flags: Option<&[bool]>,
    conv_scroll: usize,
    body_indent: usize,
) -> String {
    let (sc, sr, ec, er) = *sel;
    let buf_area = buffer.area;

    let (start_col, start_row, end_col, end_row) = if (sr, sc) <= (er, ec) {
        (sc, sr, ec, er)
    } else {
        (ec, er, sc, sr)
    };

    let clamp = conv_area.filter(|ca| {
        start_row >= ca.y
            && start_row < ca.y + ca.height
            && start_col >= ca.x
            && start_col < ca.x + ca.width
    });

    let mut lines: Vec<String> = Vec::new();
    let mut line_rows: Vec<u16> = Vec::new();
    for row in start_row..=end_row {
        if row < buf_area.y || row >= buf_area.y + buf_area.height {
            continue;
        }
        if let Some(ca) = clamp
            && (row < ca.y || row >= ca.y + ca.height)
        {
            continue;
        }
        let col_start = if row == start_row {
            start_col
        } else {
            clamp.map_or(buf_area.x, |ca| ca.x)
        };
        let col_end = if row == end_row {
            end_col
        } else {
            clamp.map_or(buf_area.x + buf_area.width - 1, |ca| ca.x + ca.width - 1)
        };

        let mut line = String::new();
        let mut col = col_start;
        let mut skip_next = false;
        while col <= col_end && col < buf_area.x + buf_area.width {
            if skip_next {
                skip_next = false;
                col += 1;
                continue;
            }
            let cell = &buffer[(col, row)];
            let sym = cell.symbol();
            line.push_str(sym);
            if sym
                .chars()
                .next()
                .is_some_and(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > 1)
            {
                skip_next = true;
            }
            col += 1;
        }
        lines.push(line.trim_end().to_string());
        line_rows.push(row);
    }

    while lines.last().is_some_and(std::string::String::is_empty) {
        lines.pop();
        line_rows.pop();
    }

    // If we have wrap_flags, we are copying from the conversation view — apply the
    // conversation-specific cleanup (strip `▶ ` marker, merge wrapped-continuation rows).
    // For any other clamped selection (e.g., the Session Detail popup), preserve the
    // literal rendered layout so multi-line commands like the resume snippet keep their
    // `\<newline>` continuation intact.
    if let (Some(ca), Some(flags)) = (clamp, wrap_flags)
        && !lines.is_empty()
    {
        let continuation_flags: Vec<bool> = line_rows
            .iter()
            .map(|&row| {
                let flag_idx = conv_scroll + (row - ca.y) as usize;
                flags.get(flag_idx).copied().unwrap_or(false)
            })
            .collect();
        return join_conversation_lines(&lines, &continuation_flags, body_indent);
    }

    lines.join("\n")
}

pub(crate) fn join_conversation_lines(
    lines: &[String],
    wrap_continuation: &[bool],
    body_indent: usize,
) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let strip_prefix = |s: &str| -> String {
        // Drop the draw-layer selection marker first, then up to `body_indent`
        // of the compact body hang-indent — but only the indent itself, so the
        // content's own leading whitespace (e.g. code indentation) survives.
        let s = s
            .strip_prefix("▶ ")
            .or_else(|| s.strip_prefix("  "))
            .unwrap_or(s);
        let trim = s
            .chars()
            .take(body_indent)
            .take_while(|c| *c == ' ')
            .count();
        s[trim..].to_string()
    };

    let mut result = String::new();
    let mut i = 0;

    while i < lines.len() {
        let stripped = strip_prefix(&lines[i]);
        result.push_str(&stripped);

        if i + 1 < lines.len() {
            let next_is_continuation = wrap_continuation.get(i + 1).copied().unwrap_or(false);
            if next_is_continuation {
                let next_stripped = strip_prefix(&lines[i + 1]);
                if !next_stripped.is_empty() {
                    result.push(' ');
                } else {
                    result.push('\n');
                }
            } else {
                result.push('\n');
            }
        }

        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::NaiveDate;

    use std::sync::Arc;

    use super::{CONV_CACHE_CAP, conv_cache_get, conv_cache_put, find_session_indices_by_path};
    use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};

    #[test]
    fn should_return_subagent_filtered_and_raw_indices() {
        // Day 0: [user A, subagent, user B]. user B is user-index 1 but raw slot 2.
        let mut sub = make_session("p", None, None);
        sub.is_subagent = true;
        sub.file_path = PathBuf::from("/tmp/sub.jsonl");
        let a = {
            let mut s = make_session("p", None, Some("a"));
            s.file_path = PathBuf::from("/tmp/a.jsonl");
            s
        };
        let b = {
            let mut s = make_session("p", None, Some("b"));
            s.file_path = PathBuf::from("/tmp/b.jsonl");
            s
        };
        let day0 = make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![a, sub, b],
        );
        let c = {
            let mut s = make_session("p", None, Some("c"));
            s.file_path = PathBuf::from("/tmp/c.jsonl");
            s
        };
        let day1 = make_daily_group(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), vec![c]); // lint-ok: date-literal
        let state = make_test_app_state(vec![day0, day1]);

        assert_eq!(
            find_session_indices_by_path(&state, &PathBuf::from("/tmp/b.jsonl")),
            Some((0, 1, 2))
        );
        assert_eq!(
            find_session_indices_by_path(&state, &PathBuf::from("/tmp/c.jsonl")),
            Some((1, 0, 0))
        );
        // Subagent sessions are skipped entirely — never a valid title target.
        assert_eq!(
            find_session_indices_by_path(&state, &PathBuf::from("/tmp/sub.jsonl")),
            None
        );
        assert_eq!(
            find_session_indices_by_path(&state, &PathBuf::from("/tmp/missing.jsonl")),
            None
        );
    }

    #[test]
    fn conv_cache_hits_same_mtime_misses_on_change_and_evicts_at_cap() {
        // Unique paths so the process-global cache can't collide with other
        // tests that load conversations concurrently.
        let base = std::env::temp_dir().join(format!("ccsight-convcache-{}", std::process::id()));
        let p = |i: usize| base.join(format!("{i}.jsonl"));

        conv_cache_put(p(0), 1, Arc::new(Vec::new()));
        assert!(
            conv_cache_get(&p(0), 1).is_some(),
            "same (path, mtime) hits"
        );
        assert!(conv_cache_get(&p(0), 2).is_none(), "mtime change misses");

        for i in 1..=CONV_CACHE_CAP {
            conv_cache_put(p(i), 1, Arc::new(Vec::new()));
        }
        assert!(
            conv_cache_get(&p(0), 1).is_none(),
            "oldest entry falls out once the cap is exceeded"
        );
    }
}
